import { describe, expect, it } from "vitest";
import { createTerminalOutputScheduler } from "./terminalOutputScheduler";

describe("terminal output scheduler", () => {
  it("coalesces bursty PTY chunks into a single frame write", () => {
    const writes: string[] = [];
    const frameCallbacks: FrameRequestCallback[] = [];
    const scheduler = createTerminalOutputScheduler({
      write: (chunk) => writes.push(chunk),
      requestFrame: (callback) => {
        frameCallbacks.push(callback);
        return frameCallbacks.length;
      },
      cancelFrame: () => undefined,
    });

    scheduler.write("cod");
    scheduler.write("ex");
    scheduler.write("\x1b[2J");

    expect(writes).toEqual([]);
    expect(frameCallbacks).toHaveLength(1);

    frameCallbacks[0](0);

    expect(writes).toEqual(["codex\x1b[2J"]);
  });

  it("flushes pending output immediately when requested", () => {
    const writes: string[] = [];
    const scheduler = createTerminalOutputScheduler({
      write: (chunk) => writes.push(chunk),
      requestFrame: () => 1,
      cancelFrame: () => undefined,
    });

    scheduler.write("hello");
    scheduler.flush();

    expect(writes).toEqual(["hello"]);
  });

  it("drops pending output after disposal", () => {
    const writes: string[] = [];
    const scheduler = createTerminalOutputScheduler({
      write: (chunk) => writes.push(chunk),
      requestFrame: () => 1,
      cancelFrame: () => undefined,
    });

    scheduler.write("stale");
    scheduler.dispose();
    scheduler.flush();

    expect(writes).toEqual([]);
  });
});
