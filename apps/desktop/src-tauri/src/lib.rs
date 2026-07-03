use serde::Serialize;
use splice_core::{AdapterRegistry, PastePayload, PasteRoute};
use splice_pty::{PtySession, TerminalSize};
use std::{
    path::PathBuf,
    process::Command,
    sync::{Arc, Mutex},
};
use tauri::{Emitter, Manager, State};

const PTY_OUTPUT_EVENT: &str = "pty-output";
const PTY_EXIT_EVENT: &str = "pty-exit";

struct PtyState {
    // Commands must lock this mutex only long enough to clone the `Arc`
    // (or take it on teardown) and release the guard BEFORE calling any
    // potentially blocking `PtySession` method. Otherwise a hung child
    // blocking `pty_write` would stall `pty_interrupt`/`pty_resize` behind
    // this lock — the same stall the library layer already eliminates
    // internally with the identical `Arc` pattern.
    //
    // Session death is no longer polled from the frontend. Each `PtySession`
    // runs a waiter thread that pushes a `pty-exit` event on natural exit
    // (see `pty_spawn`); the natural-exit path also clears this state itself
    // (`clear_and_close_session_by_id`) so a dead session's ConPTY/pipe/job
    // handles never linger.
    session: Mutex<Option<Arc<PtySession>>>,
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
fn preview_clipboard_image_paste(process_name: String) -> Result<PastePreview, String> {
    let payload = read_clipboard_image_paste_payload()?;

    Ok(preview_paste_payload(&process_name, &payload))
}

#[tauri::command]
fn active_paste_target(state: State<'_, PtyState>) -> Result<ActivePasteTarget, String> {
    let process_name = active_pty_process_name(&state)?;
    Ok(active_paste_target_for_process(&process_name))
}

#[tauri::command]
fn preview_active_clipboard_image_paste(
    state: State<'_, PtyState>,
) -> Result<PastePreview, String> {
    let process_name = active_pty_process_name(&state)?;
    let payload = read_clipboard_image_paste_payload()?;

    Ok(preview_paste_payload(&process_name, &payload))
}

#[tauri::command]
fn pty_spawn(
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

    let previous_session = state
        .session
        .lock()
        .map_err(|_| "PTY state lock poisoned".to_owned())?
        .take();
    // Close outside the lock: teardown blocks on process shutdown and the
    // reader-thread join, and must not stall concurrent PTY commands.
    if let Some(previous_session) = previous_session {
        previous_session.close();
    }

    let output_app = app.clone();
    let cleanup_app = app.clone();
    let exit_app = app;
    let session = PtySession::spawn(
        &command.program,
        &command_args,
        size,
        move |output| {
            let _ = output_app.emit(PTY_OUTPUT_EVENT, output);
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
            .session
            .lock()
            .map_err(|_| "PTY state lock poisoned".to_owned())?;
        *guard = Some(Arc::new(session));
    }

    // Instant-exit race: if the child died before we stored it, its detached
    // `clear_and_close_session_by_id(id)` cleanup already ran while
    // `state.session` was still `None` (a no-op), and we just stored a dead
    // session whose ConPTY/pipe/job handles and reader/waiter threads would
    // otherwise linger until the next interaction. Now that it is stored,
    // re-check liveness and, if it is not running, clear+close it immediately.
    // `is_running()` only errs on a poisoned lock or an already-closed session,
    // so a non-`Ok(true)` result is treated as dead. The teardown reuses the
    // id-scoped, `Option::take`-idempotent `clear_and_close_session_by_id`, so
    // a replacement spawned concurrently is never torn down, and its `close()`
    // runs with the state lock released (no thread-join deadlock).
    let still_running = clone_pty_session(&state)?
        .and_then(|session| session.is_running().ok())
        .unwrap_or(false);
    if !still_running {
        clear_and_close_session_by_id(&cleanup_app, id);
    }

    Ok(id)
}

/// Remove and close the stored session only if it is still the exact session
/// with `id` that just exited, so a replacement spawned concurrently by
/// `pty_spawn` is never torn down by a stale natural-exit path. Runs on a
/// detached thread off the waiter thread (see `pty_spawn`); `close()` here
/// joins the now-finished waiter and releases the dead session's handles.
fn clear_and_close_session_by_id(app: &tauri::AppHandle, id: u64) {
    let state = app.state::<PtyState>();
    let session = {
        let Ok(mut guard) = state.session.lock() else {
            return;
        };
        if guard.as_ref().is_some_and(|current| current.id() == id) {
            guard.take()
        } else {
            None
        }
    };
    // Close outside the lock: teardown blocks on process shutdown and the
    // reader/waiter-thread joins, and must not stall concurrent PTY commands.
    if let Some(session) = session {
        session.close();
    }
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

#[tauri::command]
fn pty_write(state: State<'_, PtyState>, data: String) -> Result<(), String> {
    let session =
        clone_pty_session(&state)?.ok_or_else(|| "PTY session is not running".to_owned())?;

    // The write runs on the clone with the state lock released, so a hung
    // child cannot stall `pty_interrupt` (or any other PTY command).
    match session.write(&data) {
        Ok(()) => Ok(()),
        Err(error) if error.is_terminal_closed() => {
            clear_pty_session_if_current(&state, &session);
            session.close();
            Err("PTY session closed; start a new terminal session".to_owned())
        }
        Err(error) => Err(error.to_string()),
    }
}

#[tauri::command]
fn pty_interrupt(state: State<'_, PtyState>) -> Result<(), String> {
    with_pty_session(&state, |session| session.interrupt())
}

#[tauri::command]
fn pty_resize(state: State<'_, PtyState>, cols: u16, rows: u16) -> Result<(), String> {
    let size = TerminalSize::new(cols, rows).map_err(|error| format!("{error:?}"))?;
    with_pty_session(&state, |session| session.resize(size))
}

#[tauri::command]
fn pty_kill(state: State<'_, PtyState>) -> Result<(), String> {
    let session = state
        .session
        .lock()
        .map_err(|_| "PTY state lock poisoned".to_owned())?
        .take();
    // Close outside the lock: teardown blocks on process shutdown and the
    // reader-thread join, and must not stall concurrent PTY commands.
    if let Some(session) = session {
        session.close();
    }

    Ok(())
}

#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
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

/// Clone the current session handle while holding the state lock only for
/// the duration of the `Arc` clone. Callers invoke (possibly blocking)
/// `PtySession` methods on the returned clone AFTER the lock is released.
fn clone_pty_session(state: &State<'_, PtyState>) -> Result<Option<Arc<PtySession>>, String> {
    let guard = state
        .session
        .lock()
        .map_err(|_| "PTY state lock poisoned".to_owned())?;

    Ok(guard.as_ref().map(Arc::clone))
}

/// Remove the stored session only if it is still the exact session that
/// observed the failure, so a replacement spawned concurrently by
/// `pty_spawn` is never torn down by a stale error path. Best-effort on
/// lock poisoning: the caller's "session closed" error is the useful one.
fn clear_pty_session_if_current(state: &State<'_, PtyState>, session: &Arc<PtySession>) {
    if let Ok(mut guard) = state.session.lock() {
        if guard
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            guard.take();
        }
    }
}

fn with_pty_session<F>(state: &State<'_, PtyState>, operation: F) -> Result<(), String>
where
    F: FnOnce(&PtySession) -> Result<(), splice_pty::PtyError>,
{
    let session =
        clone_pty_session(state)?.ok_or_else(|| "PTY session is not running".to_owned())?;

    // Run the operation on the clone with the state lock released, so a
    // blocking call here can never stall other PTY commands.
    operation(&session).map_err(|error| error.to_string())
}

fn active_pty_process_name(state: &State<'_, PtyState>) -> Result<String, String> {
    let session = clone_pty_session(state)?;

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

fn read_clipboard_image_paste_payload() -> Result<PastePayload, String> {
    let temp_dir = std::env::temp_dir().join("splice-shell").join("clipboard");
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(PtyState {
            session: Mutex::new(None),
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
            open_path
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Splice Shell desktop app");
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
            session: Mutex::new(None),
        };

        assert!(state
            .session
            .lock()
            .expect("state lock should work")
            .is_none());
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

        let error = open_path(missing_path.display().to_string())
            .expect_err("missing paths should not be opened");

        assert!(error.contains("Path does not exist"));
    }
}
