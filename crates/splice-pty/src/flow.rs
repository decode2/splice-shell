//! Credit-based PTY output flow control.
//!
//! This is the pure, Tauri-independent half of the terminal output backpressure
//! chain (child -> ConPTY pipe -> reader -> channel -> flusher -> emit -> xterm
//! -> ack -> credit). It is `std::sync`/`std::mpsc` only and carries no
//! dependency on the host application or its transport: the emit and stall
//! signal are supplied by the caller as closures. The host application
//! (`apps/desktop`) wires the Tauri `app.emit(...)` calls into those closures.
//!
//! Platform-independent by construction, so it lives OUTSIDE the crate's
//! `#[cfg(windows)]` ConPTY module and builds on every platform the crate does.

use std::sync::{Arc, Condvar, Mutex};

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
pub const CREDIT_STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
pub const PTY_CREDIT_WINDOW_BYTES: usize = 1 << 20;

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
pub const PTY_OUTPUT_CHANNEL_CAPACITY: usize = 256;

/// Per-session credit window: the bytes of output the frontend has confirmed
/// xterm actually consumed. The flusher may only emit while credit remains;
/// `pty_ack` replenishes it.
///
/// One window per session (never shared), so a tab whose webview has stalled
/// can never hold back another tab's output.
pub struct CreditWindow {
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
    pub fn new(capacity: usize) -> Self {
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
    pub fn acquire_with(
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
    pub fn consume(&self, bytes: usize) {
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
    pub fn replenish(&self, bytes: usize) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.available = state.available.saturating_add(bytes).min(self.capacity);
        drop(state);
        self.replenished.notify_all();
    }

    /// Permanently close the window. Idempotent.
    pub fn close(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.closed = true;
        drop(state);
        self.replenished.notify_all();
    }

    /// Remaining credit, for cross-crate test assertions only. Not part of the
    /// public API — `#[doc(hidden)]` because it is `pub` solely so the desktop
    /// crate's tests can reach it (`#[cfg(test)]` does not cross the crate
    /// boundary). Production code must never read this: `acquire` is the only
    /// correct way to observe the window, since anything else is a torn read the
    /// moment it returns.
    #[doc(hidden)]
    pub fn available(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .available
    }
}

/// Outcome of a flush emit, returned by the flusher's flush callback so the loop
/// knows whether to charge the credit window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushControl {
    /// The emit reached the webview: charge the bytes and keep flushing.
    Charge,
    /// The emit failed (the webview is gone): do NOT charge — those bytes can
    /// never be acked, and charging them would subtract credit forever and
    /// permanently stall the session. Stop the loop; the session is being torn
    /// down.
    StopWithoutCharge,
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
pub fn run_flusher_loop_with_stall<F, S>(
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
/// and an infallible flush callback (always charges).
///
/// NOT part of the public API and must not be used in production — it silently
/// drops the stall observability and emit-failure teardown that
/// `run_flusher_loop_with_stall` provides, which would reintroduce the
/// silent-hang class this pipeline was built to prevent. It is `pub` (and
/// `#[doc(hidden)]`) solely so the desktop crate's tests can call it across the
/// crate boundary, where `#[cfg(test)]` does not reach. Production wires
/// `run_flusher_loop_with_stall` directly.
#[doc(hidden)]
pub fn run_flusher_loop<F>(
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[cfg(windows)]
    #[test]
    fn backpressure_never_drops_a_byte_from_a_real_conpty_child() {
        use crate::{PtySession, TerminalSize};
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
}
