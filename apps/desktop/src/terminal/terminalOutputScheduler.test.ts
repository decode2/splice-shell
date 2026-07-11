import { describe, expect, it, vi } from "vitest";
import {
  createTerminalOutputScheduler,
  DEFAULT_ACK_THRESHOLD_BYTES,
} from "./terminalOutputScheduler";

/// A controllable stand-in for the two clocks the scheduler rides on: the
/// animation frame (render coalescing) and a plain timer (the fallback that
/// keeps a hidden window draining). Nothing fires unless the test says so.
function createClocks() {
  const frames: FrameRequestCallback[] = [];
  const timers: (() => void)[] = [];
  let cancelledFrames = 0;
  let clearedTimers = 0;

  return {
    frames,
    timers,
    get cancelledFrames() {
      return cancelledFrames;
    },
    get clearedTimers() {
      return clearedTimers;
    },
    runFrame: () => frames.shift()?.(0),
    runTimer: () => timers.shift()?.(),
    options: {
      requestFrame: (callback: FrameRequestCallback) => {
        frames.push(callback);
        return frames.length;
      },
      cancelFrame: () => {
        cancelledFrames += 1;
      },
      setTimer: (callback: () => void) => {
        timers.push(callback);
        return timers.length;
      },
      clearTimer: () => {
        clearedTimers += 1;
      },
    },
  };
}

/// Writer that records what xterm was handed but does NOT report consumption
/// until the test calls `settle()` — mirroring `xterm.write(data, callback)`,
/// whose callback only fires once the parser has actually processed the chunk.
function createWriter() {
  const writes: string[] = [];
  const pending: (() => void)[] = [];

  return {
    writes,
    settle: () => {
      while (pending.length > 0) {
        pending.shift()?.();
      }
    },
    write: (chunk: string, consumed: () => void) => {
      writes.push(chunk);
      pending.push(consumed);
    },
  };
}

describe("terminal output scheduler", () => {
  it("coalesces bursty PTY chunks into a single frame write", () => {
    const clocks = createClocks();
    const writer = createWriter();
    const scheduler = createTerminalOutputScheduler({
      ...clocks.options,
      write: writer.write,
    });

    scheduler.write("cod");
    scheduler.write("ex");
    scheduler.write("\x1b[2J");

    expect(writer.writes).toEqual([]);
    expect(clocks.frames).toHaveLength(1);

    clocks.runFrame();

    expect(writer.writes).toEqual(["codex\x1b[2J"]);
  });

  it("flushes pending output immediately when requested", () => {
    const clocks = createClocks();
    const writer = createWriter();
    const scheduler = createTerminalOutputScheduler({
      ...clocks.options,
      write: writer.write,
    });

    scheduler.write("hello");
    scheduler.flush();

    expect(writer.writes).toEqual(["hello"]);
  });

  it("drops pending output after disposal", () => {
    const clocks = createClocks();
    const writer = createWriter();
    const scheduler = createTerminalOutputScheduler({
      ...clocks.options,
      write: writer.write,
    });

    scheduler.write("stale");
    scheduler.dispose();
    scheduler.flush();

    expect(writer.writes).toEqual([]);
  });

  // THE MINIMIZED-WINDOW BUG.
  //
  // WebView2 SUSPENDS requestAnimationFrame while the window is minimized or
  // backgrounded. A scheduler that only ever drains on rAF therefore stops
  // draining entirely: its pending string grows without bound and then a single
  // giant xterm.write() freezes the main thread on refocus.
  //
  // With credit-based flow control the same bug gets strictly worse: a JS side
  // that stops draining also stops acking, so the credit window runs dry and the
  // CHILD PROCESS BLOCKS. Minimizing the terminal would freeze your build.
  //
  // A real terminal keeps consuming into scrollback when hidden, so the
  // scheduler must keep draining, writing and acking on a timer even if rAF
  // never fires at all.
  it("still drains, writes and acks when requestAnimationFrame never fires", () => {
    const clocks = createClocks();
    const writer = createWriter();
    const acks: number[] = [];
    const scheduler = createTerminalOutputScheduler({
      ...clocks.options,
      // rAF is registered but SUSPENDED: the callback is never invoked.
      requestFrame: () => 1,
      write: writer.write,
      ack: (bytes) => acks.push(bytes),
      ackThresholdBytes: 4,
    });

    scheduler.write("hidden", 6);

    // rAF is suspended, so nothing has drained yet...
    expect(writer.writes).toEqual([]);

    // ...but the timer fallback still fires.
    clocks.runTimer();

    expect(writer.writes).toEqual(["hidden"]);

    // And once xterm reports the bytes parsed, they are acked back to Rust, so
    // the credit window is replenished and the child is never throttled by a
    // merely-hidden window.
    writer.settle();
    expect(acks).toEqual([6]);
  });

  it("never double-flushes when the frame and the timer both fire", () => {
    const clocks = createClocks();
    const writer = createWriter();
    const scheduler = createTerminalOutputScheduler({
      ...clocks.options,
      write: writer.write,
    });

    scheduler.write("once");

    // Both clocks are armed; whichever fires first must win, and the loser must
    // become a no-op rather than re-writing (or writing an empty) chunk.
    clocks.runFrame();
    clocks.runTimer();

    expect(writer.writes).toEqual(["once"]);
  });

  it("re-arms both clocks for output that arrives after a flush", () => {
    const clocks = createClocks();
    const writer = createWriter();
    const scheduler = createTerminalOutputScheduler({
      ...clocks.options,
      write: writer.write,
    });

    scheduler.write("first");
    clocks.runTimer();
    scheduler.write("second");
    clocks.runTimer();

    expect(writer.writes).toEqual(["first", "second"]);
  });

  it("acks only after xterm reports the chunk consumed, not when it is handed over", () => {
    const clocks = createClocks();
    const writer = createWriter();
    const acks: number[] = [];
    const scheduler = createTerminalOutputScheduler({
      ...clocks.options,
      write: writer.write,
      ack: (bytes) => acks.push(bytes),
      ackThresholdBytes: 1,
    });

    scheduler.write("slow", 4);
    clocks.runFrame();

    // Handed to xterm, but xterm has not finished parsing it. Acking here would
    // be a lie: it would hand credit back for bytes still queued in the parser,
    // which is exactly the unobservable backlog this design exists to remove.
    expect(writer.writes).toEqual(["slow"]);
    expect(acks).toEqual([]);

    writer.settle();
    expect(acks).toEqual([4]);
  });

  it("coalesces acks instead of sending one per chunk", () => {
    const clocks = createClocks();
    const writer = createWriter();
    const acks: number[] = [];
    const scheduler = createTerminalOutputScheduler({
      ...clocks.options,
      write: writer.write,
      ack: (bytes) => acks.push(bytes),
      ackThresholdBytes: 100,
    });

    // 20 chunks of 10 bytes across 20 separate frames: 200 bytes total. Acking
    // per chunk would be 20 IPC round-trips, which just moves the problem from
    // "unbounded buffer" to "IPC storm".
    for (let index = 0; index < 20; index += 1) {
      scheduler.write("0123456789", 10);
      clocks.runFrame();
      writer.settle();
    }

    expect(writer.writes).toHaveLength(20);
    // 200 bytes consumed, 100-byte threshold => exactly 2 acks.
    expect(acks).toEqual([100, 100]);
  });

  it("acks bytes the filter swallowed entirely, so a holdback cannot starve the window", () => {
    const clocks = createClocks();
    const writer = createWriter();
    const acks: number[] = [];
    const scheduler = createTerminalOutputScheduler({
      ...clocks.options,
      write: writer.write,
      ack: (bytes) => acks.push(bytes),
      ackThresholdBytes: 1,
    });

    // The output filter can hold a partial escape sequence back and emit no
    // action at all. Those bytes were still received and still charged against
    // the credit window, so they must still be acked or the session slowly
    // starves itself to a halt.
    scheduler.write("", 12);
    clocks.runFrame();

    expect(writer.writes).toEqual([]);
    expect(acks).toEqual([12]);
  });

  it("returns credit still owed when disposed mid-flight", () => {
    const clocks = createClocks();
    const writer = createWriter();
    const acks: number[] = [];
    const scheduler = createTerminalOutputScheduler({
      ...clocks.options,
      write: writer.write,
      ack: (bytes) => acks.push(bytes),
      ackThresholdBytes: 1_000,
    });

    scheduler.write("partial", 7);
    clocks.runFrame();
    writer.settle();

    // Below the threshold, so nothing acked yet.
    expect(acks).toEqual([]);

    scheduler.dispose();

    expect(acks).toEqual([7]);
  });

  it("cancels both clocks on disposal", () => {
    const clocks = createClocks();
    const writer = createWriter();
    const scheduler = createTerminalOutputScheduler({
      ...clocks.options,
      write: writer.write,
    });

    scheduler.write("pending");
    scheduler.dispose();

    expect(clocks.cancelledFrames).toBe(1);
    expect(clocks.clearedTimers).toBe(1);
  });

  it("defaults the ack threshold to a quarter of the backend credit window", () => {
    // LIVENESS INVARIANT (mirrored in lib.rs): the threshold must stay strictly
    // below the 1 MiB backend window. Unacked bytes are then bounded by
    // (threshold + one batch), so credit can never hit zero on a healthy
    // webview — which is why no idle-ack timer is needed.
    expect(DEFAULT_ACK_THRESHOLD_BYTES).toBe(256 * 1024);
    expect(DEFAULT_ACK_THRESHOLD_BYTES).toBeLessThan(1024 * 1024);
  });

  it("ignores writes that carry neither output nor byte cost", () => {
    const clocks = createClocks();
    const writer = createWriter();
    const scheduler = createTerminalOutputScheduler({
      ...clocks.options,
      write: writer.write,
      ack: vi.fn(),
    });

    scheduler.write("");

    expect(clocks.frames).toHaveLength(0);
    expect(clocks.timers).toHaveLength(0);
  });
});
