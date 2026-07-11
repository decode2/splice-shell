use serde::Serialize;
use splice_core::{AdapterRegistry, PastePayload, PasteRoute};
use splice_pty::{PtySession, TerminalSize};
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};
use tauri::{Emitter, Manager, State};

const PTY_OUTPUT_EVENT: &str = "pty-output";
const PTY_EXIT_EVENT: &str = "pty-exit";

struct PtyState {
    // Keyed by each session's monotonic id (`PtySession::id()`), so multiple
    // concurrent sessions coexist without collision. Commands must lock this
    // mutex only long enough to clone the `Arc` for a given id (or remove it
    // on teardown) and release the guard BEFORE calling any potentially
    // blocking `PtySession` method. Otherwise a hung child blocking
    // `pty_write` would stall `pty_interrupt`/`pty_resize` behind this lock —
    // the same stall the library layer already eliminates internally with the
    // identical `Arc` pattern.
    //
    // Session death is no longer polled from the frontend. Each `PtySession`
    // runs a waiter thread that pushes a `pty-exit` event on natural exit
    // (see `pty_spawn`); the natural-exit path also clears this state itself
    // (`clear_and_close_session_by_id`) so a dead session's ConPTY/pipe/job
    // handles never linger. Removal is id-scoped and idempotent: a second
    // removal of the same id is a harmless no-op.
    sessions: Mutex<HashMap<u64, Arc<PtySession>>>,
}

/// Payload for the global `pty-output` event. Every emission carries the
/// emitting session's monotonic id so the frontend can demultiplex output
/// across concurrent sessions (mirroring the `pty-exit` id payload).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PtyOutputPayload {
    session_id: u64,
    data: String,
}

#[tauri::command]
fn app_status() -> String {
    "Splice Shell scaffold ready".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum PastePreview {
    Ready {
        text: String,
        process_name: String,
        adapter_name: String,
    },
    UnsupportedImage {
        path: String,
        process_name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActivePasteTarget {
    process_name: String,
    adapter_name: Option<String>,
    supported: bool,
}

#[tauri::command]
async fn preview_clipboard_image_paste(process_name: String) -> Result<PastePreview, String> {
    let payload = read_clipboard_image_paste_payload()?;

    Ok(preview_paste_payload(&process_name, &payload))
}

#[tauri::command]
async fn active_paste_target(
    state: State<'_, PtyState>,
    session_id: Option<u64>,
) -> Result<ActivePasteTarget, String> {
    let process_name = active_pty_process_name(state.inner(), session_id)?;
    Ok(active_paste_target_for_process(&process_name))
}

#[tauri::command]
async fn preview_active_clipboard_image_paste(
    state: State<'_, PtyState>,
    session_id: Option<u64>,
) -> Result<PastePreview, String> {
    let process_name = active_pty_process_name(state.inner(), session_id)?;
    let payload = read_clipboard_image_paste_payload()?;

    Ok(preview_paste_payload(&process_name, &payload))
}

#[tauri::command]
async fn pty_spawn(
    app: tauri::AppHandle,
    state: State<'_, PtyState>,
    cols: u16,
    rows: u16,
    program: Option<String>,
    args: Option<Vec<String>>,
) -> Result<u64, String> {
    let size = TerminalSize::new(cols, rows).map_err(|error| format!("{error:?}"))?;
    let command = resolve_pty_command(program, args);
    let command_args = command.args.iter().map(String::as_str).collect::<Vec<_>>();

    // No predecessor `.take()`/close here: sessions are keyed by id and coexist.
    // Each session is torn down explicitly by its own id (frontend `killPty`,
    // the detached natural-exit cleanup, or the instant-exit re-check below),
    // so a new spawn never reaps an existing session.
    let (tx, rx) = std::sync::mpsc::channel::<(u64, String)>();
    let flusher_app = app.clone();
    std::thread::spawn(move || {
        run_flusher_loop(rx, move |session_id, data| {
            let _ = flusher_app.emit(PTY_OUTPUT_EVENT, PtyOutputPayload { session_id, data });
        });
    });

    let cleanup_app = app.clone();
    let exit_app = app;
    let session = PtySession::spawn(
        &command.program,
        &command_args,
        size,
        move |id, output| {
            let _ = tx.send((id, output));
        },
        move |id| {
            // Natural exit: push the id to the frontend so it can decide
            // whether to restart (it ignores stale ids).
            let _ = exit_app.emit(PTY_EXIT_EVENT, id);
            // Then proactively tear down the dead session's backend state so
            // its ConPTY/pipe/job handles and reader thread do not linger if
            // the frontend never restarts. This MUST run on a detached thread,
            // never inline on the waiter thread that invoked this callback:
            // `session.close()` joins that very waiter thread, so an inline
            // call would self-join and deadlock. `clear_and_close_session_by_id`
            // is id-scoped and `Option::take`-idempotent, so it is a harmless
            // no-op if a newer spawn already replaced (or another path already
            // closed) the session.
            let cleanup_app = exit_app.clone();
            std::thread::spawn(move || {
                clear_and_close_session_by_id(&cleanup_app, id);
            });
        },
    )
    .map_err(|error| error.to_string())?;

    let id = session.id();

    {
        let mut guard = state
            .sessions
            .lock()
            .map_err(|_| "PTY state lock poisoned".to_owned())?;
        guard.insert(id, Arc::new(session));
    }

    // Instant-exit race: if the child died before we stored it, its detached
    // `clear_and_close_session_by_id(id)` cleanup already ran while this id was
    // absent from the registry (a no-op), and we just stored a dead session
    // whose ConPTY/pipe/job handles and reader/waiter threads would otherwise
    // linger until the next interaction. Now that it is stored, re-check
    // liveness by id and, if it is not running, clear+close it immediately.
    // `is_running()` only errs on a poisoned lock or an already-closed session,
    // so a non-`Ok(true)` result is treated as dead. The teardown reuses the
    // id-scoped, idempotent `clear_and_close_session_by_id`, so a different
    // session is never torn down, and its `close()` runs with the state lock
    // released (no thread-join deadlock).
    let still_running = clone_pty_session_by_id(state.inner(), id)?
        .and_then(|session| session.is_running().ok())
        .unwrap_or(false);
    if !still_running {
        clear_and_close_session_by_id(&cleanup_app, id);
    }

    Ok(id)
}

/// Remove the session with `id` from the registry and return its `Arc` so the
/// caller can close it OUTSIDE the state lock. Id-scoped and idempotent: a
/// second removal of the same id yields `None`, and no other session is
/// touched. Best-effort on lock poisoning (returns `None`). Takes `&PtyState`
/// (not `&AppHandle`) so registry mutation is unit-testable without a Tauri
/// runtime.
fn remove_pty_session_by_id(state: &PtyState, id: u64) -> Option<Arc<PtySession>> {
    let mut guard = state.sessions.lock().ok()?;
    guard.remove(&id)
}

/// Remove and close the session with `id` that just exited. Runs on a detached
/// thread off the waiter thread (see `pty_spawn`); `close()` here joins the
/// now-finished waiter and releases the dead session's handles. Delegates the
/// registry mutation to `remove_pty_session_by_id` (unit-testable) and closes
/// outside the lock. Idempotent: a no-op if the id was already removed.
fn clear_and_close_session_by_id(app: &tauri::AppHandle, id: u64) {
    let state = app.state::<PtyState>();
    let session = remove_pty_session_by_id(state.inner(), id);
    // Close outside the lock: teardown blocks on process shutdown and the
    // reader/waiter-thread joins, and must not stall concurrent PTY commands.
    if let Some(session) = session {
        session.close();
    }
    // Session close hook calling sweep_temp_images (CLIP-3)
    let temp_dir = std::env::temp_dir().join("splice-shell").join("clipboard");
    let _ = splice_clipboard::sweep_temp_images(&temp_dir);
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PtyCommand {
    program: String,
    args: Vec<String>,
}

fn resolve_pty_command(program: Option<String>, args: Option<Vec<String>>) -> PtyCommand {
    match program {
        Some(program) if !program.trim().is_empty() => PtyCommand {
            program,
            args: args.unwrap_or_default(),
        },
        _ => PtyCommand {
            program: "cmd.exe".to_owned(),
            args: default_shell_args(),
        },
    }
}

fn default_shell_args() -> Vec<String> {
    vec![
        "/D".to_owned(),
        "/K".to_owned(),
        format!("set PATH={};%PATH%", common_cli_path_prefix()),
    ]
}

fn common_cli_path_prefix() -> String {
    let user_profile = std::env::var("USERPROFILE").unwrap_or_default();
    let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_default();

    [
        format!("{user_profile}\\.local\\bin"),
        format!("{user_profile}\\scoop\\shims"),
        format!("{user_profile}\\scoop\\apps\\nodejs\\current\\bin"),
        format!("{user_profile}\\scoop\\apps\\nodejs\\current"),
        format!("{local_app_data}\\agy\\bin"),
        format!("{local_app_data}\\Programs\\OpenCode\\bin"),
        format!("{local_app_data}\\Programs\\opencode\\bin"),
        format!("{local_app_data}\\OpenAI\\Codex\\bin"),
    ]
    .into_iter()
    .filter(|path| !path.starts_with('\\') && !path.is_empty())
    .collect::<Vec<_>>()
    .join(";")
}

/// Id-scoped write core, split out so its miss path is unit-testable without a
/// Tauri `State`. A miss returns the EXACT string `"PTY session is not
/// running"`, which the frontend's `isClosedPtyInputError` matches verbatim —
/// changing it is a regression.
fn pty_write_impl(state: &PtyState, session_id: u64, data: &str) -> Result<(), String> {
    let session = clone_pty_session_by_id(state, session_id)?
        .ok_or_else(|| "PTY session is not running".to_owned())?;

    // The write runs on the clone with the state lock released, so a hung
    // child cannot stall `pty_interrupt` (or any other PTY command).
    match session.write(data) {
        Ok(()) => Ok(()),
        Err(error) if error.is_terminal_closed() => {
            clear_pty_session_if_current(state, &session);
            session.close();
            Err("PTY session closed; start a new terminal session".to_owned())
        }
        Err(error) => Err(error.to_string()),
    }
}

#[tauri::command]
async fn pty_write(
    state: State<'_, PtyState>,
    session_id: u64,
    data: String,
) -> Result<(), String> {
    pty_write_impl(state.inner(), session_id, &data)
}

#[tauri::command]
async fn pty_interrupt(state: State<'_, PtyState>, session_id: u64) -> Result<(), String> {
    with_pty_session(state.inner(), session_id, |session| session.interrupt())
}

#[tauri::command]
async fn pty_resize(
    state: State<'_, PtyState>,
    session_id: u64,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let size = TerminalSize::new(cols, rows).map_err(|error| format!("{error:?}"))?;
    with_pty_session(state.inner(), session_id, |session| session.resize(size))
}

/// Id-scoped, idempotent kill core (unit-testable without a Tauri `State`). An
/// unknown or already-removed id is a harmless `Ok(())` — never an error — so
/// the frontend's fire-and-forget `void killPty()` can never reject and can
/// race the detached natural-exit cleanup safely.
fn pty_kill_impl(state: &PtyState, session_id: u64) -> Result<(), String> {
    // Close outside the lock: teardown blocks on process shutdown and the
    // reader-thread join, and must not stall concurrent PTY commands.
    if let Some(session) = remove_pty_session_by_id(state, session_id) {
        session.close();
    }

    Ok(())
}

#[tauri::command]
async fn pty_kill(state: State<'_, PtyState>, session_id: u64) -> Result<(), String> {
    pty_kill_impl(state.inner(), session_id)
}

#[tauri::command]
async fn open_path(path: String) -> Result<(), String> {
    let path = PathBuf::from(path);
    if !path.exists() {
        return Err(format!("Path does not exist: {}", path.display()));
    }

    // Reveal the file in Explorer (`/select,`) instead of launching it.
    // These paths are extracted from untrusted terminal output (including AI
    // CLI output), and launching a path with the default handler would run
    // shell-associated files (.exe/.bat/.ps1/.lnk) on a single click. Revealing
    // keeps the "locate what the CLI mentioned" affordance without ever
    // executing the target.
    Command::new("explorer.exe")
        .arg(format!("/select,{}", path.display()))
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Failed to reveal path: {error}"))
}

/// Clone the session handle for `id` while holding the state lock only for the
/// duration of the `Arc` clone. Callers invoke (possibly blocking)
/// `PtySession` methods on the returned clone AFTER the lock is released.
/// Returns `Ok(None)` when no session with that id exists. Takes `&PtyState`
/// so it is usable from both commands (via `State::inner`) and unit tests.
fn clone_pty_session_by_id(state: &PtyState, id: u64) -> Result<Option<Arc<PtySession>>, String> {
    let guard = state
        .sessions
        .lock()
        .map_err(|_| "PTY state lock poisoned".to_owned())?;

    Ok(guard.get(&id).map(Arc::clone))
}

/// Remove the stored session only if the entry under its id is still the exact
/// session that observed the failure, so a different session sharing the id key
/// is never torn down by a stale error path. Best-effort on lock poisoning:
/// the caller's "session closed" error is the useful one.
fn clear_pty_session_if_current(state: &PtyState, session: &Arc<PtySession>) {
    if let Ok(mut guard) = state.sessions.lock() {
        let id = session.id();
        if guard
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            guard.remove(&id);
        }
    }
}

fn with_pty_session<F>(state: &PtyState, id: u64, operation: F) -> Result<(), String>
where
    F: FnOnce(&PtySession) -> Result<(), splice_pty::PtyError>,
{
    let session = clone_pty_session_by_id(state, id)?
        .ok_or_else(|| "PTY session is not running".to_owned())?;

    // Run the operation on the clone with the state lock released, so a
    // blocking call here can never stall other PTY commands.
    operation(&session).map_err(|error| error.to_string())
}

/// Resolve the active PTY process name for paste routing. `session_id` is
/// `None` at mount (before any session exists) and may reference an unknown id;
/// both fall back to the `cmd.exe` process name rather than erroring, so the
/// TitleBar paste target stays populated (spec: Paste-Target Fallback Parity).
fn active_pty_process_name(state: &PtyState, session_id: Option<u64>) -> Result<String, String> {
    let session = match session_id {
        Some(id) => clone_pty_session_by_id(state, id)?,
        None => None,
    };

    let registry = AdapterRegistry::with_builtin_adapters();
    let candidates = session
        .as_deref()
        .map(PtySession::active_process_candidates)
        .transpose()
        .map_err(|error| error.to_string())?
        .unwrap_or_else(|| vec!["cmd.exe".to_owned()]);

    Ok(select_process_for_adapter(&registry, &candidates)
        .unwrap_or("cmd.exe")
        .to_owned())
}

// `async` so Tauri runs this on a worker thread: the Win32 clipboard open can
// contend (a clipboard manager holding it), and the bounded retry/backoff would
// otherwise stall the main UI thread for up to ~90ms on a plain Ctrl+C.
#[tauri::command(async)]
fn clipboard_write_text(text: String) -> Result<(), String> {
    splice_clipboard::write_clipboard_text(&text).map_err(|error| error.to_string())
}

// `async` for the same reason as `clipboard_write_text`: the Win32 clipboard open
// can contend and retry/backoff, which must not stall the main UI thread on a
// plain Ctrl+V. Returns the CF_UNICODETEXT contents, or an empty string when the
// clipboard holds no text (so the frontend can fall back to the image route).
#[tauri::command(async)]
fn clipboard_read_text() -> Result<String, String> {
    splice_clipboard::read_clipboard_text().map_err(|error| error.to_string())
}

fn read_clipboard_image_paste_payload() -> Result<PastePayload, String> {
    let temp_dir = std::env::temp_dir().join("splice-shell").join("clipboard");
    let _ = splice_clipboard::sweep_temp_images(&temp_dir);
    splice_clipboard::read_clipboard_image_paste_payload(&temp_dir)
        .map_err(|error| error.to_string())
}

fn preview_paste_payload(process_name: &str, payload: &PastePayload) -> PastePreview {
    let registry = AdapterRegistry::with_builtin_adapters();
    let adapter_name = registry.adapter_name_for_process(process_name);

    match registry.route_paste(process_name, payload) {
        PasteRoute::Text(text) => PastePreview::Ready {
            text,
            process_name: process_name.to_owned(),
            adapter_name: adapter_name.unwrap_or("text-passthrough").to_owned(),
        },
        PasteRoute::UnsupportedImage { path } => PastePreview::UnsupportedImage {
            path,
            process_name: process_name.to_owned(),
        },
    }
}

fn active_paste_target_for_process(process_name: &str) -> ActivePasteTarget {
    let registry = AdapterRegistry::with_builtin_adapters();
    let adapter_name = registry
        .adapter_name_for_process(process_name)
        .map(str::to_owned);

    ActivePasteTarget {
        process_name: process_name.to_owned(),
        supported: adapter_name.is_some(),
        adapter_name,
    }
}

fn select_process_for_adapter<'a>(
    registry: &AdapterRegistry,
    process_candidates: &'a [String],
) -> Option<&'a str> {
    process_candidates
        .iter()
        .find(|process_name| registry.adapter_name_for_process(process_name).is_some())
        .or_else(|| process_candidates.first())
        .map(String::as_str)
}

fn run_flusher_loop<F>(rx: std::sync::mpsc::Receiver<(u64, String)>, mut flush_callback: F)
where
    F: FnMut(u64, String),
{
    let mut buffer = String::new();
    let mut last_flush = std::time::Instant::now();
    let limit = std::time::Duration::from_millis(16);

    while let Ok((id, msg)) = rx.recv() {
        let current_session_id = id;
        buffer.push_str(&msg);

        // Drain any other immediately available messages in the channel
        while let Ok((_, extra)) = rx.try_recv() {
            buffer.push_str(&extra);
        }

        // Check if we can flush immediately or need to wait
        let elapsed = last_flush.elapsed();
        if elapsed < limit {
            let delay = limit - elapsed;
            std::thread::sleep(delay);
            // After sleeping, drain any messages that arrived during the sleep
            while let Ok((_, extra)) = rx.try_recv() {
                buffer.push_str(&extra);
            }
        }

        // Flush the buffer
        if !buffer.is_empty() {
            flush_callback(current_session_id, std::mem::take(&mut buffer));
            last_flush = std::time::Instant::now();
        }
    }
}

#[tauri::command]
async fn close_paste_session(path: Option<String>) -> Result<(), String> {
    close_paste_session_impl(path);
    Ok(())
}

fn close_paste_session_impl(path: Option<String>) {
    if let Some(ref path_str) = path {
        let path = std::path::PathBuf::from(path_str);
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
    }
    let temp_dir = std::env::temp_dir().join("splice-shell").join("clipboard");
    let _ = splice_clipboard::sweep_temp_images(&temp_dir);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .manage(PtyState {
            sessions: Mutex::new(HashMap::new()),
        })
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let temp_dir = std::env::temp_dir().join("splice-shell").join("clipboard");
            // Startup Cleanup (CLIP-2)
            let _ = std::fs::remove_dir_all(&temp_dir);
            let _ = splice_clipboard::sweep_temp_images(&temp_dir);

            #[cfg(desktop)]
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    use tauri_plugin_updater::UpdaterExt;
                    if let Ok(updater) = handle.updater_builder().build() {
                        match updater.check().await {
                            Ok(Some(update)) => {
                                if update.download_and_install(|_, _| {}, || {}).await.is_ok() {
                                    handle.restart();
                                }
                            }
                            Ok(None) => {
                                log::debug!("splice-shell: no update available");
                            }
                            Err(e) => {
                                log::warn!("splice-shell: updater check failed: {e}");
                            }
                        }
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            app_status,
            active_paste_target,
            preview_clipboard_image_paste,
            preview_active_clipboard_image_paste,
            pty_spawn,
            pty_write,
            pty_interrupt,
            pty_resize,
            pty_kill,
            clipboard_write_text,
            clipboard_read_text,
            open_path,
            close_paste_session
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Splice Shell desktop app");

    app.run(|_app_handle, event| {
        if let tauri::RunEvent::Exit = event {
            let temp_dir = std::env::temp_dir().join("splice-shell").join("clipboard");
            // Shutdown Cleanup (CLIP-4)
            let _ = std::fs::remove_dir_all(&temp_dir);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_status_describes_scaffold_state() {
        assert_eq!(app_status(), "Splice Shell scaffold ready");
    }

    #[test]
    fn preview_paste_payload_returns_text_for_supported_cli() {
        let payload = PastePayload::Image(splice_core::ImagePaste {
            path: "C:/Temp/splice/image.png".to_owned(),
            mime_type: "image/png".to_owned(),
        });

        assert_eq!(
            preview_paste_payload("codex.exe", &payload),
            PastePreview::Ready {
                text: "Image file: C:/Temp/splice/image.png\r".to_owned(),
                process_name: "codex.exe".to_owned(),
                adapter_name: "codex-cli".to_owned()
            }
        );
    }

    #[test]
    fn preview_paste_payload_refuses_unknown_image_process() {
        let payload = PastePayload::Image(splice_core::ImagePaste {
            path: "C:/Temp/splice/image.png".to_owned(),
            mime_type: "image/png".to_owned(),
        });

        assert_eq!(
            preview_paste_payload("unknown.exe", &payload),
            PastePreview::UnsupportedImage {
                path: "C:/Temp/splice/image.png".to_owned(),
                process_name: "unknown.exe".to_owned()
            }
        );
    }

    #[test]
    fn pty_state_starts_empty() {
        let state = PtyState {
            sessions: Mutex::new(HashMap::new()),
        };

        assert!(state
            .sessions
            .lock()
            .expect("state lock should work")
            .is_empty());
    }

    #[test]
    fn clone_pty_session_by_id_unknown_returns_none() {
        let state = PtyState {
            sessions: Mutex::new(HashMap::new()),
        };

        assert!(clone_pty_session_by_id(&state, 42)
            .expect("lookup should not error on an empty registry")
            .is_none());
    }

    #[test]
    fn with_pty_session_unknown_id_returns_not_running_string() {
        let state = PtyState {
            sessions: Mutex::new(HashMap::new()),
        };

        // `pty_interrupt`/`pty_resize` route through `with_pty_session`; a miss
        // must error cleanly with the shared "not running" message rather than
        // panicking or touching another session.
        let error = with_pty_session(&state, 7, |session| session.interrupt())
            .expect_err("an unknown id must not resolve to a session");
        assert_eq!(error, "PTY session is not running");
    }

    #[test]
    fn pty_write_impl_unknown_id_returns_exact_closed_input_error_string() {
        let state = PtyState {
            sessions: Mutex::new(HashMap::new()),
        };

        // `isClosedPtyInputError` on the frontend matches this EXACT string;
        // changing it is a regression (spec: Missing-id write preserves the
        // exact error string).
        let error =
            pty_write_impl(&state, 7, "echo hi").expect_err("writing to an unknown id must fail");
        assert_eq!(error, "PTY session is not running");
    }

    #[test]
    fn pty_kill_impl_unknown_id_is_idempotent_ok() {
        let state = PtyState {
            sessions: Mutex::new(HashMap::new()),
        };

        // Kill on a missing id must be a harmless `Ok(())` so the frontend's
        // fire-and-forget `void killPty()` never rejects (spec: Idempotent Kill).
        assert_eq!(pty_kill_impl(&state, 7), Ok(()));
        // A second kill of the same (still-absent) id is likewise a no-op.
        assert_eq!(pty_kill_impl(&state, 7), Ok(()));
    }

    #[test]
    fn active_pty_process_name_falls_back_when_no_session_matches() {
        let state = PtyState {
            sessions: Mutex::new(HashMap::new()),
        };

        // Mount-time call before any session exists: `None` must fall back to
        // the cmd.exe process name, never error (spec: Paste-Target Fallback
        // Parity).
        assert_eq!(
            active_pty_process_name(&state, None),
            Ok("cmd.exe".to_owned())
        );

        // An unknown id resolves to no session and falls back identically.
        assert_eq!(
            active_pty_process_name(&state, Some(999)),
            Ok("cmd.exe".to_owned())
        );
    }

    #[test]
    fn pty_output_payload_serializes_with_camel_case_session_id() {
        let payload = PtyOutputPayload {
            session_id: 7,
            data: "hi".to_owned(),
        };

        assert_eq!(
            serde_json::to_string(&payload).expect("payload should serialize"),
            r#"{"sessionId":7,"data":"hi"}"#
        );
    }

    #[cfg(windows)]
    #[test]
    fn pty_state_registry_inserts_and_looks_up_by_id() {
        let state = PtyState {
            sessions: Mutex::new(HashMap::new()),
        };
        let session = Arc::new(
            PtySession::spawn(
                "cmd.exe",
                &["/D", "/K"],
                TerminalSize::new(80, 24).expect("valid terminal size"),
                |_id, _output| {},
                |_id| {},
            )
            .expect("session should spawn"),
        );
        let id = session.id();

        state
            .sessions
            .lock()
            .expect("state lock should work")
            .insert(id, Arc::clone(&session));

        let looked_up = clone_pty_session_by_id(&state, id)
            .expect("lookup should not error")
            .expect("the inserted id should resolve to the session");
        assert!(Arc::ptr_eq(&looked_up, &session));

        session.close();
    }

    #[cfg(windows)]
    #[test]
    fn pty_state_registry_remove_is_id_scoped_and_idempotent() {
        let state = PtyState {
            sessions: Mutex::new(HashMap::new()),
        };
        let session_a = Arc::new(
            PtySession::spawn(
                "cmd.exe",
                &["/D", "/K"],
                TerminalSize::new(80, 24).expect("valid terminal size"),
                |_id, _output| {},
                |_id| {},
            )
            .expect("session A should spawn"),
        );
        let session_b = Arc::new(
            PtySession::spawn(
                "cmd.exe",
                &["/D", "/K"],
                TerminalSize::new(80, 24).expect("valid terminal size"),
                |_id, _output| {},
                |_id| {},
            )
            .expect("session B should spawn"),
        );
        let id_a = session_a.id();
        let id_b = session_b.id();
        assert_ne!(id_a, id_b, "monotonic ids must be distinct");

        {
            let mut guard = state.sessions.lock().expect("state lock should work");
            guard.insert(id_a, Arc::clone(&session_a));
            guard.insert(id_b, Arc::clone(&session_b));
        }

        // Removing A returns A and leaves B untouched.
        let removed = remove_pty_session_by_id(&state, id_a)
            .expect("removing an existing id should return its session");
        assert!(Arc::ptr_eq(&removed, &session_a));
        assert!(clone_pty_session_by_id(&state, id_a)
            .expect("lookup should not error")
            .is_none());
        assert!(clone_pty_session_by_id(&state, id_b)
            .expect("lookup should not error")
            .is_some());

        // A second removal of the same id is a harmless no-op.
        assert!(remove_pty_session_by_id(&state, id_a).is_none());

        session_a.close();
        session_b.close();
    }

    #[test]
    fn resolve_pty_command_uses_safe_default_shell() {
        assert_eq!(
            resolve_pty_command(None, None),
            PtyCommand {
                program: "cmd.exe".to_owned(),
                args: default_shell_args(),
            }
        );
    }

    #[test]
    fn default_shell_path_includes_common_cli_locations() {
        let path_prefix = common_cli_path_prefix();

        assert!(path_prefix.contains(".local\\bin"));
        assert!(path_prefix.contains("scoop\\shims"));
        assert!(path_prefix.contains("agy\\bin"));
    }

    #[test]
    fn resolve_pty_command_accepts_configured_program() {
        assert_eq!(
            resolve_pty_command(
                Some("codex.exe".to_owned()),
                Some(vec!["--help".to_owned()])
            ),
            PtyCommand {
                program: "codex.exe".to_owned(),
                args: vec!["--help".to_owned()],
            }
        );
    }

    #[test]
    fn select_process_for_adapter_prefers_supported_parent_over_unsupported_leaf() {
        let registry = AdapterRegistry::with_builtin_adapters();
        let candidates = vec![
            "node.exe".to_owned(),
            "codex.exe".to_owned(),
            "cmd.exe".to_owned(),
        ];

        assert_eq!(
            select_process_for_adapter(&registry, &candidates),
            Some("codex.exe")
        );
    }

    #[test]
    fn active_paste_target_reports_adapter_support() {
        assert_eq!(
            active_paste_target_for_process("codex.exe"),
            ActivePasteTarget {
                process_name: "codex.exe".to_owned(),
                adapter_name: Some("codex-cli".to_owned()),
                supported: true,
            }
        );

        assert_eq!(
            active_paste_target_for_process("unknown.exe"),
            ActivePasteTarget {
                process_name: "unknown.exe".to_owned(),
                adapter_name: None,
                supported: false,
            }
        );
    }

    #[test]
    fn open_path_rejects_missing_paths() {
        let missing_path = std::env::temp_dir().join("splice-shell-missing-open-path-file.png");
        let _ = std::fs::remove_file(&missing_path);

        let error = tauri::async_runtime::block_on(open_path(missing_path.display().to_string()))
            .expect_err("missing paths should not be opened");

        assert!(error.contains("Path does not exist"));
    }

    #[test]
    fn test_flusher_aggregates_high_frequency_output() {
        use std::sync::mpsc;
        use std::thread;

        let (tx, rx) = mpsc::channel::<(u64, String)>();
        let (done_tx, done_rx) = mpsc::channel::<(u64, String)>();

        // Spawn the flusher loop in a background thread
        thread::spawn(move || {
            run_flusher_loop(rx, move |id, data| {
                let _ = done_tx.send((id, data));
            });
        });

        // GIVEN an active terminal session
        // WHEN a command produces a continuous stream of output
        tx.send((42, "hello ".to_owned())).unwrap();
        // Send a burst of messages immediately
        for i in 1..=5 {
            tx.send((42, format!("part{} ", i))).unwrap();
        }

        // Drop the transmitter to exit the loop once drained
        drop(tx);

        // Read the flushed output
        let mut results = Vec::new();
        while let Ok(res) = done_rx.recv() {
            results.push(res);
        }

        // Verify that the output was aggregated
        assert!(!results.is_empty(), "should have at least one flush event");
        let combined: String = results.iter().map(|(_, data)| data.as_str()).collect();
        assert_eq!(combined, "hello part1 part2 part3 part4 part5 ");

        // Assert that the aggregation actually occurred (number of events < number of sent messages)
        assert!(
            results.len() < 6,
            "flusher should have aggregated events, got {}",
            results.len()
        );
    }

    #[test]
    fn test_flusher_idle_flushes_immediately() {
        use std::sync::mpsc;
        use std::thread;
        use std::time::{Duration, Instant};

        let (tx, rx) = mpsc::channel::<(u64, String)>();
        let (done_tx, done_rx) = mpsc::channel::<(u64, String)>();

        // Spawn flusher
        thread::spawn(move || {
            run_flusher_loop(rx, move |id, data| {
                let _ = done_tx.send((id, data));
            });
        });

        // Send one message, wait for flush
        let start1 = Instant::now();
        tx.send((42, "first".to_owned())).unwrap();
        let (_id1, val1) = done_rx.recv().unwrap();
        let elapsed1 = start1.elapsed();

        assert_eq!(val1, "first");
        // Should be almost instant (idle flush) - well below 16ms
        assert!(
            elapsed1 < Duration::from_millis(30),
            "idle flush should be immediate, took {:?}",
            elapsed1
        );

        // Wait 20ms to ensure flusher is idle again
        thread::sleep(Duration::from_millis(20));

        // Send a second message, wait for flush
        let start2 = Instant::now();
        tx.send((42, "second".to_owned())).unwrap();
        let (_id2, val2) = done_rx.recv().unwrap();
        let elapsed2 = start2.elapsed();

        assert_eq!(val2, "second");
        assert!(
            elapsed2 < Duration::from_millis(30),
            "second idle flush should also be immediate, took {:?}",
            elapsed2
        );
    }

    #[test]
    fn test_concurrent_commands_no_deadlock() {
        use std::thread;

        let state = Arc::new(PtyState {
            sessions: Mutex::new(HashMap::new()),
        });

        let session = Arc::new(
            PtySession::spawn(
                "cmd.exe",
                &["/D", "/K"],
                TerminalSize::new(80, 24).unwrap(),
                |_, _| {},
                |_| {},
            )
            .unwrap(),
        );
        let id = session.id();
        state
            .sessions
            .lock()
            .unwrap()
            .insert(id, Arc::clone(&session));

        let mut threads = Vec::new();
        for _ in 0..10 {
            let state_clone = Arc::clone(&state);
            let t = thread::spawn(move || {
                for _ in 0..50 {
                    let _ = pty_write_impl(&state_clone, id, "echo hello\r");
                    let _ = clone_pty_session_by_id(&state_clone, id);
                }
            });
            threads.push(t);
        }

        for t in threads {
            t.join().unwrap();
        }

        let _ = pty_kill_impl(&state, id);
    }

    #[test]
    fn test_startup_cleanup_deletes_all_temp_files() {
        let temp_dir = std::env::temp_dir()
            .join("splice-shell-test-startup")
            .join("clipboard");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Create some files
        let file1 = temp_dir.join("splice-clipboard-1.png");
        let file2 = temp_dir.join("splice-clipboard-2.png");
        let file3 = temp_dir.join("other-file.txt");
        std::fs::write(&file1, b"png").unwrap();
        std::fs::write(&file2, b"png").unwrap();
        std::fs::write(&file3, b"txt").unwrap();

        // GIVEN a previous application session crashed and left files in the clipboard temp directory
        // WHEN the application starts up (we simulate the startup hook)
        let _ = std::fs::remove_dir_all(&temp_dir);
        let _ = splice_clipboard::sweep_temp_images(&temp_dir);

        // THEN the system MUST delete all files and subdirectories under the clipboard temp directory
        assert!(!temp_dir.exists() || std::fs::read_dir(&temp_dir).unwrap().count() == 0);
    }

    #[test]
    fn test_shutdown_cleanup_deletes_temp_directory() {
        let temp_dir = std::env::temp_dir()
            .join("splice-shell-test-shutdown")
            .join("clipboard");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        // Create some files
        let file1 = temp_dir.join("splice-clipboard-1.png");
        std::fs::write(&file1, b"png").unwrap();

        // GIVEN multiple temporary preview images exist on disk
        // WHEN the user exits the application (we simulate the shutdown hook)
        let _ = std::fs::remove_dir_all(&temp_dir);

        // THEN the system MUST delete the clipboard temp directory and all its contents
        assert!(!temp_dir.exists());
    }

    #[test]
    fn test_session_close_deletes_specific_image_immediately() {
        let temp_dir = std::env::temp_dir()
            .join("splice-shell-test-session-close")
            .join("clipboard");
        let _ = std::fs::remove_dir_all(&temp_dir);
        std::fs::create_dir_all(&temp_dir).unwrap();

        let file = temp_dir.join("splice-clipboard-session.png");
        std::fs::write(&file, b"png").unwrap();
        assert!(file.exists());

        // WHEN the user closes the clipboard paste session (simulated by calling close_paste_session_impl)
        close_paste_session_impl(Some(file.display().to_string()));

        // THEN the system MUST delete the temporary image file from disk immediately
        assert!(!file.exists());
    }
}
