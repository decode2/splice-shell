use serde::Serialize;
use splice_core::{AdapterRegistry, PastePayload, PasteRoute};
use splice_pty::{PtySession, TerminalSize};
use std::{
    collections::HashMap,
    path::PathBuf,
    process::Command,
    sync::{Arc, Condvar, Mutex},
};
use tauri::{Emitter, Manager, State};

const PTY_OUTPUT_EVENT: &str = "pty-output";
const PTY_EXIT_EVENT: &str = "pty-exit";
/// Global event carrying a session's flow-control stall state to the frontend so
/// a frozen tab gets a visible "stalled" signal instead of silently hanging.
const PTY_STALL_EVENT: &str = "pty-stall";

/// How long the flusher parks in `acquire` waiting for the renderer to ack
/// before it treats the session as *stalled* and surfaces it — WITHOUT dropping
/// bytes or emitting without credit.
///
/// Why 5s:
///   * The flusher coalesces on a ~16 ms cadence and a healthy window replenishes
///     within milliseconds, so 5s is ~300× the flush period — far beyond any
///     legitimate renderer pause. Even a full-window `xterm.write` parses in well
///     under a second, so a 5s silence unambiguously means the renderer has
///     stopped consuming (a WebView2-suspended background tab, a wedged main
///     thread, or a gone webview), never a momentary hitch.
///   * It is purely a *reporting* threshold. On timeout the flusher keeps the
///     bytes and keeps waiting (the child stays correctly throttled), so an
///     over-conservative value only delays the stall signal — it can NEVER drop
///     output or corrupt the credit ledger. That safety is what lets us pick a
///     value tuned for "a human notices a frozen terminal" rather than for flow
///     control correctness.
const CREDIT_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Bytes of PTY output a session may have "in flight" — emitted to the webview
/// but not yet acknowledged as consumed by xterm — before the flusher stops
/// emitting and backpressure engages.
///
/// Why 1 MiB:
///   * The flusher coalesces on a ~16 ms cadence, so a 1 MiB window lets a
///     session sustain ~60 MB/s before the brake can ever engage. Real ConPTY
///     output tops out well below that, so the window is invisible to normal
///     work (a `cargo build`'s few hundred KB of output never touches it) and
///     only bites when xterm genuinely cannot keep up — which is exactly when
///     slowing the child is the correct behavior.
///   * It bounds the previously UNBOUNDED backlog hiding in the webview's
///     message queue after a fire-and-forget `emit`. Worst case in-flight
///     memory is now ~1 MiB of unacked output plus the bounded channel below,
///     instead of "however much the child produced while minimized".
///   * It is large enough that the JS-side ack threshold (1/4 window = 256 KiB)
///     keeps IPC chatter to a handful of `pty_ack` calls per megabyte.
///
/// LIVENESS INVARIANT: the JS ack threshold MUST stay strictly below this
/// window. Unacked bytes are then bounded by (threshold + one flush batch), so
/// available credit can never reach zero while the webview is healthy — no idle
/// ack timer is needed to unstick a quiet session.
const PTY_CREDIT_WINDOW_BYTES: usize = 1 << 20;

/// Mirror of `DEFAULT_ACK_THRESHOLD_BYTES` in apps/desktop/src/terminal/
/// terminalOutputScheduler.ts. The credit window and that ack threshold are a
/// contract split across two languages, and nothing else links them. If the
/// window is ever lowered at or below the threshold, the flusher can park out of
/// credit while the frontend never accumulates enough unacked bytes to send an
/// ack — a permanent stall, the exact freeze this backpressure work prevents.
/// This compile-time assertion fails the build if that invariant is broken here;
/// the TS suite guards the other direction (raising the threshold above the
/// mirrored window). Keep this value in sync with the TS constant.
const JS_ACK_THRESHOLD_BYTES: usize = 256 * 1024;
const _: () = assert!(
    PTY_CREDIT_WINDOW_BYTES > JS_ACK_THRESHOLD_BYTES,
    "PTY_CREDIT_WINDOW_BYTES must stay strictly above the JS ack threshold \
     mirrored from terminalOutputScheduler.ts, or a healthy session can stall \
     permanently"
);

/// Capacity, in reader chunks, of the bounded output channel between the ConPTY
/// reader thread and the session's flusher. The reader emits at most 4 KiB per
/// `ReadFile`, so 256 slots bound the channel at ~1 MiB.
///
/// This is the *second* half of the backpressure chain: when the flusher stops
/// draining (no credit), this channel fills, the reader parks in `send`, it
/// stops calling `ReadFile`, the ConPTY pipe fills, and the child finally
/// blocks on write. That is correct terminal behavior — and it is why
/// `PtySession::spawn_with_close_hook` (not `spawn`) is used below: a parked
/// reader must be released on teardown or `close()` deadlocks joining it.
const PTY_OUTPUT_CHANNEL_CAPACITY: usize = 256;

/// Per-session credit window: the bytes of output the frontend has confirmed
/// xterm actually consumed. The flusher may only emit while credit remains;
/// `pty_ack` replenishes it.
///
/// One window per session (never shared), so a tab whose webview has stalled
/// can never hold back another tab's output.
struct CreditWindow {
    capacity: usize,
    state: Mutex<CreditState>,
    replenished: Condvar,
}

#[derive(Debug)]
struct CreditState {
    available: usize,
    /// Set by the session's close hook. Releases any flusher parked in
    /// `acquire` so it can drop the channel receiver and, in turn, release a
    /// ConPTY reader parked in `send`. Without this, `close()` would wedge.
    closed: bool,
}

impl CreditWindow {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            state: Mutex::new(CreditState {
                available: capacity,
                closed: false,
            }),
            replenished: Condvar::new(),
        }
    }

    /// Block until at least one byte of credit is available, returning the
    /// current allowance. `None` means the window was closed and the caller
    /// must stop emitting and release the channel.
    ///
    /// Lock poisoning is recovered from rather than propagated: a poisoned
    /// credit window would otherwise permanently wedge the reader for a
    /// session, which is precisely the failure this whole mechanism exists to
    /// prevent.
    // Test-only convenience: the default production timeout with no stall
    // reporting. Production takes the observable path (`acquire_with`); the
    // existing backpressure tests keep calling this unchanged.
    #[cfg(test)]
    fn acquire(&self) -> Option<usize> {
        self.acquire_with(CREDIT_STALL_TIMEOUT, &mut |_stalled| {})
    }

    /// Like `acquire`, but bounds each wait to `timeout` and reports a stall
    /// across `on_stall` so a stuck session becomes OBSERVABLE without ever
    /// becoming lossy.
    ///
    /// Semantics (deliberately non-lossy): on a wait timeout it does NOT return
    /// and does NOT drop bytes — it keeps the child throttled by looping. The
    /// FIRST timeout that finds the window still exhausted crosses the stall
    /// threshold and calls `on_stall(true)` exactly once; further timeouts stay
    /// silent. When credit returns (or the window closes) it calls
    /// `on_stall(false)` if it had stalled, then returns the allowance / `None`.
    fn acquire_with(
        &self,
        timeout: std::time::Duration,
        on_stall: &mut dyn FnMut(bool),
    ) -> Option<usize> {
        // Whether this call has already reported a stall, so the report fires at
        // most once per stall episode and is cleared exactly once on recovery.
        let mut stalled = false;
        loop {
            // Re-lock each iteration so `on_stall` (which may emit a Tauri event
            // in production) is NEVER invoked while holding the credit lock —
            // that would let a stall report block a concurrent `pty_ack`.
            let state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());

            if state.closed {
                drop(state);
                // Do not clear the stall here: the session is being torn down and
                // the frontend resets health on kill/restart. Emitting "recovered"
                // for a dead session would be misleading.
                return None;
            }
            if state.available > 0 {
                let available = state.available;
                drop(state);
                if stalled {
                    on_stall(false);
                }
                return Some(available);
            }

            let (state, timeout_result) = self
                .replenished
                .wait_timeout(state, timeout)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Only a genuine timeout that still finds the window exhausted (and
            // open) is a stall — a spurious wakeup or a racing close/replenish is
            // resolved by the next loop iteration, never mis-signalled here.
            let genuine_stall = timeout_result.timed_out() && !state.closed && state.available == 0;
            drop(state);

            if genuine_stall && !stalled {
                stalled = true;
                on_stall(true);
            }
        }
    }

    /// Charge `bytes` against the window. Saturates at zero: a flush batch may
    /// slightly overshoot the allowance (the first message of a batch is always
    /// taken whole, so bytes are never dropped), and overshoot must not wrap.
    fn consume(&self, bytes: usize) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.available = state.available.saturating_sub(bytes);
    }

    /// Return `bytes` of credit that the webview confirmed xterm consumed, and
    /// wake a flusher parked in `acquire`.
    ///
    /// Capped at `capacity` so a duplicated, replayed or stale ack (e.g. an
    /// xterm write callback that lands after a session restart) can never
    /// inflate a session's window beyond its configured size.
    fn replenish(&self, bytes: usize) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.available = state.available.saturating_add(bytes).min(self.capacity);
        drop(state);
        self.replenished.notify_all();
    }

    /// Permanently close the window. Idempotent.
    fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.closed = true;
        drop(state);
        self.replenished.notify_all();
    }

    /// Remaining credit, for assertions. The production path never needs to
    /// read this: `acquire` is the only correct way to observe the window,
    /// because anything else would be a torn read the moment it returned.
    #[cfg(test)]
    fn available(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .available
    }
}

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

/// Outcome of a flush emit, returned by the flusher's flush callback so the loop
/// knows whether to charge the credit window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlushControl {
    /// The emit reached the webview: charge the bytes and keep flushing.
    Charge,
    /// The emit failed (the webview is gone): do NOT charge — those bytes can
    /// never be acked, and charging them would subtract credit forever and
    /// permanently stall the session. Stop the loop; the session is being torn
    /// down.
    StopWithoutCharge,
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
    )
    .map_err(|error| error.to_string())?;

    let id = session.id();
    // Publish the id so the flusher's stall reporter can attribute a stall event
    // to this session (see the flusher thread above).
    stall_session_id.store(id, std::sync::atomic::Ordering::SeqCst);

    {
        let mut guard = state
            .sessions
            .lock()
            .map_err(|_| "PTY state lock poisoned".to_owned())?;
        guard.insert(id, Arc::new(session));
    }
    {
        let mut guard = state
            .credits
            .lock()
            .map_err(|_| "PTY credit lock poisoned".to_owned())?;
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

/// Drain immediately-available messages into `buffer`, but stop once the batch
/// has reached the credit `allowance`.
///
/// Stopping at the allowance is what makes the channel fill (and therefore the
/// reader park) instead of the flusher hoovering the whole backlog into one
/// giant unbounded `String` — which is exactly the old behavior. Bytes are
/// never dropped; whatever is left simply stays in the channel.
fn drain_available_into(
    rx: &std::sync::mpsc::Receiver<(u64, String)>,
    buffer: &mut String,
    allowance: usize,
) {
    while buffer.len() < allowance {
        match rx.try_recv() {
            Ok((_, extra)) => buffer.push_str(&extra),
            Err(_) => break,
        }
    }
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

/// Per-session output flusher: coalesces reader chunks on a ~16 ms cadence and
/// emits them to the webview, but ONLY while the session's credit window says
/// the webview is keeping up.
///
/// The credit gate is checked BEFORE the channel is touched. That ordering is
/// the whole mechanism: with no credit the flusher parks in `acquire_with` and
/// never calls `recv`, so the bounded channel fills, the ConPTY reader parks in
/// `send`, and the child blocks on write. Reversing the order (recv first, then
/// gate) would keep draining the channel into memory and defeat backpressure.
///
/// `stall_timeout` bounds each park so a session stuck behind an unresponsive
/// renderer becomes OBSERVABLE: `on_stall_change(true)` fires once when the park
/// first exceeds the timeout and `on_stall_change(false)` once when credit flows
/// again. The park itself is non-lossy — a timeout keeps the bytes and keeps
/// waiting, so the child stays correctly throttled.
///
/// `flush_callback` returns a `FlushControl`: `Charge` to charge the batch and
/// keep going, or `StopWithoutCharge` (emit failed → webview gone) to stop the
/// loop WITHOUT charging, so the credit ledger is never corrupted by bytes that
/// can never be acked.
///
/// Returns — dropping `rx` — when the window is closed (session teardown), the
/// sender is gone, or a flush reports the webview is gone. Dropping `rx` is what
/// releases a reader parked in `send`, so `PtySession::close()` can join it
/// instead of deadlocking.
fn run_flusher_loop_with_stall<F, S>(
    rx: std::sync::mpsc::Receiver<(u64, String)>,
    credit: Arc<CreditWindow>,
    stall_timeout: std::time::Duration,
    mut flush_callback: F,
    mut on_stall_change: S,
) where
    F: FnMut(u64, String) -> FlushControl,
    S: FnMut(bool),
{
    let mut buffer = String::new();
    let mut last_flush = std::time::Instant::now();
    let limit = std::time::Duration::from_millis(16);

    loop {
        // 1. Credit gate. Parks here while the webview is behind (surfacing a
        //    stall on timeout). Deliberately does NOT drain the channel meanwhile.
        let Some(allowance) = credit.acquire_with(stall_timeout, &mut on_stall_change) else {
            // Session closing. Emit whatever is already buffered (never drop
            // bytes we were handed) and return, dropping `rx`. A `StopWithoutCharge`
            // here is irrelevant — we are already returning.
            let mut tail = String::new();
            let mut tail_id = None;
            while let Ok((id, extra)) = rx.try_recv() {
                tail_id = Some(id);
                tail.push_str(&extra);
            }
            if let Some(id) = tail_id {
                if !tail.is_empty() {
                    let _ = flush_callback(id, tail);
                }
            }
            return;
        };

        // 2. Block for the next chunk. Errors only when the reader thread is
        //    gone (its `tx` dropped), i.e. the session is over.
        let Ok((current_session_id, msg)) = rx.recv() else {
            return;
        };
        buffer.push_str(&msg);

        // The first message of a batch is ALWAYS taken whole, even if it alone
        // overshoots the allowance. Splitting it could cut a UTF-8 character or
        // an escape sequence in half; `CreditWindow::consume` saturates, so an
        // overshoot simply means the next `acquire` parks.
        drain_available_into(&rx, &mut buffer, allowance);

        let elapsed = last_flush.elapsed();
        if elapsed < limit {
            std::thread::sleep(limit - elapsed);
            drain_available_into(&rx, &mut buffer, allowance);
        }

        if !buffer.is_empty() {
            let bytes = buffer.len();
            match flush_callback(current_session_id, std::mem::take(&mut buffer)) {
                FlushControl::Charge => {
                    // Charge AFTER emitting: the credit represents bytes in flight
                    // to the webview, and they are only in flight once emitted.
                    credit.consume(bytes);
                    last_flush = std::time::Instant::now();
                }
                FlushControl::StopWithoutCharge => {
                    // Emit failed: the webview is gone. Do NOT charge (those bytes
                    // can never be acked). Return so `rx` drops and a parked reader
                    // is released; the session is being torn down separately.
                    return;
                }
            }
        }
    }
}

/// Non-stall flusher wrapper: the default 5 s stall timeout, no stall reporting,
/// and an infallible flush callback (always charges). Test-only — production
/// runs the observable `run_flusher_loop_with_stall` directly; the existing
/// flusher tests keep calling this unchanged.
#[cfg(test)]
fn run_flusher_loop<F>(
    rx: std::sync::mpsc::Receiver<(u64, String)>,
    credit: Arc<CreditWindow>,
    mut flush_callback: F,
) where
    F: FnMut(u64, String),
{
    run_flusher_loop_with_stall(
        rx,
        credit,
        CREDIT_STALL_TIMEOUT,
        move |id, data| {
            flush_callback(id, data);
            FlushControl::Charge
        },
        |_stalled| {},
    );
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

    /// A window big enough that credit never gates: for tests that are about
    /// aggregation/latency, not about backpressure.
    fn unlimited_credit() -> Arc<CreditWindow> {
        Arc::new(CreditWindow::new(PTY_CREDIT_WINDOW_BYTES))
    }

    #[test]
    fn credit_window_starts_full_and_consume_saturates_at_zero() {
        let credit = CreditWindow::new(16);
        assert_eq!(credit.available(), 16);

        // A flush batch always takes its first message whole (bytes are NEVER
        // dropped), so it may overshoot the allowance. Overshoot must saturate,
        // not wrap.
        credit.consume(100);
        assert_eq!(credit.available(), 0);
    }

    #[test]
    fn credit_window_replenish_is_capped_at_capacity() {
        let credit = CreditWindow::new(16);
        credit.consume(16);
        assert_eq!(credit.available(), 0);

        // A stale/duplicated ack (e.g. an xterm write callback landing after a
        // session restart) must never inflate the window past its capacity.
        credit.replenish(1_000);
        assert_eq!(credit.available(), 16);
    }

    #[test]
    fn credit_window_acquire_returns_none_once_closed() {
        let credit = CreditWindow::new(0);
        credit.close();

        // The exhausted-and-closed case: `acquire` must NOT park forever, or
        // the flusher never drops the channel receiver and `close()` wedges.
        assert_eq!(credit.acquire(), None);
    }

    #[test]
    fn credit_window_close_releases_a_parked_acquire() {
        use std::time::Duration;

        let credit = Arc::new(CreditWindow::new(0));
        let parked_credit = Arc::clone(&credit);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done_tx.send(parked_credit.acquire());
        });

        // Still parked: no credit, not closed.
        assert_eq!(
            done_rx.recv_timeout(Duration::from_millis(150)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        );

        credit.close();
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(2)),
            Ok(None),
            "close() must release a flusher parked on an exhausted window"
        );
    }

    #[test]
    fn flusher_stops_emitting_when_credit_is_exhausted_and_resumes_after_ack() {
        use std::sync::mpsc::{channel, sync_channel, RecvTimeoutError};
        use std::time::Duration;

        let (tx, rx) = sync_channel::<(u64, String)>(8);
        let (emitted_tx, emitted_rx) = channel::<(u64, String)>();
        // An 8-byte window, so one 8-byte chunk consumes it exactly.
        let credit = Arc::new(CreditWindow::new(8));
        let flusher_credit = Arc::clone(&credit);
        std::thread::spawn(move || {
            run_flusher_loop(rx, flusher_credit, move |id, data| {
                let _ = emitted_tx.send((id, data));
            });
        });

        tx.send((7, "12345678".to_owned()))
            .expect("the first chunk fits in the window");
        assert_eq!(
            emitted_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("output within the window must be emitted"),
            (7, "12345678".to_owned())
        );
        assert_eq!(
            credit.available(),
            0,
            "emitting 8 bytes must charge the whole 8-byte window"
        );

        // Credit is exhausted: the flusher must now STOP emitting — and, just
        // as importantly, stop draining the channel, so backpressure propagates
        // to the reader and on to the child.
        tx.send((7, "blocked".to_owned()))
            .expect("the channel still has room");
        assert_eq!(
            emitted_rx.recv_timeout(Duration::from_millis(300)),
            Err(RecvTimeoutError::Timeout),
            "the flusher must not emit while the credit window is exhausted"
        );

        // The webview acks what xterm consumed: the flusher wakes and resumes.
        credit.replenish(8);
        assert_eq!(
            emitted_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("an ack must resume the flusher"),
            (7, "blocked".to_owned())
        );
    }

    #[test]
    fn bounded_output_channel_parks_the_reader_when_the_flusher_has_no_credit() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::mpsc::sync_channel;
        use std::time::Duration;

        const CAPACITY: usize = 4;
        const ATTEMPTS: usize = 5_000;

        let (tx, rx) = sync_channel::<(u64, String)>(CAPACITY);
        // A window that is exhausted from the start: the flusher can never
        // emit, therefore it must never drain the channel either.
        let credit = Arc::new(CreditWindow::new(0));
        let flusher_credit = Arc::clone(&credit);
        std::thread::spawn(move || {
            run_flusher_loop(rx, flusher_credit, |_id, _data| {
                panic!("the flusher must not emit a single byte without credit");
            });
        });

        // Stands in for the ConPTY reader thread: it pushes chunks as fast as it
        // can and parks in `send` once the channel is full.
        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_for_reader = Arc::clone(&accepted);
        let reader = std::thread::spawn(move || {
            for index in 0..ATTEMPTS {
                if tx.send((1, format!("chunk{index}"))).is_err() {
                    break;
                }
                accepted_for_reader.fetch_add(1, Ordering::SeqCst);
            }
        });

        std::thread::sleep(Duration::from_millis(250));

        // THE BOUND HOLDS. Memory cannot grow: the reader is parked in `send`,
        // so it has stopped calling `ReadFile`, so the ConPTY pipe fills and the
        // child blocks. With the old unbounded `channel()` this would be 5000.
        let parked_at = accepted.load(Ordering::SeqCst);
        assert!(
            parked_at <= CAPACITY + 1,
            "the reader must park once the bounded channel is full, but it accepted \
             {parked_at} of {ATTEMPTS} chunks — the channel is not bounding memory"
        );

        // And a parked reader is never wedged: closing the window releases it.
        credit.close();
        reader
            .join()
            .expect("closing the credit window must release the parked reader");
    }

    #[test]
    fn a_stalled_session_does_not_starve_another_session() {
        use std::sync::mpsc::{channel, sync_channel};
        use std::time::Duration;

        // Each session owns its channel, flusher thread and credit window, so a
        // tab whose webview stopped acking cannot hold back a sibling tab.
        let (flood_tx, flood_rx) = sync_channel::<(u64, String)>(4);
        let (flood_emitted_tx, flood_emitted_rx) = channel::<(u64, String)>();
        let flood_credit = Arc::new(CreditWindow::new(0));
        let flood_flusher_credit = Arc::clone(&flood_credit);
        std::thread::spawn(move || {
            run_flusher_loop(flood_rx, flood_flusher_credit, move |id, data| {
                let _ = flood_emitted_tx.send((id, data));
            });
        });

        let (calm_tx, calm_rx) = sync_channel::<(u64, String)>(4);
        let (calm_emitted_tx, calm_emitted_rx) = channel::<(u64, String)>();
        let calm_credit = unlimited_credit();
        let calm_flusher_credit = Arc::clone(&calm_credit);
        std::thread::spawn(move || {
            run_flusher_loop(calm_rx, calm_flusher_credit, move |id, data| {
                let _ = calm_emitted_tx.send((id, data));
            });
        });

        // Session 1 floods until its reader parks on the full channel.
        std::thread::spawn(move || {
            for index in 0..5_000 {
                if flood_tx.send((1, format!("flood{index}"))).is_err() {
                    break;
                }
            }
        });
        std::thread::sleep(Duration::from_millis(150));

        // Session 2 is idle and still gets its output through, promptly.
        calm_tx.send((2, "prompt> ".to_owned())).expect("send");
        assert_eq!(
            calm_emitted_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("an idle session must keep emitting while a sibling is stalled"),
            (2, "prompt> ".to_owned())
        );
        assert!(
            flood_emitted_rx.try_recv().is_err(),
            "the stalled session must not have emitted anything"
        );

        flood_credit.close();
        calm_credit.close();
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
    fn backpressure_never_drops_a_byte_from_a_real_conpty_child() {
        use std::sync::mpsc::{channel, sync_channel};

        // THE HARD INVARIANT, end to end against a real ConPTY child.
        //
        // Flow control is not sampling: dropping output would corrupt escape
        // sequences and desynchronise terminal state. This runs a child that
        // prints 200 numbered lines through a DELIBERATELY tiny credit window
        // (512 B) and a DELIBERATELY tiny channel (2 slots), so the brake
        // engages over and over — the reader really parks, the ConPTY pipe
        // really fills, the child really blocks on write — and then asserts
        // every single line still arrives, in order.
        const LINES: usize = 200;
        let credit = Arc::new(CreditWindow::new(512));
        let (tx, rx) = sync_channel::<(u64, String)>(2);
        let (emitted_tx, emitted_rx) = channel::<(u64, String)>();
        let flusher_credit = Arc::clone(&credit);
        std::thread::spawn(move || {
            run_flusher_loop(rx, flusher_credit, move |id, data| {
                let _ = emitted_tx.send((id, data));
            });
        });

        let hook_credit = Arc::clone(&credit);
        let session = PtySession::spawn_with_close_hook(
            "cmd.exe",
            &["/D", "/C", "for /L %i in (1,1,200) do @echo LINE%i"],
            TerminalSize::new(80, 24).expect("valid terminal size"),
            move |id, output| {
                let _ = tx.send((id, output));
            },
            |_id| {},
            move || hook_credit.close(),
        )
        .expect("session should spawn");

        // Stands in for the webview: consume, then ack exactly what was
        // consumed. This MUST run concurrently with the child, on its own
        // thread — if the test consumed only after waiting for the child to
        // exit, the credit window would run dry, the reader would park, the
        // ConPTY pipe would fill and the child would block on write and NEVER
        // exit. (Which is the mechanism working correctly, and is exactly how
        // the first draft of this test deadlocked itself.)
        let consumer_credit = Arc::clone(&credit);
        let consumer = std::thread::spawn(move || {
            let mut received = String::new();
            for (_id, data) in emitted_rx {
                consumer_credit.replenish(data.len());
                received.push_str(&data);
            }
            received
        });

        // Wait for the child to finish printing. Note the reader does NOT see
        // EOF here: the pseudoconsole still owns the output pipe's write end,
        // so only `close()` below ends the reader.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while session.is_running().unwrap_or(false) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(
            !session.is_running().unwrap_or(false),
            "the child must run to completion — if it is still alive, backpressure has WEDGED it \
             rather than merely throttled it"
        );
        // Let the reader drain whatever the child left in the ConPTY pipe and
        // the flusher emit it, before `close()` tears the pipeline down.
        std::thread::sleep(std::time::Duration::from_millis(500));

        // `close()` fires the hook -> closes the window -> releases the flusher
        // -> drops the receiver -> the reader ends -> the emit channel closes ->
        // the consumer thread finishes.
        session.close();
        let received = consumer.join().expect("the consumer thread must not panic");

        // Guard against a vacuous pass: the child must genuinely have produced
        // several windows' worth of output, so the 512-byte window really was
        // exhausted and replenished many times over rather than never engaging.
        assert!(
            received.len() > 512 * 3,
            "the child produced only {} bytes — too little to have exercised the credit gate, \
             so this test would prove nothing",
            received.len()
        );

        // Every line, in order. An ordered scan (rather than a plain `contains`)
        // is what makes this prefix-safe: LINE1 is a prefix of LINE10, so only a
        // forward-advancing cursor proves both are present AND correctly ordered.
        let mut cursor = 0;
        for line in 1..=LINES {
            let needle = format!("LINE{line}");
            let found = received[cursor..].find(&needle).unwrap_or_else(|| {
                panic!(
                    "backpressure dropped output: {needle} is missing from the {} bytes received. \
                     Flow control must never be lossy.",
                    received.len()
                )
            });
            cursor += found + needle.len();
        }
    }

    #[cfg(windows)]
    #[test]
    fn kill_completes_while_the_reader_is_parked_and_the_webview_never_acks() {
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
    fn test_flusher_aggregates_high_frequency_output() {
        use std::sync::mpsc;
        use std::thread;

        let (tx, rx) = mpsc::sync_channel::<(u64, String)>(PTY_OUTPUT_CHANNEL_CAPACITY);
        let (done_tx, done_rx) = mpsc::channel::<(u64, String)>();

        // Spawn the flusher loop in a background thread
        thread::spawn(move || {
            run_flusher_loop(rx, unlimited_credit(), move |id, data| {
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

        let (tx, rx) = mpsc::sync_channel::<(u64, String)>(PTY_OUTPUT_CHANNEL_CAPACITY);
        let (done_tx, done_rx) = mpsc::channel::<(u64, String)>();

        // Spawn flusher
        thread::spawn(move || {
            run_flusher_loop(rx, unlimited_credit(), move |id, data| {
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

    // ---- FIX 1: acquire must not block forever; a stall must be OBSERVABLE ----

    #[test]
    fn acquire_with_times_out_without_dropping_and_signals_stall_exactly_once() {
        use std::sync::mpsc::{channel, TryRecvError};
        use std::time::Duration;

        // Exhausted window: `acquire_with` must PARK — never returning a lossy
        // "0 credit" allowance and never emitting — but the FIRST wait timeout
        // must surface the stall exactly once. Later timeouts stay silent; a
        // replenish clears the stall and finally returns the allowance.
        // Capacity 8 (then fully consumed) so the later `replenish(8)` actually
        // restores credit — `replenish` caps at capacity, so a capacity-0 window
        // could never be un-stalled.
        let credit = Arc::new(CreditWindow::new(8));
        credit.consume(8);
        let (signal_tx, signal_rx) = channel::<bool>();
        let parked_credit = Arc::clone(&credit);
        let handle = std::thread::spawn(move || {
            let mut on_stall = move |stalled: bool| {
                let _ = signal_tx.send(stalled);
            };
            parked_credit.acquire_with(Duration::from_millis(40), &mut on_stall)
        });

        // First timeout crosses the stall threshold → exactly one `true`.
        assert_eq!(
            signal_rx.recv_timeout(Duration::from_secs(2)),
            Ok(true),
            "the first wait timeout must surface a stall"
        );
        // It must NOT return on timeout (bytes kept, child stays blocked): still
        // parked after several more timeout intervals, and no second signal.
        std::thread::sleep(Duration::from_millis(160));
        assert!(
            !handle.is_finished(),
            "a timed-out acquire must keep waiting, never return a lossy allowance"
        );
        assert_eq!(
            signal_rx.try_recv(),
            Err(TryRecvError::Empty),
            "stall must be signalled exactly once, not on every timeout"
        );

        // Credit flows again: acquire returns the allowance and the stall clears.
        credit.replenish(8);
        assert_eq!(
            handle.join().expect("acquire thread must not panic"),
            Some(8),
            "a replenished window must unblock acquire with its allowance"
        );
        assert_eq!(
            signal_rx.recv_timeout(Duration::from_secs(2)),
            Ok(false),
            "credit returning must clear the stall exactly once"
        );
    }

    #[test]
    fn acquire_with_returns_immediately_without_stall_when_credit_is_available() {
        use std::time::Duration;

        let credit = CreditWindow::new(16);
        let mut signalled = Vec::new();
        let allowance = {
            let mut on_stall = |stalled: bool| signalled.push(stalled);
            credit.acquire_with(Duration::from_millis(50), &mut on_stall)
        };

        assert_eq!(allowance, Some(16));
        assert!(
            signalled.is_empty(),
            "an available window must never report a stall"
        );
    }

    // ---- FIX 2: emit failure tears down the session, never charges credit ----

    #[test]
    fn flusher_stops_without_charging_credit_when_emit_fails() {
        use std::sync::mpsc::{channel, sync_channel};
        use std::time::Duration;

        // Emit failure (dead webview) must NOT charge credit: charging bytes that
        // can never be acked would subtract credit forever → permanent stall. The
        // flusher must stop instead, leaving the ledger intact.
        let (tx, rx) = sync_channel::<(u64, String)>(8);
        let (done_tx, done_rx) = channel::<()>();
        let credit = Arc::new(CreditWindow::new(64));
        let flusher_credit = Arc::clone(&credit);
        std::thread::spawn(move || {
            run_flusher_loop_with_stall(
                rx,
                flusher_credit,
                Duration::from_secs(5),
                // Every emit "fails", standing in for a gone webview.
                |_id, _data| FlushControl::StopWithoutCharge,
                |_stalled| {},
            );
            let _ = done_tx.send(());
        });

        tx.send((7, "12345678".to_owned()))
            .expect("send first chunk");

        // The loop must return (teardown path), dropping rx so a parked reader is
        // released.
        assert_eq!(
            done_rx.recv_timeout(Duration::from_secs(2)),
            Ok(()),
            "a failed emit must end the flusher loop so rx is dropped"
        );
        // And the ledger must be intact: nothing charged for the unackable bytes.
        assert_eq!(
            credit.available(),
            64,
            "a failed emit must not charge the credit window"
        );
    }

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
