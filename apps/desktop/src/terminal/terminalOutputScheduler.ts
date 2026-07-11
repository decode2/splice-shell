export type TerminalOutputScheduler = {
  /**
   * Queue `chunk` for xterm.
   *
   * `bytes` is the UTF-8 byte cost the backend charged against this session's
   * credit window for the PTY payload this chunk came from — NOT the length of
   * `chunk` itself. They differ: the output filter can rewrite, coalesce or
   * hold back what it emits, but flow control accounts for what was *received*.
   * Pass `0` for chunks the frontend synthesised itself (they cost no credit),
   * and `write("", bytes)` for a payload the filter swallowed entirely.
   */
  write: (chunk: string, bytes?: number) => void;
  flush: () => void;
  dispose: () => void;
};

/**
 * How many bytes of consumed output accumulate before an ack is sent back to
 * Rust. Acking every chunk would replace an unbounded buffer with an IPC storm,
 * which just moves the problem.
 *
 * LIVENESS INVARIANT (mirrored by `PTY_CREDIT_WINDOW_BYTES` in lib.rs): this
 * MUST stay strictly below the backend's 1 MiB credit window. Unacked bytes are
 * then bounded by (threshold + one flush batch), so available credit can never
 * reach zero while the webview is healthy — which is why no idle-ack timer is
 * needed to unstick a quiet session.
 */
export const DEFAULT_ACK_THRESHOLD_BYTES = 256 * 1024;

/**
 * Cadence of the timer that drains output when `requestAnimationFrame` is not
 * running. Matched to a 60 Hz frame so a hidden window keeps roughly the same
 * throughput as a visible one.
 */
export const DEFAULT_DRAIN_INTERVAL_MS = 16;

type TerminalOutputSchedulerOptions = {
  /**
   * Hands a coalesced chunk to xterm. MUST invoke `consumed` once xterm has
   * actually parsed it — i.e. `terminal.write(chunk, consumed)`. Reporting
   * consumption at hand-over time instead would return credit for bytes still
   * sitting in xterm's parser queue, recreating the very backlog this exists to
   * remove.
   */
  write: (chunk: string, consumed: () => void) => void;
  /** Returns `bytes` of consumed output to the backend's credit window. */
  ack?: (bytes: number) => void;
  ackThresholdBytes?: number;
  requestFrame?: (callback: FrameRequestCallback) => number;
  cancelFrame?: (handle: number) => void;
  setTimer?: (callback: () => void, ms: number) => number;
  clearTimer?: (handle: number) => void;
  drainIntervalMs?: number;
};

export function createTerminalOutputScheduler({
  ack,
  ackThresholdBytes = DEFAULT_ACK_THRESHOLD_BYTES,
  cancelFrame = (handle) => window.cancelAnimationFrame(handle),
  clearTimer = (handle) => clearTimeout(handle),
  drainIntervalMs = DEFAULT_DRAIN_INTERVAL_MS,
  requestFrame = (callback) => window.requestAnimationFrame(callback),
  setTimer = (callback, ms) => setTimeout(callback, ms) as unknown as number,
  write,
}: TerminalOutputSchedulerOptions): TerminalOutputScheduler {
  let disposed = false;
  let frameHandle: number | undefined;
  let timerHandle: number | undefined;
  let pendingOutput = "";
  let pendingBytes = 0;
  // Bytes xterm has finished parsing but that have not been acked back to Rust
  // yet. Bounded by `ackThresholdBytes` plus the last batch (see the liveness
  // invariant above).
  let unackedBytes = 0;

  const disarm = () => {
    if (frameHandle !== undefined) {
      cancelFrame(frameHandle);
      frameHandle = undefined;
    }
    if (timerHandle !== undefined) {
      clearTimer(timerHandle);
      timerHandle = undefined;
    }
  };

  const recordConsumed = (bytes: number) => {
    if (bytes <= 0) {
      return;
    }

    unackedBytes += bytes;
    if (unackedBytes < ackThresholdBytes) {
      return;
    }

    const settled = unackedBytes;
    unackedBytes = 0;
    ack?.(settled);
  };

  const flush = () => {
    // Whichever clock got here first wins; disarm the other so a window that is
    // restored mid-flight cannot double-flush (and so the loser does not fire a
    // pointless empty flush later).
    disarm();
    if (disposed) {
      return;
    }

    const output = pendingOutput;
    const bytes = pendingBytes;
    pendingOutput = "";
    pendingBytes = 0;

    if (!output) {
      // The filter swallowed the payload whole (e.g. it is holding a partial
      // escape sequence back). Those bytes were still received and still
      // charged against the credit window, so they must still be acked — the
      // holdback itself is tiny and bounded, so counting it as consumed is
      // honest.
      recordConsumed(bytes);
      return;
    }

    write(output, () => recordConsumed(bytes));
  };

  const arm = () => {
    // The animation frame keeps rendering coalesced to the compositor...
    frameHandle ??= requestFrame(flush);
    // ...but WebView2 SUSPENDS requestAnimationFrame while the window is
    // minimized or backgrounded. On rAF alone the pending string would grow
    // without bound while hidden, then a single giant xterm.write() would
    // freeze the main thread on refocus — and, with credit-based flow control,
    // a JS side that stops draining also stops acking, so the credit window
    // would run dry and BLOCK THE CHILD PROCESS. Minimizing the terminal would
    // freeze your build.
    //
    // A real terminal keeps consuming into scrollback when hidden, so a plain
    // timer keeps draining, writing and acking regardless of rAF. Whichever
    // fires first wins (see `flush`), so backpressure now engages only when
    // xterm genuinely cannot keep up — a real CPU limit, and exactly when
    // slowing the child is the right answer.
    timerHandle ??= setTimer(flush, drainIntervalMs);
  };

  return {
    write: (chunk, bytes = 0) => {
      if (disposed || (!chunk && bytes <= 0)) {
        return;
      }

      pendingOutput += chunk;
      pendingBytes += bytes;
      arm();
    },
    flush,
    dispose: () => {
      disposed = true;
      disarm();
      pendingOutput = "";
      pendingBytes = 0;

      // Hand back credit xterm already consumed but that never reached the ack
      // threshold, so a view that is torn down without its session being killed
      // (e.g. a StrictMode remount) cannot leave that session's window short.
      const owed = unackedBytes;
      unackedBytes = 0;
      if (owed > 0) {
        ack?.(owed);
      }
    },
  };
}
