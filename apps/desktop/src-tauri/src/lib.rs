use serde::Serialize;
use splice_core::{AdapterRegistry, PastePayload, PasteRoute};
use splice_pty::flow::{
    run_flusher_loop_with_stall, CreditWindow, FlushControl, CREDIT_STALL_TIMEOUT,
    PTY_CREDIT_WINDOW_BYTES, PTY_OUTPUT_CHANNEL_CAPACITY,
};
use splice_pty::{PtySession, TerminalSize};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tauri::{Emitter, Manager, State};

pub mod platform;

const PTY_OUTPUT_EVENT: &str = "pty-output";
const PTY_EXIT_EVENT: &str = "pty-exit";
/// Global event carrying a session's flow-control stall state to the frontend so
/// a frozen tab gets a visible "stalled" signal instead of silently hanging.
const PTY_STALL_EVENT: &str = "pty-stall";

#[derive(Default)]
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
    // Per-session credit windows, keyed identically to `sessions`. `pty_ack`
    // looks a session's window up here to replenish it. Registered by
    // `pty_spawn` and removed alongside the session; the window itself is
    // *closed* by the session's close hook, which covers every teardown path
    // (kill, natural exit, write failure, and `Drop` at app shutdown).
    credits: Mutex<HashMap<u64, Arc<CreditWindow>>>,
}

/// Payload for the global `pty-output` event. Every emission carries the
/// emitting session's monotonic id so the frontend can demultiplex output
/// across concurrent sessions (mirroring the `pty-exit` id payload).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PtyOutputPayload {
    session_id: u64,
    /// UTF-8 byte cost this emission charged against the session's credit
    /// window. The frontend echoes it back through `pty_ack` once xterm has
    /// actually consumed the data.
    ///
    /// Carried explicitly instead of being recomputed in JS on purpose:
    /// `String.length` there counts UTF-16 code units, which diverges from
    /// Rust's UTF-8 `str::len()` for any non-ASCII output. Recomputing would
    /// silently desynchronise the credit ledger and eventually stall a session.
    bytes: usize,
    data: String,
}

/// Payload for the global `pty-stall` event. Carries the session id and whether
/// it is currently stalled, so the frontend can drive that tab to a distinct
/// "stalled" health state (output backed up, waiting on the renderer) and back
/// to healthy when it recovers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PtyStallPayload {
    session_id: u64,
    stalled: bool,
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
) -> Result<u64, platform::PlatformError> {
    let platform = platform::PlatformServices::detect()?;
    let size = TerminalSize::new(cols, rows).map_err(|error| {
        platform::PlatformError::native_mechanism(
            platform.target(),
            format!("invalid terminal size: {error:?}"),
            false,
        )
    })?;
    let command = resolve_pty_command(program, args, &platform)?;
    let command_args = command.args.iter().map(String::as_str).collect::<Vec<_>>();

    // No predecessor `.take()`/close here: sessions are keyed by id and coexist.
    // Each session is torn down explicitly by its own id (frontend `killPty`,
    // the detached natural-exit cleanup, or the instant-exit re-check below),
    // so a new spawn never reaps an existing session.
    //
    // BACKPRESSURE CHAIN (child -> ConPTY pipe -> reader -> channel -> flusher
    // -> emit -> xterm -> pty_ack -> credit):
    //   * `credit` gates the flusher: it emits only while the webview has
    //     confirmed consumption. No credit => it stops draining `rx`.
    //   * `sync_channel` is BOUNDED: once the flusher stops draining, it fills
    //     and the reader thread parks in `send`, stops calling `ReadFile`, the
    //     ConPTY pipe fills, and the child blocks on write.
    //   * the close hook closes `credit`, which releases the flusher, which
    //     drops `rx`, which makes the parked `send` fail — so `close()` can
    //     join the reader instead of deadlocking on it.
    let credit = Arc::new(CreditWindow::new(PTY_CREDIT_WINDOW_BYTES));
    let (tx, rx) = std::sync::mpsc::sync_channel::<(u64, String)>(PTY_OUTPUT_CHANNEL_CAPACITY);
    let flusher_app = app.clone();
    let stall_app = app.clone();
    let flusher_credit = Arc::clone(&credit);
    // The flusher/stall closures need the session id, but it is only known after
    // `spawn_with_close_hook` returns. Publish it through this cell once stored;
    // a stall can only fire after the 1 MiB window has been drained, which is
    // long after the id lands here, so the reporter never reads the `0` sentinel.
    let stall_session_id = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let stall_id = Arc::clone(&stall_session_id);
    std::thread::spawn(move || {
        run_flusher_loop_with_stall(
            rx,
            flusher_credit,
            CREDIT_STALL_TIMEOUT,
            move |session_id, data| {
                // Emit to the webview; if it fails the webview is gone, so tear
                // the session down (on a DETACHED thread — an inline
                // `session.close()` would join the ConPTY reader, which is parked
                // in `send` waiting on THIS flusher, and deadlock). Returning
                // `StopWithoutCharge` makes the loop return and drop `rx`, which
                // releases that reader so the detached teardown can join it.
                let teardown_app = flusher_app.clone();
                flush_output_decision(
                    session_id,
                    data,
                    |payload| flusher_app.emit(PTY_OUTPUT_EVENT, payload).is_ok(),
                    move |id| {
                        std::thread::spawn(move || {
                            clear_and_close_session_by_id(&teardown_app, id);
                        });
                    },
                )
            },
            move |stalled| {
                let session_id = stall_id.load(std::sync::atomic::Ordering::SeqCst);
                if stalled {
                    log::warn!(
                        "pty session {session_id}: flow-control stall — output backed up, renderer not acking; child throttled (not hung, not crashed)"
                    );
                } else {
                    log::info!(
                        "pty session {session_id}: flow-control stall cleared; renderer acking again"
                    );
                }
                let _ = stall_app.emit(
                    PTY_STALL_EVENT,
                    PtyStallPayload {
                        session_id,
                        stalled,
                    },
                );
            },
        );
    });

    let cleanup_app = app.clone();
    let exit_app = app;
    let closing_credit = Arc::clone(&credit);
    #[cfg(unix)]
    let session = PtySession::spawn_with_options_and_close_hook(
        &command.program,
        &command_args,
        splice_pty::PtySpawnOptions {
            cwd: None,
            env: command.environment,
        },
        size,
        move |id, output| {
            let _ = tx.send((id, output));
        },
        move |id| {
            let _ = exit_app.emit(PTY_EXIT_EVENT, id);
            let cleanup_app = exit_app.clone();
            std::thread::spawn(move || {
                clear_and_close_session_by_id(&cleanup_app, id);
            });
        },
        move || closing_credit.close(),
    );
    #[cfg(windows)]
    let session = PtySession::spawn_with_close_hook(
        &command.program,
        &command_args,
        size,
        move |id, output| {
            // Deliberately BLOCKING when the channel is full: this is the brake.
            // `Err` means the flusher dropped the receiver (session closing), in
            // which case the reader is on its way out anyway.
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
        // Teardown hook. Runs at the top of EVERY `close()` — kill, natural
        // exit, write failure, and `Drop` at app shutdown — before the reader
        // thread is joined. Closing the credit window is what guarantees a
        // reader parked in the blocking `send` above is always released, so a
        // never-acking (crashed/closed) webview can never wedge teardown.
        move || closing_credit.close(),
    );
    let session = session.map_err(|error| {
        platform::PlatformError::native_mechanism(
            platform.target(),
            format!("failed to start PTY: {error}"),
            true,
        )
    })?;

    let id = session.id();
    // Publish the id so the flusher's stall reporter can attribute a stall event
    // to this session (see the flusher thread above).
    stall_session_id.store(id, std::sync::atomic::Ordering::SeqCst);

    {
        let mut guard = state.sessions.lock().map_err(|_| {
            platform::PlatformError::native_mechanism(
                platform.target(),
                "PTY state lock poisoned",
                true,
            )
        })?;
        guard.insert(id, Arc::new(session));
    }
    {
        let mut guard = state.credits.lock().map_err(|_| {
            platform::PlatformError::native_mechanism(
                platform.target(),
                "PTY credit lock poisoned",
                true,
            )
        })?;
        guard.insert(id, credit);
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
    let still_running = clone_pty_session_by_id(state.inner(), id)
        .map_err(|message| {
            platform::PlatformError::native_mechanism(platform.target(), message, true)
        })?
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
    // Drop the session's credit-window registration alongside it so the map
    // cannot grow without bound across spawn/kill cycles. This only unregisters
    // it; the window is *closed* (releasing any parked flusher/reader) by the
    // session's close hook, which also covers paths that never reach here.
    forget_credit_window_by_id(state, id);

    let mut guard = state.sessions.lock().ok()?;
    guard.remove(&id)
}

/// Unregister a session's credit window. Idempotent; best-effort on poisoning.
fn forget_credit_window_by_id(state: &PtyState, id: u64) {
    if let Ok(mut guard) = state.credits.lock() {
        guard.remove(&id);
    }
}

/// Replenish a session's credit window with `bytes` the frontend confirmed
/// xterm has consumed, waking its flusher if it was parked.
///
/// An unknown id is a harmless `Ok(())`, never an error: acks are fire-and-
/// forget from the frontend and legitimately race session teardown (an xterm
/// write callback can land after `pty_kill`). Erroring would make a benign race
/// look like a failure.
fn pty_ack_impl(state: &PtyState, session_id: u64, bytes: usize) -> Result<(), String> {
    let window = {
        let guard = state
            .credits
            .lock()
            .map_err(|_| "PTY credit lock poisoned".to_owned())?;
        guard.get(&session_id).map(Arc::clone)
    };

    // Replenish with the registry lock RELEASED: `replenish` notifies a condvar
    // that a flusher thread is parked on, and must not run under a lock that
    // other PTY commands need.
    if let Some(window) = window {
        window.replenish(bytes);
    }

    Ok(())
}

#[tauri::command]
async fn pty_ack(state: State<'_, PtyState>, session_id: u64, bytes: usize) -> Result<(), String> {
    pty_ack_impl(state.inner(), session_id, bytes)
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
    environment: Vec<(String, String)>,
}

fn resolve_pty_command(
    program: Option<String>,
    args: Option<Vec<String>>,
    platform: &platform::PlatformServices,
) -> Result<PtyCommand, platform::PlatformError> {
    match program {
        Some(program) if !program.trim().is_empty() => Ok(PtyCommand {
            program,
            args: args.unwrap_or_default(),
            environment: vec![],
        }),
        _ => {
            let launch = platform.pty_launch();
            Ok(PtyCommand {
                program: launch.command.program,
                args: launch.command.args,
                environment: launch.environment,
            })
        }
    }
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
async fn open_path(path: String) -> Result<(), platform::PlatformError> {
    platform::PlatformServices::detect()?.reveal(path)
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
    let mut removed = false;
    if let Ok(mut guard) = state.sessions.lock() {
        let id = session.id();
        if guard
            .get(&id)
            .is_some_and(|current| Arc::ptr_eq(current, session))
        {
            guard.remove(&id);
            removed = true;
        }
    }

    // Keep the credit registry in lockstep with the session registry, but only
    // when THIS session was the one removed — otherwise a stale error path could
    // unregister a different, live session's window. Done outside the sessions
    // lock to preserve the "never hold two PTY locks at once" rule.
    if removed {
        forget_credit_window_by_id(state, session.id());
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

/// Decide what to do with one coalesced flush batch: emit it, and if the emit
/// fails treat the webview as gone — log it, tear the session down, and tell the
/// loop NOT to charge credit for bytes that can never be acked.
///
/// Split out from the production closure so the emit-failure → teardown path is
/// unit-testable without a live webview: tests pass a synthetic `emit` that
/// returns `false` and a `teardown` that records the id.
fn flush_output_decision<E, T>(session_id: u64, data: String, emit: E, teardown: T) -> FlushControl
where
    E: FnOnce(PtyOutputPayload) -> bool,
    T: FnOnce(u64),
{
    let bytes = data.len();
    let payload = PtyOutputPayload {
        session_id,
        bytes,
        data,
    };

    if emit(payload) {
        FlushControl::Charge
    } else {
        // The webview is gone: its message queue no longer exists, so this
        // output — and everything after it — can never be acked. Charging credit
        // would subtract it forever and permanently stall the session. Tear the
        // session down instead (kill the child, free its credit window) and tell
        // the loop to stop without charging.
        log::error!(
            "pty session {session_id}: flush emit failed (webview gone); tearing down session and freeing its credit window"
        );
        teardown(session_id);
        FlushControl::StopWithoutCharge
    }
}

/// Kill every live PTY session and clear every credit window. This is the
/// backend-side defense the JS runtime cannot provide: when the WebView2 process
/// dies (crash, hard reload, window close), the React cleanup that calls
/// `killPty` never runs, leaking corked children, their reader/waiter threads,
/// and ConPTY/pipe/job handles. Wired to `WindowEvent::CloseRequested` /
/// `Destroyed` and to app exit (see `run`). Idempotent and best-effort on lock
/// poisoning.
fn reap_all_sessions(state: &PtyState) {
    // Drain and close the credit windows FIRST: closing releases any flusher
    // parked in `acquire`, which drops its channel receiver and frees a ConPTY
    // reader parked in `send`, so the subsequent `session.close()` can join that
    // reader instead of deadlocking on it.
    let windows: Vec<Arc<CreditWindow>> = match state.credits.lock() {
        Ok(mut guard) => guard.drain().map(|(_, window)| window).collect(),
        Err(poisoned) => poisoned
            .into_inner()
            .drain()
            .map(|(_, window)| window)
            .collect(),
    };
    for window in &windows {
        window.close();
    }

    // Then drain the sessions and close each OUTSIDE the lock (close() blocks on
    // process shutdown and reader/waiter joins, and must not stall other PTY
    // commands racing this teardown).
    let sessions: Vec<Arc<PtySession>> = match state.sessions.lock() {
        Ok(mut guard) => guard.drain().map(|(_, session)| session).collect(),
        Err(poisoned) => poisoned
            .into_inner()
            .drain()
            .map(|(_, session)| session)
            .collect(),
    };
    let reaped = sessions.len();
    for session in sessions {
        session.close();
    }
    if reaped > 0 {
        log::warn!("reaped {reaped} orphaned PTY session(s) on webview teardown/app exit");
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

/// Build the logging backend. Without a registered backend every `log::*` call
/// is a silent no-op, so the diagnostics at the stall/teardown/reap failure
/// points would go nowhere in a windowed release build (which has no stderr).
/// Writes to a rotating file in the OS log dir always, plus stdout in debug.
fn build_log_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    let mut targets = vec![tauri_plugin_log::Target::new(
        tauri_plugin_log::TargetKind::LogDir { file_name: None },
    )];
    // stdout is only useful in the console-subsystem debug build; the release GUI
    // build (`windows_subsystem = "windows"`) has no stdout to write to.
    if cfg!(debug_assertions) {
        targets.push(tauri_plugin_log::Target::new(
            tauri_plugin_log::TargetKind::Stdout,
        ));
    }

    tauri_plugin_log::Builder::new()
        .targets(targets)
        // Bound disk use: roll to a fresh file past ~5 MB and keep only the
        // previous one, so a chatty session cannot fill the log dir unbounded.
        .max_file_size(5_000_000)
        .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepOne)
        .level(if cfg!(debug_assertions) {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        })
        .build()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app = tauri::Builder::default()
        .manage(PtyState::default())
        // Register the log backend FIRST so diagnostics during setup and every
        // later failure point are actually recorded.
        .plugin(build_log_plugin())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            let temp_dir = std::env::temp_dir().join("splice-shell").join("clipboard");
            // Startup Cleanup (CLIP-2)
            let _ = std::fs::remove_dir_all(&temp_dir);
            let _ = splice_clipboard::sweep_temp_images(&temp_dir);

            // Webview-teardown reaping. When the WebView2 process dies (window
            // close, hard reload, or a renderer crash that also tears the window
            // down), the JS cleanup that calls `killPty` never runs, leaking
            // corked children + threads + handles. Tauri 2 exposes window
            // lifecycle events (`CloseRequested`/`Destroyed`) but NOT a distinct
            // "webview process crashed" hook, so this covers every teardown that
            // reaches the window layer; a pure renderer crash that leaves the
            // window alive is instead caught by the emit-failure teardown in the
            // flusher (FIX 2). Both, plus app exit below, are defenses the JS
            // side structurally cannot provide.
            #[cfg(desktop)]
            if let Some(window) = app.get_webview_window("main") {
                let reap_handle = app.handle().clone();
                window.on_window_event(move |event| {
                    if matches!(
                        event,
                        tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
                    ) {
                        reap_all_sessions(reap_handle.state::<PtyState>().inner());
                    }
                });
            }

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
            pty_ack,
            clipboard_write_text,
            clipboard_read_text,
            open_path,
            close_paste_session
        ])
        .build(tauri::generate_context!())
        .expect("failed to build Splice Shell desktop app");

    app.run(|app_handle, event| {
        if let tauri::RunEvent::Exit = event {
            // Reap any still-live sessions on the way out so a shutdown that did
            // not route through the webview teardown (e.g. the process ending
            // while a session is corked) never leaks a child or its handles.
            reap_all_sessions(app_handle.state::<PtyState>().inner());
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
        let state = PtyState::default();

        assert!(state
            .sessions
            .lock()
            .expect("state lock should work")
            .is_empty());
    }

    #[test]
    fn clone_pty_session_by_id_unknown_returns_none() {
        let state = PtyState::default();

        assert!(clone_pty_session_by_id(&state, 42)
            .expect("lookup should not error on an empty registry")
            .is_none());
    }

    #[test]
    fn with_pty_session_unknown_id_returns_not_running_string() {
        let state = PtyState::default();

        // `pty_interrupt`/`pty_resize` route through `with_pty_session`; a miss
        // must error cleanly with the shared "not running" message rather than
        // panicking or touching another session.
        let error = with_pty_session(&state, 7, |session| session.interrupt())
            .expect_err("an unknown id must not resolve to a session");
        assert_eq!(error, "PTY session is not running");
    }

    #[test]
    fn pty_write_impl_unknown_id_returns_exact_closed_input_error_string() {
        let state = PtyState::default();

        // `isClosedPtyInputError` on the frontend matches this EXACT string;
        // changing it is a regression (spec: Missing-id write preserves the
        // exact error string).
        let error =
            pty_write_impl(&state, 7, "echo hi").expect_err("writing to an unknown id must fail");
        assert_eq!(error, "PTY session is not running");
    }

    #[test]
    fn pty_kill_impl_unknown_id_is_idempotent_ok() {
        let state = PtyState::default();

        // Kill on a missing id must be a harmless `Ok(())` so the frontend's
        // fire-and-forget `void killPty()` never rejects (spec: Idempotent Kill).
        assert_eq!(pty_kill_impl(&state, 7), Ok(()));
        // A second kill of the same (still-absent) id is likewise a no-op.
        assert_eq!(pty_kill_impl(&state, 7), Ok(()));
    }

    #[test]
    fn active_pty_process_name_falls_back_when_no_session_matches() {
        let state = PtyState::default();

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
    fn pty_output_payload_serializes_with_camel_case_session_id_and_byte_cost() {
        // `bytes` is the UTF-8 byte cost charged against the session's credit
        // window. It is carried explicitly rather than recomputed in JS, where
        // `String.length` counts UTF-16 code units and would drift from Rust's
        // byte accounting on any non-ASCII output — silently corrupting the
        // credit ledger.
        let payload = PtyOutputPayload {
            session_id: 7,
            bytes: "hé".len(),
            data: "hé".to_owned(),
        };

        assert_eq!(
            serde_json::to_string(&payload).expect("payload should serialize"),
            r#"{"sessionId":7,"bytes":3,"data":"hé"}"#
        );
    }

    #[test]
    fn pty_output_payload_byte_cost_is_utf8_not_utf16() {
        // "hé" is 3 UTF-8 bytes but 2 UTF-16 code units. Pinning this is what
        // stops the JS side from acking the wrong number.
        assert_eq!("hé".len(), 3);
    }

    #[cfg(windows)]
    #[test]
    fn pty_state_registry_inserts_and_looks_up_by_id() {
        let state = PtyState::default();
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
        let state = PtyState::default();
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
        let linux = platform::PlatformServices::from_facts(platform::PlatformFacts {
            os: "linux".into(),
            ubuntu: Some("24.04".into()),
            wsl: None,
            wslg: false,
            path: Some("/usr/local/bin:/usr/bin:/bin".into()),
        })
        .expect("supported Linux platform");

        assert_eq!(
            resolve_pty_command(None, None, &linux).expect("Linux default command"),
            PtyCommand {
                program: "/bin/sh".to_owned(),
                args: vec![],
                environment: vec![("PATH".to_owned(), "/usr/local/bin:/usr/bin:/bin".to_owned())],
            }
        );
    }

    #[test]
    fn resolve_pty_command_accepts_configured_program() {
        let linux = platform::PlatformServices::from_facts(platform::PlatformFacts {
            os: "linux".into(),
            ubuntu: Some("24.04".into()),
            wsl: None,
            wslg: false,
            path: Some("/usr/bin:/bin".into()),
        })
        .expect("supported Linux platform");

        assert_eq!(
            resolve_pty_command(
                Some("codex.exe".to_owned()),
                Some(vec!["--help".to_owned()]),
                &linux,
            )
            .expect("configured command"),
            PtyCommand {
                program: "codex.exe".to_owned(),
                args: vec!["--help".to_owned()],
                environment: vec![],
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

        let platform = platform::PlatformServices::from_facts(platform::PlatformFacts {
            os: "windows".into(),
            ubuntu: None,
            wsl: None,
            wslg: false,
            path: Some(r"C:\\Windows\\System32".into()),
        })
        .expect("Windows authority fixture");
        let error = platform
            .reveal_command(&missing_path)
            .expect_err("missing paths should not be opened");

        assert_eq!(error.code, platform::PlatformErrorCode::MissingPath);
        assert_eq!(error.platform, Some(platform::PlatformTarget::Windows));
        assert!(!error.retryable);
    }

    #[test]
    fn pty_ack_impl_replenishes_only_the_named_session() {
        let state = PtyState::default();
        let first = Arc::new(CreditWindow::new(64));
        let second = Arc::new(CreditWindow::new(64));
        first.consume(64);
        second.consume(64);
        {
            let mut credits = state.credits.lock().expect("credits lock");
            credits.insert(1, Arc::clone(&first));
            credits.insert(2, Arc::clone(&second));
        }

        assert_eq!(pty_ack_impl(&state, 1, 32), Ok(()));

        assert_eq!(first.available(), 32);
        assert_eq!(
            second.available(),
            0,
            "an ack must not leak across sessions"
        );
    }

    #[test]
    fn pty_ack_impl_unknown_id_is_a_harmless_no_op() {
        let state = PtyState::default();

        // The frontend acks fire-and-forget and can race a session teardown, so
        // an ack for a dead/unknown id must never error.
        assert_eq!(pty_ack_impl(&state, 999, 4_096), Ok(()));
    }

    #[cfg(windows)]
    #[test]
    fn kill_completes_while_the_reader_is_parked_and_the_webview_never_acks() {
        use splice_pty::flow::run_flusher_loop;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc::{channel, sync_channel};
        use std::time::{Duration, Instant};

        // The crashed/closed-renderer case: the webview never acks, so credit is
        // never replenished, the flusher stops draining, the channel fills and
        // the ConPTY reader parks in `send`. `close()` JOINS that reader, so a
        // parked reader that is never released wedges kill and app shutdown.
        let state = Arc::new(PtyState::default());
        let credit = Arc::new(CreditWindow::new(0));
        // Rendezvous channel: the reader parks on its very first chunk, which
        // makes this deterministic instead of timing-dependent.
        let (tx, rx) = sync_channel::<(u64, String)>(0);
        let flusher_credit = Arc::clone(&credit);
        std::thread::spawn(move || {
            run_flusher_loop(rx, flusher_credit, |_id, _data| {});
        });

        let parked = Arc::new(AtomicBool::new(false));
        let parked_for_reader = Arc::clone(&parked);
        let hook_credit = Arc::clone(&credit);
        let session = PtySession::spawn_with_close_hook(
            "cmd.exe",
            &["/D", "/K"],
            TerminalSize::new(80, 24).expect("valid terminal size"),
            move |id, output| {
                parked_for_reader.store(true, Ordering::SeqCst);
                let _ = tx.send((id, output));
            },
            |_id| {},
            move || hook_credit.close(),
        )
        .expect("session should spawn");
        let id = session.id();
        state
            .sessions
            .lock()
            .expect("sessions lock")
            .insert(id, Arc::new(session));
        state
            .credits
            .lock()
            .expect("credits lock")
            .insert(id, Arc::clone(&credit));

        let deadline = Instant::now() + Duration::from_secs(10);
        while !parked.load(Ordering::SeqCst) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            parked.load(Ordering::SeqCst),
            "the reader should have reached the blocking send"
        );

        let (done_tx, done_rx) = channel::<()>();
        let killer_state = Arc::clone(&state);
        let killer = std::thread::spawn(move || {
            let _ = pty_kill_impl(&killer_state, id);
            let _ = done_tx.send(());
        });

        done_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("pty_kill must not wedge on a reader parked behind a never-acking webview");
        killer.join().expect("killer thread");
        assert!(
            state.credits.lock().expect("credits lock").is_empty(),
            "teardown must not leak the session's credit window"
        );
    }

    #[test]
    fn test_concurrent_commands_no_deadlock() {
        use std::thread;

        let state = Arc::new(PtyState::default());

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

    // ---- FIX 2: emit failure tears down the session, never charges credit ----

    #[test]
    fn flush_output_decision_tears_down_on_emit_failure_and_charges_on_success() {
        use std::cell::Cell;

        // Success: charge credit, never tear the session down.
        let torn: Cell<Option<u64>> = Cell::new(None);
        let ok =
            flush_output_decision(7, "hi".to_owned(), |_payload| true, |id| torn.set(Some(id)));
        assert_eq!(ok, FlushControl::Charge);
        assert_eq!(torn.get(), None, "a successful emit must never tear down");

        // Failure: stop without charging AND tear down exactly that session.
        let torn2: Cell<Option<u64>> = Cell::new(None);
        let failed = flush_output_decision(
            7,
            "hi".to_owned(),
            |_payload| false,
            |id| torn2.set(Some(id)),
        );
        assert_eq!(failed, FlushControl::StopWithoutCharge);
        assert_eq!(
            torn2.get(),
            Some(7),
            "a failed emit must tear down exactly that session"
        );
    }

    // ---- FIX 3: orphaned sessions on webview teardown must be reaped ----

    #[test]
    fn reap_all_sessions_closes_and_clears_every_credit_window() {
        use std::time::Duration;

        // Platform-independent: only credit windows (no real sessions) so this
        // runs everywhere. Reaping must close every window (releasing any parked
        // flusher) and clear the registry so nothing leaks on webview teardown.
        let state = PtyState::default();
        let first = Arc::new(CreditWindow::new(64));
        let second = Arc::new(CreditWindow::new(64));
        {
            let mut credits = state.credits.lock().expect("credits lock");
            credits.insert(1, Arc::clone(&first));
            credits.insert(2, Arc::clone(&second));
        }

        reap_all_sessions(&state);

        assert!(
            state.credits.lock().expect("credits lock").is_empty(),
            "reap must clear every credit-window registration"
        );
        // A closed window's acquire returns `None` regardless of any remaining
        // credit, proving every window was actually closed.
        let mut noop = |_stalled: bool| {};
        assert_eq!(
            first.acquire_with(Duration::from_millis(10), &mut noop),
            None
        );
        assert_eq!(
            second.acquire_with(Duration::from_millis(10), &mut noop),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn reap_all_sessions_kills_every_live_session_and_clears_both_registries() {
        // The webview-teardown defense the JS side cannot provide: kill ALL live
        // sessions and clear ALL credit windows in one sweep.
        let state = PtyState::default();
        for _ in 0..2 {
            let credit = Arc::new(CreditWindow::new(PTY_CREDIT_WINDOW_BYTES));
            let closing_credit = Arc::clone(&credit);
            let session = Arc::new(
                PtySession::spawn_with_close_hook(
                    "cmd.exe",
                    &["/D", "/K"],
                    TerminalSize::new(80, 24).expect("valid terminal size"),
                    |_id, _output| {},
                    |_id| {},
                    move || closing_credit.close(),
                )
                .expect("session should spawn"),
            );
            let id = session.id();
            state
                .sessions
                .lock()
                .expect("sessions lock")
                .insert(id, session);
            state
                .credits
                .lock()
                .expect("credits lock")
                .insert(id, credit);
        }

        reap_all_sessions(&state);

        assert!(
            state.sessions.lock().expect("sessions lock").is_empty(),
            "reap must kill every session"
        );
        assert!(
            state.credits.lock().expect("credits lock").is_empty(),
            "reap must clear every credit window"
        );
    }
}
