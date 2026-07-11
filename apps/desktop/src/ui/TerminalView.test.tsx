// @vitest-environment jsdom
import React from "react";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PTY_EXIT_EVENT, PTY_OUTPUT_EVENT, PTY_STALL_EVENT } from "../terminal/ptyClient";
import { TerminalView } from "./TerminalView";

// jsdom does not implement requestAnimationFrame. TerminalView's output scheduler
// (terminalOutputScheduler.ts) relies on it to flush buffered PTY output into xterm, so the
// test environment needs a minimal, deterministic polyfill for PTY output assertions to resolve.
if (typeof window.requestAnimationFrame !== "function") {
  window.requestAnimationFrame = (callback: FrameRequestCallback) =>
    window.setTimeout(() => callback(performance.now()), 0) as unknown as number;
}
if (typeof window.cancelAnimationFrame !== "function") {
  window.cancelAnimationFrame = (handle: number) => window.clearTimeout(handle);
}

type InvokeArgs = unknown;
type InvokeMock = (command: string, args?: InvokeArgs, options?: unknown) => Promise<unknown>;
type PtyEventHandler = (event: { event: string; id: number; payload: unknown }) => void;
type ListenMock = (event: string, handler: PtyEventHandler) => Promise<() => void>;

const mocks = vi.hoisted(() => ({
  invoke: vi.fn<InvokeMock>(),
  listen: vi.fn<ListenMock>(),
  unlisten: vi.fn<() => void>(),
  terminalConstruct: vi.fn<(options: Record<string, unknown>) => void>(),
  terminalOpen: vi.fn<(container: HTMLElement) => void>(),
  terminalFocus: vi.fn<() => void>(),
  terminalWrite: vi.fn<(data: string) => void>(),
  terminalDispose: vi.fn<() => void>(),
  terminalLoadAddon: vi.fn<(addon: unknown) => void>(),
  terminalRegisterLinkProvider: vi.fn<(provider: unknown) => { dispose: () => void }>(),
  terminalOnData: vi.fn<(handler: (data: string) => void) => { dispose: () => void }>(),
  terminalOnResize: vi.fn<
    (handler: (size: { cols: number; rows: number }) => void) => { dispose: () => void }
  >(),
  terminalHasSelection: vi.fn<() => boolean>(),
  terminalGetSelection: vi.fn<() => string>(),
  terminalClearSelection: vi.fn<() => void>(),
  terminalPaste: vi.fn<(data: string) => void>(),
  fitAddonFit: vi.fn<() => void>(),
  fitAddonDispose: vi.fn<() => void>(),
  webglAddonOnContextLoss: vi.fn<(handler: () => void) => void>(),
  webglAddonDispose: vi.fn<() => void>(),
}));

// Mock the Tauri IPC boundary. ptyClient.ts and fileLinks.ts call the real `invoke` wrapper
// underneath, so this exercises TerminalView's actual PTY command wiring rather than
// re-implementing it in the test.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: mocks.listen,
}));

// xterm.js touches canvas measurement and platform APIs jsdom does not implement, so the
// Terminal class itself is mocked. Every method TerminalView actually calls is tracked so the
// test can assert on the real production wiring (createTerminalBridge is NOT mocked).
vi.mock("@xterm/xterm", () => {
  class MockTerminal {
    cols = 80;
    rows = 24;
    options: Record<string, unknown>;

    constructor(options: Record<string, unknown>) {
      this.options = options;
      // Capture the live options object so tests can assert that the settings
      // effect mutates fontSize/theme in place after construction.
      mocks.terminalConstruct(options);
    }

    open(container: HTMLElement) {
      mocks.terminalOpen(container);
    }

    focus() {
      mocks.terminalFocus();
    }

    write(data: string, callback?: () => void) {
      mocks.terminalWrite(data);
      callback?.();
    }

    onData(handler: (data: string) => void) {
      return mocks.terminalOnData(handler);
    }

    onResize(handler: (size: { cols: number; rows: number }) => void) {
      return mocks.terminalOnResize(handler);
    }

    loadAddon(addon: unknown) {
      mocks.terminalLoadAddon(addon);
    }

    registerLinkProvider(provider: unknown) {
      return mocks.terminalRegisterLinkProvider(provider);
    }

    hasSelection() {
      return mocks.terminalHasSelection();
    }

    getSelection() {
      return mocks.terminalGetSelection();
    }

    clearSelection() {
      mocks.terminalClearSelection();
    }

    paste(data: string) {
      mocks.terminalPaste(data);
    }

    dispose() {
      mocks.terminalDispose();
    }
  }

  return { Terminal: MockTerminal };
});

vi.mock("@xterm/addon-fit", () => {
  class MockFitAddon {
    fit() {
      mocks.fitAddonFit();
    }

    dispose() {
      mocks.fitAddonDispose();
    }
  }

  return { FitAddon: MockFitAddon };
});

vi.mock("@xterm/addon-webgl", () => {
  class MockWebglAddon {
    onContextLoss(handler: () => void) {
      mocks.webglAddonOnContextLoss(handler);
    }

    dispose() {
      mocks.webglAddonDispose();
    }
  }

  return { WebglAddon: MockWebglAddon };
});

let capturedOutputHandlers: PtyEventHandler[] = [];
let capturedExitHandlers: PtyEventHandler[] = [];
let capturedStallHandlers: PtyEventHandler[] = [];
let nextSpawnId = 0;

function getInvokeCallsFor(command: string) {
  return mocks.invoke.mock.calls.filter(([calledCommand]) => calledCommand === command);
}

// Emits a `pty-exit` event to the most recently registered exit handler,
// mirroring how the Tauri backend pushes the exiting session's monotonic id.
function emitPtyExit(sessionId: number) {
  const exitHandler = capturedExitHandlers.at(-1);
  act(() => {
    exitHandler?.({ event: PTY_EXIT_EVENT, id: 0, payload: sessionId });
  });
}

// The UTF-8 byte cost the backend charges against a session's credit window for
// a payload — Rust's `str::len()`, NOT JS's UTF-16 `String.length`.
function utf8Bytes(data: string) {
  return new TextEncoder().encode(data).length;
}

// Emits a `pty-output` event carrying the session-attributed payload the
// backend now sends: `{ sessionId, bytes, data }`. `bytes` is the credit cost
// the frontend must ack back once xterm has consumed the data.
function emitPtyOutput(sessionId: number, data: string) {
  const outputHandler = capturedOutputHandlers.at(-1);
  act(() => {
    outputHandler?.({
      event: PTY_OUTPUT_EVENT,
      id: 0,
      payload: { sessionId, bytes: utf8Bytes(data), data },
    });
  });
}

// Emits a `pty-stall` event carrying the session's flow-control stall state, as
// the backend flusher pushes when a session crosses (or clears) the stall
// threshold.
function emitPtyStall(sessionId: number, stalled: boolean) {
  const stallHandler = capturedStallHandlers.at(-1);
  act(() => {
    stallHandler?.({
      event: PTY_STALL_EVENT,
      id: 0,
      payload: { sessionId, stalled },
    });
  });
}

// Concatenates every string written to the terminal, so order-sensitive
// assertions can check that early-output chunks were flushed in arrival order
// even after the scheduler coalesces adjacent plain-text writes.
function joinedTerminalWrites() {
  return mocks.terminalWrite.mock.calls
    .map(([data]) => (typeof data === "string" ? data : ""))
    .join("");
}

// Flush the microtask + macrotask queues so an async restart (spawnPty await +
// finally) settles before the test inspects invoke calls.
async function flushAsync() {
  await act(async () => {
    await new Promise((resolve) => window.setTimeout(resolve, 0));
  });
}

beforeEach(() => {
  capturedOutputHandlers = [];
  capturedExitHandlers = [];
  capturedStallHandlers = [];
  nextSpawnId = 0;

  mocks.invoke.mockReset();
  mocks.invoke.mockImplementation((command) => {
    if (command === "pty_spawn") {
      nextSpawnId += 1;
      return Promise.resolve(nextSpawnId);
    }
    return Promise.resolve(undefined);
  });

  mocks.listen.mockReset();
  mocks.listen.mockImplementation((event, handler) => {
    if (event === PTY_EXIT_EVENT) {
      capturedExitHandlers.push(handler);
    } else if (event === PTY_STALL_EVENT) {
      capturedStallHandlers.push(handler);
    } else {
      capturedOutputHandlers.push(handler);
    }
    return Promise.resolve(mocks.unlisten);
  });

  mocks.unlisten.mockReset();
  mocks.terminalConstruct.mockReset();
  mocks.terminalOpen.mockReset();
  mocks.terminalFocus.mockReset();
  mocks.terminalWrite.mockReset();
  mocks.terminalDispose.mockReset();
  mocks.terminalLoadAddon.mockReset();

  mocks.terminalRegisterLinkProvider.mockReset();
  mocks.terminalRegisterLinkProvider.mockImplementation(() => ({ dispose: vi.fn() }));

  mocks.terminalOnData.mockReset();
  mocks.terminalOnData.mockImplementation(() => ({ dispose: vi.fn() }));

  mocks.terminalOnResize.mockReset();
  mocks.terminalOnResize.mockImplementation(() => ({ dispose: vi.fn() }));

  mocks.terminalHasSelection.mockReset();
  mocks.terminalHasSelection.mockReturnValue(false);
  mocks.terminalGetSelection.mockReset();
  mocks.terminalGetSelection.mockReturnValue("");
  mocks.terminalClearSelection.mockReset();
  mocks.terminalPaste.mockReset();

  mocks.fitAddonFit.mockReset();
  mocks.fitAddonDispose.mockReset();

  mocks.webglAddonOnContextLoss.mockReset();
  mocks.webglAddonDispose.mockReset();
});

afterEach(() => {
  cleanup();
});

describe("TerminalView PTY lifecycle", () => {
  it("spawns the PTY, registers the output listener, streams output, and disposes everything on unmount", async () => {
    const { unmount } = render(<TerminalView />);

    await waitFor(() => {
      expect(mocks.listen).toHaveBeenCalledWith(PTY_OUTPUT_EVENT, expect.any(Function));
    });

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    // The spawn call carries the terminal's current size; no explicit launch command was
    // configured, so program/args stay undefined.
    expect(getInvokeCallsFor("pty_spawn")[0]?.[1]).toEqual({
      cols: 80,
      rows: 24,
      program: undefined,
      args: undefined,
    });

    expect(mocks.terminalOpen).toHaveBeenCalledTimes(1);
    expect(mocks.terminalFocus).toHaveBeenCalledTimes(1);

    const outputHandler = capturedOutputHandlers.at(-1);
    expect(outputHandler).toBeDefined();

    act(() => {
      outputHandler?.({
        event: PTY_OUTPUT_EVENT,
        id: 1,
        payload: {
          sessionId: 1,
          bytes: utf8Bytes("hello from the shell\r\n"),
          data: "hello from the shell\r\n",
        },
      });
    });

    await waitFor(() => {
      expect(mocks.terminalWrite).toHaveBeenCalledWith("hello from the shell\r\n");
    });

    act(() => {
      unmount();
    });

    // The frontend no longer polls `pty_read`; liveness is pushed via
    // `pty-exit`, so that command must never be invoked.
    expect(getInvokeCallsFor("pty_read")).toHaveLength(0);

    expect(getInvokeCallsFor("pty_kill")).toHaveLength(1);
    // Three listeners are registered (pty-output, pty-exit, pty-stall); all
    // three are torn down on unmount.
    expect(mocks.unlisten).toHaveBeenCalledTimes(3);
    expect(mocks.terminalDispose).toHaveBeenCalledTimes(1);
    expect(mocks.fitAddonDispose).toHaveBeenCalledTimes(1);
  });

  it("collapses React 19 StrictMode's mount/cleanup/mount cycle into a single PTY spawn", async () => {
    const { unmount } = render(
      <React.StrictMode>
        <TerminalView />
      </React.StrictMode>,
    );

    // StrictMode mounts, cleans up, and re-mounts the effect once in development. TerminalView
    // guards its async listen()/spawnPty() chain with a `disposed` flag specifically so the
    // discarded first mount never reaches spawnPty — this is the invariant under test.
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    // Three listeners (pty-output + pty-exit + pty-stall) registered per mount;
    // StrictMode mounts twice in development, so listen is called six times.
    expect(mocks.listen).toHaveBeenCalledTimes(6);

    act(() => {
      unmount();
    });

    expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    expect(getInvokeCallsFor("pty_kill").length).toBeGreaterThanOrEqual(1);
    expect(mocks.terminalDispose.mock.calls.length).toBeGreaterThanOrEqual(1);
  });
});

describe("TerminalView pty-exit driven restart", () => {
  it("restarts the shell when the current session emits pty-exit", async () => {
    render(<TerminalView />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    await waitFor(() => {
      expect(capturedExitHandlers.length).toBeGreaterThan(0);
    });

    // The first spawn resolved to id 1; an exit for id 1 is the live session.
    emitPtyExit(1);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(2);
    });
  });

  it("flushes the output filter before respawning so a mid-span restart cannot corrupt the next session", async () => {
    render(<TerminalView />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    await waitFor(() => {
      expect(capturedOutputHandlers.length).toBeGreaterThan(0);
      expect(capturedExitHandlers.length).toBeGreaterThan(0);
    });

    // Feed output that opens a synthetic 2026 span but never closes it (no
    // cursor-show): the filter is now mid-span with an open synthetic sync.
    const outputHandler = capturedOutputHandlers.at(-1);
    act(() => {
      outputHandler?.({
        event: PTY_OUTPUT_EVENT,
        id: 1,
        payload: {
          sessionId: 1,
          bytes: utf8Bytes("\x1b[?25l\x1b[K"),
          data: "\x1b[?25l\x1b[K",
        },
      });
    });
    await flushAsync();

    // The session dies mid-span. The restart must flush the filter before
    // respawning, emitting the closing 2026l and resetting filter state.
    emitPtyExit(1);
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(2);
    });

    // The fed output contained no cursor-show, so a synthetic `\x1b[?2026l` can
    // only have been emitted by the restart flush. The flush writes through the
    // rAF-backed output scheduler, so wait for the scheduled terminal write
    // rather than asserting immediately after the restart promise settles.
    await waitFor(() => {
      const wroteSyntheticClose = mocks.terminalWrite.mock.calls.some(
        ([data]) => typeof data === "string" && data.includes("\x1b[?2026l"),
      );
      expect(wroteSyntheticClose).toBe(true);
    });
  });

  it("ignores a stale pty-exit for an already-superseded session", async () => {
    render(<TerminalView />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    // Restart once so the current session id advances to 2.
    emitPtyExit(1);
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(2);
    });

    // A late exit for the old session (id 1 < current id 2) must be ignored.
    emitPtyExit(1);
    await flushAsync();

    expect(getInvokeCallsFor("pty_spawn")).toHaveLength(2);
  });

  it("ignores pty-exit that arrives after unmount", async () => {
    const { unmount } = render(<TerminalView />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    act(() => {
      unmount();
    });

    emitPtyExit(1);
    await flushAsync();

    // Disposed: no restart spawn beyond the original.
    expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
  });

  it("caps consecutive restarts to prevent a restart storm", async () => {
    render(<TerminalView />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    // Each exit for the current session (ids are sequential, so the current id
    // equals the current spawn count) triggers one restart. With no PTY output
    // in between, nothing resets the counter, so the cap must halt restarts.
    let lastSpawnCount = getInvokeCallsFor("pty_spawn").length;
    for (let attempt = 0; attempt < 12; attempt += 1) {
      emitPtyExit(lastSpawnCount);
      await flushAsync();
      const spawnCount = getInvokeCallsFor("pty_spawn").length;
      if (spawnCount === lastSpawnCount) {
        break;
      }
      lastSpawnCount = spawnCount;
    }

    // Some restarts happened, but the cap kept them bounded (initial + at most
    // a handful of restarts) rather than spinning forever.
    expect(lastSpawnCount).toBeGreaterThan(1);
    expect(lastSpawnCount).toBeLessThanOrEqual(6);
  });

  it("signals reconnecting on a restart and failed when the storm cap gives up", async () => {
    const onSessionHealth = vi.fn<(status: string) => void>();
    render(<TerminalView onSessionHealth={onSessionHealth} />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    await waitFor(() => {
      expect(capturedExitHandlers.length).toBeGreaterThan(0);
    });

    // A single restart announces "reconnecting" the moment restartPty begins.
    emitPtyExit(1);
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(2);
    });
    await flushAsync();
    expect(onSessionHealth.mock.calls.map(([status]) => status)).toContain("reconnecting");

    // Keep exiting with no healthy output in between so the storm cap trips.
    let lastSpawnCount = getInvokeCallsFor("pty_spawn").length;
    for (let attempt = 0; attempt < 12; attempt += 1) {
      emitPtyExit(lastSpawnCount);
      await flushAsync();
      const spawnCount = getInvokeCallsFor("pty_spawn").length;
      if (spawnCount === lastSpawnCount) {
        break;
      }
      lastSpawnCount = spawnCount;
    }

    // When consecutiveRestarts exceeds the cap, the guard gives up and the
    // health signal reports "failed".
    expect(onSessionHealth.mock.calls.map(([status]) => status)).toContain("failed");
  });

  it("does not reset the restart-storm cap for output that arrives before the minimum healthy uptime", async () => {
    // Pin the clock so every spawn and its output share the same timestamp:
    // the session's uptime is always 0, i.e. below MIN_HEALTHY_UPTIME_MS. A
    // shell that prints a banner THEN dies on every launch must NOT get its
    // storm counter reset by that pre-uptime output, otherwise the cap never
    // trips and restarts spin forever.
    const nowSpy = vi.spyOn(Date, "now").mockReturnValue(1_000);
    try {
      render(<TerminalView />);

      await waitFor(() => {
        expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
      });
      await waitFor(() => {
        expect(capturedOutputHandlers.length).toBeGreaterThan(0);
        expect(capturedExitHandlers.length).toBeGreaterThan(0);
      });

      let lastSpawnCount = getInvokeCallsFor("pty_spawn").length;
      for (let attempt = 0; attempt < 12; attempt += 1) {
        const outputHandler = capturedOutputHandlers.at(-1);
        act(() => {
          outputHandler?.({
            event: PTY_OUTPUT_EVENT,
            id: lastSpawnCount,
            payload: {
              sessionId: lastSpawnCount,
              bytes: utf8Bytes("boot banner\r\n"),
              data: "boot banner\r\n",
            },
          });
        });
        emitPtyExit(lastSpawnCount);
        await flushAsync();
        const spawnCount = getInvokeCallsFor("pty_spawn").length;
        if (spawnCount === lastSpawnCount) {
          break;
        }
        lastSpawnCount = spawnCount;
      }

      // Output that arrived before the min uptime did not reset the counter, so
      // the cap halted the storm (initial + at most a handful of restarts).
      expect(lastSpawnCount).toBeGreaterThan(1);
      expect(lastSpawnCount).toBeLessThanOrEqual(6);
    } finally {
      nowSpy.mockRestore();
    }
  });

  it("restarts once when a session's pty-exit arrives before its spawn is recorded", async () => {
    let resolveSpawn1: (() => void) | undefined;
    let spawnId = 0;
    mocks.invoke.mockImplementation((command) => {
      if (command === "pty_spawn") {
        spawnId += 1;
        const thisId = spawnId;
        if (thisId === 1) {
          // Defer session 1's spawn so its pty-exit can land BEFORE the
          // frontend records currentSessionId = 1.
          return new Promise<number>((resolve) => {
            resolveSpawn1 = () => resolve(thisId);
          });
        }
        return Promise.resolve(thisId);
      }
      return Promise.resolve(undefined);
    });

    render(<TerminalView />);

    // Listeners are registered before the (still-pending) first spawn.
    await waitFor(() => {
      expect(capturedExitHandlers.length).toBeGreaterThan(0);
    });
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    // Backend reports session 1 died instantly — BEFORE spawnPty resolved, so
    // currentSessionId is still undefined. The exit must be stashed, not dropped.
    emitPtyExit(1);
    await flushAsync();
    expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);

    // The spawn now resolves and records id 1; the stashed exit drives exactly
    // one restart (spawn #2), not a storm.
    await act(async () => {
      resolveSpawn1?.();
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(2);
    });
    await flushAsync();
    expect(getInvokeCallsFor("pty_spawn")).toHaveLength(2);
  });

  it("coalesces a pty-exit that arrives mid-restart into exactly one more restart", async () => {
    let resolveDeferredSpawn: (() => void) | undefined;
    let spawnId = 0;
    mocks.invoke.mockImplementation((command) => {
      if (command === "pty_spawn") {
        spawnId += 1;
        const thisId = spawnId;
        if (spawnId === 1) {
          return Promise.resolve(thisId);
        }
        // Defer the first restart's spawn so an exit can land mid-flight.
        return new Promise<number>((resolve) => {
          resolveDeferredSpawn = () => resolve(thisId);
        });
      }
      return Promise.resolve(undefined);
    });

    render(<TerminalView />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    // First exit starts a restart whose spawn (#2) is deferred.
    emitPtyExit(1);
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(2);
    });

    // A second exit for the still-current session lands while the restart is
    // in flight; it must be coalesced, not dropped.
    emitPtyExit(1);
    await flushAsync();

    // Resolving the deferred spawn finishes the first restart; the coalesced
    // pending exit then drives exactly one additional restart (spawn #3).
    await act(async () => {
      resolveDeferredSpawn?.();
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(3);
    });
  });

  // Cursor-show holdback wiring (change: codex-cursor-holdback). The filter is
  // now constructed with an `onDeferredOutput` sink and a real timer, so a
  // cursor-show is HELD rather than written inline, and the disposed-guarded sink
  // plus teardown flush guarantee the held show is released exactly once — never
  // stranded and never double-emitted by a post-dispose timer fire.
  it("holds the cursor-show, then releases it exactly once at teardown with no post-dispose double emit", async () => {
    const { unmount } = render(<TerminalView />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    await waitFor(() => {
      expect(capturedOutputHandlers.length).toBeGreaterThan(0);
    });

    // A cursor-hidden span that then shows the cursor: hide opens a synthetic
    // sync (BEGIN_SYNC), show closes it (END_SYNC) but the show is HELD behind
    // the quiet timer instead of being written inline.
    const outputHandler = capturedOutputHandlers.at(-1);
    act(() => {
      outputHandler?.({
        event: PTY_OUTPUT_EVENT,
        id: 1,
        payload: {
          sessionId: 1,
          bytes: utf8Bytes("\x1b[?25l frame \x1b[?25h"),
          data: "\x1b[?25l frame \x1b[?25h",
        },
      });
    });
    await flushAsync();

    const showWrites = () =>
      mocks.terminalWrite.mock.calls.filter(
        ([data]) => typeof data === "string" && data.includes("\x1b[?25h"),
      ).length;

    // Holdback active: the show has NOT been written to the terminal yet. Without
    // the sink wiring it would have been emitted inline immediately.
    expect(showWrites()).toBe(0);

    // Teardown flush() releases the held show and cancels the quiet timer.
    act(() => {
      unmount();
    });
    const releasedAtTeardown = showWrites();
    expect(releasedAtTeardown).toBe(1);

    // Wait well past the quiet interval: a cancelled/disposed-guarded timer must
    // NOT fire a second, stranded deferred emission after the component is gone.
    await new Promise((resolve) => setTimeout(resolve, 250));
    expect(showWrites()).toBe(releasedAtTeardown);
  });
});

describe("TerminalView credit acking", () => {
  // End-to-end guard on the ACK half of the flow-control loop: payload ->
  // filter -> scheduler -> xterm.write -> consumption callback -> pty_ack.
  // If this wiring breaks, the backend's credit window drains, its flusher
  // stops emitting, the bounded channel fills and the CHILD PROCESS BLOCKS
  // forever — a hang, not a visible error. So it gets its own test.
  it("acks consumed bytes back to the owning session once the threshold is crossed", async () => {
    render(<TerminalView />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    await waitFor(() => {
      expect(capturedOutputHandlers.length).toBeGreaterThan(0);
    });

    // One payload past the 256 KiB ack threshold, so exactly one ack is due.
    const flood = "x".repeat(300_000);
    emitPtyOutput(1, flood);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_ack")).toHaveLength(1);
    });
    expect(getInvokeCallsFor("pty_ack")[0][1]).toEqual({
      sessionId: 1,
      bytes: 300_000,
    });
  });

  it("does not ack a small burst one chunk at a time", async () => {
    render(<TerminalView />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    await waitFor(() => {
      expect(capturedOutputHandlers.length).toBeGreaterThan(0);
    });

    // Ten ordinary chunks, nowhere near the threshold. Acking per chunk would
    // trade an unbounded buffer for an IPC storm, which is not a fix.
    for (let index = 0; index < 10; index += 1) {
      emitPtyOutput(1, `line ${index}\r\n`);
    }
    await flushAsync();

    expect(getInvokeCallsFor("pty_ack")).toHaveLength(0);
  });
});

describe("TerminalView session demultiplexing", () => {
  it("writes output whose sessionId matches the current session", async () => {
    render(<TerminalView />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    await waitFor(() => {
      expect(capturedOutputHandlers.length).toBeGreaterThan(0);
    });

    // The first spawn resolved to id 1, so output attributed to session 1 is
    // this view's own output and must be written.
    emitPtyOutput(1, "own output\r\n");
    await waitFor(() => {
      expect(mocks.terminalWrite).toHaveBeenCalledWith("own output\r\n");
    });
  });

  it("drops output attributed to a stale, already-superseded session", async () => {
    render(<TerminalView />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    // Restart once so the current session id advances to 2.
    emitPtyExit(1);
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(2);
    });
    await flushAsync();

    // Output for the old session (id 1 < current id 2) must NOT be written.
    emitPtyOutput(1, "STALE-SESSION-OUTPUT");
    await flushAsync();

    expect(joinedTerminalWrites()).not.toContain("STALE-SESSION-OUTPUT");
  });

  it("queues multi-chunk early output and flushes it in arrival order on record", async () => {
    let resolveSpawn1: (() => void) | undefined;
    let spawnId = 0;
    mocks.invoke.mockImplementation((command) => {
      if (command === "pty_spawn") {
        spawnId += 1;
        const thisId = spawnId;
        if (thisId === 1) {
          return new Promise<number>((resolve) => {
            resolveSpawn1 = () => resolve(thisId);
          });
        }
        return Promise.resolve(thisId);
      }
      return Promise.resolve(undefined);
    });

    render(<TerminalView />);

    await waitFor(() => {
      expect(capturedOutputHandlers.length).toBeGreaterThan(0);
    });
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    // Three chunks for session 1 arrive BEFORE spawnPty resolves (currentSessionId
    // is still undefined), so they must be queued, not dropped.
    emitPtyOutput(1, "AAA");
    emitPtyOutput(1, "BBB");
    emitPtyOutput(1, "CCC");
    await flushAsync();
    // Nothing recorded yet: the chunks are still queued, not written.
    expect(joinedTerminalWrites()).not.toContain("AAA");

    // Recording session 1 synchronously drains the queue in arrival order.
    await act(async () => {
      resolveSpawn1?.();
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });
    // Let the output scheduler's rAF flush the coalesced write to the terminal.
    await flushAsync();

    await waitFor(() => {
      expect(joinedTerminalWrites()).toContain("AAABBBCCC");
    });
  });

  it("drops queued chunks whose sessionId differs from the recorded session on flush", async () => {
    let resolveSpawn1: (() => void) | undefined;
    let spawnId = 0;
    mocks.invoke.mockImplementation((command) => {
      if (command === "pty_spawn") {
        spawnId += 1;
        const thisId = spawnId;
        if (thisId === 1) {
          return new Promise<number>((resolve) => {
            resolveSpawn1 = () => resolve(thisId);
          });
        }
        return Promise.resolve(thisId);
      }
      return Promise.resolve(undefined);
    });

    render(<TerminalView />);

    await waitFor(() => {
      expect(capturedOutputHandlers.length).toBeGreaterThan(0);
    });
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    // One chunk for the about-to-be-recorded session 1 and one for a different
    // (never-recorded-here) session 2, both queued before spawn resolves.
    emitPtyOutput(1, "KEEP-ME");
    emitPtyOutput(2, "DROP-ME");
    await flushAsync();

    await act(async () => {
      resolveSpawn1?.();
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });
    await flushAsync();

    await waitFor(() => {
      expect(joinedTerminalWrites()).toContain("KEEP-ME");
    });
    expect(joinedTerminalWrites()).not.toContain("DROP-ME");
  });

  it("flushes queued output BEFORE acting on a stashed instant-exit so the banner still prints", async () => {
    let resolveSpawn1: (() => void) | undefined;
    let spawnId = 0;
    mocks.invoke.mockImplementation((command) => {
      if (command === "pty_spawn") {
        spawnId += 1;
        const thisId = spawnId;
        if (thisId === 1) {
          return new Promise<number>((resolve) => {
            resolveSpawn1 = () => resolve(thisId);
          });
        }
        return Promise.resolve(thisId);
      }
      return Promise.resolve(undefined);
    });

    render(<TerminalView />);

    await waitFor(() => {
      expect(capturedOutputHandlers.length).toBeGreaterThan(0);
      expect(capturedExitHandlers.length).toBeGreaterThan(0);
    });
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    // An instant-exit shell prints a banner then dies, BOTH before spawnPty
    // resolves: the banner is queued and the exit is stashed.
    emitPtyOutput(1, "INSTANT-BANNER");
    emitPtyExit(1);
    await flushAsync();

    // Recording session 1 must flush the banner BEFORE it acts on the stashed
    // exit and restarts, so the banner is not eaten by the early-return path.
    await act(async () => {
      resolveSpawn1?.();
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });
    await flushAsync();

    await waitFor(() => {
      expect(joinedTerminalWrites()).toContain("INSTANT-BANNER");
    });
    // The stashed exit still drove exactly one restart.
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(2);
    });
  });

  it("kills a spawn that resolves after the view was disposed and records no state", async () => {
    let resolveSpawn1: (() => void) | undefined;
    let spawnId = 0;
    mocks.invoke.mockImplementation((command) => {
      if (command === "pty_spawn") {
        spawnId += 1;
        const thisId = spawnId;
        if (thisId === 1) {
          return new Promise<number>((resolve) => {
            resolveSpawn1 = () => resolve(thisId);
          });
        }
        return Promise.resolve(thisId);
      }
      return Promise.resolve(undefined);
    });

    const onPtyReady = vi.fn<(sessionId: number) => void>();
    const { unmount } = render(<TerminalView onPtyReady={onPtyReady} />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    // Dispose the view while spawn 1 is still in flight.
    act(() => {
      unmount();
    });

    // The in-flight spawn now resolves AFTER disposal: it must kill the orphan
    // session by id and record no state (no onPtyReady for it).
    await act(async () => {
      resolveSpawn1?.();
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });

    expect(
      getInvokeCallsFor("pty_kill").some(
        ([, args]) => (args as { sessionId: number }).sessionId === 1,
      ),
    ).toBe(true);
    expect(onPtyReady).not.toHaveBeenCalled();
  });

  it("passes the spawned session id to onPtyReady", async () => {
    const onPtyReady = vi.fn<(sessionId: number) => void>();
    render(<TerminalView onPtyReady={onPtyReady} />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    await flushAsync();

    expect(onPtyReady).toHaveBeenCalledWith(1);
  });
});

describe("TerminalView title bar extraction", () => {
  it("no longer renders the status footer or a Settings toggle", async () => {
    const { container, queryByRole } = render(<TerminalView />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    // The status cluster + Settings toggle moved to the TitleBar; the terminal
    // frame must no longer host the footer or its toggle.
    expect(container.querySelector(".terminal-statusbar")).toBeNull();
    expect(queryByRole("button", { name: "Settings" })).toBeNull();
  });

  it("no longer renders its own settings panel (the panel was lifted to App)", async () => {
    // Settings are global now: App owns the single TerminalSettingsPanel. The
    // per-tab TerminalView must never render a settings panel of its own,
    // regardless of props, so N tabs cannot each pop their own overlay.
    const { container } = render(<TerminalView />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    expect(container.querySelector(".terminal-settings-panel")).toBeNull();
  });

  it("signals healthy once the initial session settles", async () => {
    const onSessionHealth = vi.fn<(status: string) => void>();
    render(<TerminalView onSessionHealth={onSessionHealth} />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    await flushAsync();

    // A successful startPty settling proves the session booted; the health
    // signal reports "healthy" so the title bar dot stays quiet.
    expect(onSessionHealth.mock.calls.map(([status]) => status)).toContain("healthy");
  });

  it("reports stalled on a pty-stall event and returns to healthy when it clears", async () => {
    const onSessionHealth = vi.fn<(status: string) => void>();
    render(<TerminalView onSessionHealth={onSessionHealth} />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    await flushAsync();

    // A stall for the live session must surface as a distinct "stalled" health,
    // so a frozen terminal is never silent.
    emitPtyStall(1, true);
    expect(onSessionHealth.mock.calls.map(([status]) => status)).toContain("stalled");

    // When credit flows again the backend clears the stall; the tab returns to
    // healthy rather than staying stuck on the stalled dot.
    onSessionHealth.mockClear();
    emitPtyStall(1, false);
    expect(onSessionHealth.mock.calls.map(([status]) => status)).toContain("healthy");
  });

  it("ignores a pty-stall event for a foreign session id", async () => {
    const onSessionHealth = vi.fn<(status: string) => void>();
    render(<TerminalView onSessionHealth={onSessionHealth} />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    await flushAsync();
    onSessionHealth.mockClear();

    // A sibling tab's stall (id 99, not this view's session 1) must never mark
    // this tab stalled.
    emitPtyStall(99, true);
    expect(onSessionHealth.mock.calls.map(([status]) => status)).not.toContain("stalled");
  });
});

describe("TerminalView copy / interrupt key handling", () => {
  // The keydown listener is registered on the terminal host element in the
  // capture phase (before xterm's own handling), so tests dispatch a real
  // KeyboardEvent on that element to exercise the production wiring.
  async function renderAndGetHost() {
    const { container } = render(<TerminalView />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    const host = container.querySelector(".terminal-host") as HTMLElement;
    expect(host).toBeTruthy();
    return host;
  }

  function dispatchKeyDown(
    host: HTMLElement,
    init: { key: string; ctrlKey?: boolean; shiftKey?: boolean },
  ) {
    act(() => {
      host.dispatchEvent(
        new KeyboardEvent("keydown", {
          bubbles: true,
          cancelable: true,
          ...init,
        }),
      );
    });
  }

  it("copies the selection on Ctrl+C and does not send SIGINT", async () => {
    mocks.terminalHasSelection.mockReturnValue(true);
    mocks.terminalGetSelection.mockReturnValue("selected text");
    const host = await renderAndGetHost();

    dispatchKeyDown(host, { key: "c", ctrlKey: true });
    await flushAsync();

    expect(getInvokeCallsFor("clipboard_write_text")).toHaveLength(1);
    expect(getInvokeCallsFor("clipboard_write_text")[0]?.[1]).toEqual({ text: "selected text" });
    expect(mocks.terminalClearSelection).toHaveBeenCalledTimes(1);
    expect(getInvokeCallsFor("pty_interrupt")).toHaveLength(0);
    expect(
      getInvokeCallsFor("pty_write").some(([, args]) => (args as { data: string }).data === "\x03"),
    ).toBe(false);
  });

  it("sends SIGINT on Ctrl+C when there is no selection and does not copy", async () => {
    mocks.terminalHasSelection.mockReturnValue(false);
    const host = await renderAndGetHost();

    dispatchKeyDown(host, { key: "c", ctrlKey: true });
    await flushAsync();

    // Ctrl+C MUST route through `pty_interrupt` (raw \x03 write + native
    // CTRL_C_EVENT), not a bare `pty_write` of the C0 byte, which does not
    // reliably interrupt a Windows console child.
    expect(getInvokeCallsFor("pty_interrupt")).toHaveLength(1);
    expect(getInvokeCallsFor("pty_interrupt")[0]?.[1]).toEqual({ sessionId: 1 });
    expect(
      getInvokeCallsFor("pty_write").some(([, args]) => (args as { data: string }).data === "\x03"),
    ).toBe(false);
    expect(getInvokeCallsFor("clipboard_write_text")).toHaveLength(0);
  });

  it("copies the selection on Ctrl+Shift+C", async () => {
    mocks.terminalHasSelection.mockReturnValue(true);
    mocks.terminalGetSelection.mockReturnValue("shift copy");
    const host = await renderAndGetHost();

    dispatchKeyDown(host, { key: "c", ctrlKey: true, shiftKey: true });
    await flushAsync();

    expect(getInvokeCallsFor("clipboard_write_text")).toHaveLength(1);
    expect(getInvokeCallsFor("clipboard_write_text")[0]?.[1]).toEqual({ text: "shift copy" });
    expect(getInvokeCallsFor("pty_interrupt")).toHaveLength(0);
    expect(
      getInvokeCallsFor("pty_write").some(([, args]) => (args as { data: string }).data === "\x03"),
    ).toBe(false);
  });

  it("does nothing on Ctrl+C when the selection resolves to an empty string", async () => {
    // hasSelection() can report true while getSelection() yields "" (e.g. a
    // zero-width selection). The handler must neither copy nor eat the key.
    mocks.terminalHasSelection.mockReturnValue(true);
    mocks.terminalGetSelection.mockReturnValue("");
    const host = await renderAndGetHost();

    dispatchKeyDown(host, { key: "c", ctrlKey: true });
    await flushAsync();

    expect(getInvokeCallsFor("clipboard_write_text")).toHaveLength(0);
    expect(mocks.terminalClearSelection).not.toHaveBeenCalled();
    expect(getInvokeCallsFor("pty_interrupt")).toHaveLength(0);
    expect(
      getInvokeCallsFor("pty_write").some(([, args]) => (args as { data: string }).data === "\x03"),
    ).toBe(false);
  });
});

describe("TerminalView paste key handling", () => {
  // The keydown listener runs in the capture phase before xterm maps Ctrl+V to
  // the C0 byte \x16, so tests dispatch a real KeyboardEvent on the host element.
  async function renderAndGetHost(
    props: {
      onClipboardImagePaste?: () => void | Promise<void>;
    } = {},
  ) {
    const { container } = render(
      <TerminalView onClipboardImagePaste={props.onClipboardImagePaste} />,
    );

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    const host = container.querySelector(".terminal-host") as HTMLElement;
    expect(host).toBeTruthy();
    return host;
  }

  function dispatchKeyDown(
    host: HTMLElement,
    init: { key: string; ctrlKey?: boolean; shiftKey?: boolean },
  ) {
    act(() => {
      host.dispatchEvent(
        new KeyboardEvent("keydown", { bubbles: true, cancelable: true, ...init }),
      );
    });
  }

  function mockClipboardReadText(text: string) {
    mocks.invoke.mockImplementation((command) => {
      if (command === "pty_spawn") {
        nextSpawnId += 1;
        return Promise.resolve(nextSpawnId);
      }
      if (command === "clipboard_read_text") {
        return Promise.resolve(text);
      }
      return Promise.resolve(undefined);
    });
  }

  it("reads the clipboard and pastes text via terminal.paste on Ctrl+V (never the \\x16 byte)", async () => {
    mockClipboardReadText("hello world");
    const onClipboardImagePaste = vi.fn<() => Promise<void>>().mockResolvedValue();
    const host = await renderAndGetHost({ onClipboardImagePaste });

    dispatchKeyDown(host, { key: "v", ctrlKey: true });
    await flushAsync();

    expect(getInvokeCallsFor("clipboard_read_text")).toHaveLength(1);
    // terminal.paste applies bracketed-paste wrapping + CRLF normalization; the
    // raw \x16 control byte must never be sent to the PTY.
    expect(mocks.terminalPaste).toHaveBeenCalledWith("hello world");
    expect(
      getInvokeCallsFor("pty_write").some(([, args]) => (args as { data: string }).data === "\x16"),
    ).toBe(false);
    // Prefer text over image when text is present.
    expect(onClipboardImagePaste).not.toHaveBeenCalled();
  });

  it("falls back to the image paste route when the clipboard has no text", async () => {
    mockClipboardReadText("");
    const onClipboardImagePaste = vi.fn<() => Promise<void>>().mockResolvedValue();
    const host = await renderAndGetHost({ onClipboardImagePaste });

    dispatchKeyDown(host, { key: "v", ctrlKey: true });
    await flushAsync();

    expect(getInvokeCallsFor("clipboard_read_text")).toHaveLength(1);
    expect(mocks.terminalPaste).not.toHaveBeenCalled();
    expect(onClipboardImagePaste).toHaveBeenCalledTimes(1);
  });

  it("is a clean no-op on Ctrl+V when there is neither text nor an image handler", async () => {
    mockClipboardReadText("");
    const host = await renderAndGetHost();

    dispatchKeyDown(host, { key: "v", ctrlKey: true });
    await flushAsync();

    expect(getInvokeCallsFor("clipboard_read_text")).toHaveLength(1);
    expect(mocks.terminalPaste).not.toHaveBeenCalled();
    // No image handler was provided: the handler must not throw or write an error.
    expect(
      mocks.terminalWrite.mock.calls.some(
        ([data]) => typeof data === "string" && data.includes("Paste failed"),
      ),
    ).toBe(false);
  });

  it("pastes clipboard text on Shift+Insert as well", async () => {
    mockClipboardReadText("insert paste");
    const host = await renderAndGetHost();

    dispatchKeyDown(host, { key: "Insert", ctrlKey: false, shiftKey: true });
    await flushAsync();

    expect(getInvokeCallsFor("clipboard_read_text")).toHaveLength(1);
    expect(mocks.terminalPaste).toHaveBeenCalledWith("insert paste");
  });
});

describe("TerminalView active prop", () => {
  it("refits and focuses when the active prop flips to true", async () => {
    const { rerender } = render(<TerminalView active={false} />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    await flushAsync();

    // Baseline counts after the initial mount (bridge fits + focuses once).
    const fitsBefore = mocks.fitAddonFit.mock.calls.length;
    const focusBefore = mocks.terminalFocus.mock.calls.length;

    // Activating a previously-inactive tab must re-fit (it may have been hidden
    // and resized) and route focus to it.
    rerender(<TerminalView active />);
    await flushAsync();

    expect(mocks.fitAddonFit.mock.calls.length).toBeGreaterThan(fitsBefore);
    expect(mocks.terminalFocus.mock.calls.length).toBeGreaterThan(focusBefore);
  });
});

describe("TerminalView settings prop", () => {
  it("applies fontSize/theme to xterm and refits when the settings prop changes", async () => {
    const { rerender } = render(
      <TerminalView settings={{ background: "#000000", foreground: "#ffffff", fontSize: 14 }} />,
    );

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    await flushAsync();

    const options = mocks.terminalConstruct.mock.calls.at(-1)?.[0] as
      | Record<string, unknown>
      | undefined;
    expect(options?.fontSize).toBe(14);

    const fitsBefore = mocks.fitAddonFit.mock.calls.length;

    rerender(
      <TerminalView settings={{ background: "#111111", foreground: "#eeeeee", fontSize: 20 }} />,
    );
    await flushAsync();

    // The settings effect mutates the live options object and refits so the
    // grid tracks the new cell size.
    expect(options?.fontSize).toBe(20);
    expect((options?.theme as { background?: string })?.background).toBe("#111111");
    expect(mocks.fitAddonFit.mock.calls.length).toBeGreaterThan(fitsBefore);
  });
});

describe("TerminalView multi-instance event isolation", () => {
  it("drops foreign-session pty-output while not spawning (never enqueued)", async () => {
    render(<TerminalView />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    // Settle the spawn so spawnInFlight is false and currentSessionId === 1.
    await flushAsync();

    // A sibling tab's session (id 99) emits output on the shared global stream.
    // This instance is NOT spawning, so the chunk must be DROPPED outright, not
    // enqueued into earlyOutputQueue where it would leak forever.
    emitPtyOutput(99, "FOREIGN-OUTPUT");
    await flushAsync();

    expect(joinedTerminalWrites()).not.toContain("FOREIGN-OUTPUT");

    // Proof the instance's own pipeline is intact: its own session output still
    // writes, and a foreign chunk never sneaks in via a later own-session flush.
    emitPtyOutput(1, "OWN-OUTPUT");
    await flushAsync();
    await waitFor(() => {
      expect(joinedTerminalWrites()).toContain("OWN-OUTPUT");
    });
    expect(joinedTerminalWrites()).not.toContain("FOREIGN-OUTPUT");
  });

  it("drops a foreign pty-exit while not spawning and does not mark itself dead", async () => {
    const onSessionHealth = vi.fn<(status: string) => void>();
    render(<TerminalView onSessionHealth={onSessionHealth} />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    await flushAsync();

    // A sibling tab's session exits. This instance is not spawning, so the exit
    // must be dropped: no restart, no health change from the foreign event.
    emitPtyExit(99);
    await flushAsync();
    expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);

    // This instance's own machinery is untouched: its own exit still restarts.
    emitPtyExit(1);
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(2);
    });
  });

  it("keeps its own stashed instant-exit even when a foreign exit interleaves in the spawn window", async () => {
    // Defer session 1's spawn so exits can land BEFORE currentSessionId = 1 is
    // recorded — this is the spawn window where the old single-slot stash was
    // clobberable by a foreign exit.
    let resolveSpawn1: (() => void) | undefined;
    let spawnId = 0;
    mocks.invoke.mockImplementation((command) => {
      if (command === "pty_spawn") {
        spawnId += 1;
        const thisId = spawnId;
        if (thisId === 1) {
          return new Promise<number>((resolve) => {
            resolveSpawn1 = () => resolve(thisId);
          });
        }
        return Promise.resolve(thisId);
      }
      return Promise.resolve(undefined);
    });

    render(<TerminalView />);

    await waitFor(() => {
      expect(capturedExitHandlers.length).toBeGreaterThan(0);
    });
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    // This instance's own session (1) instant-exits, then a FOREIGN session (99)
    // exit lands right after — both inside this instance's spawn window. With a
    // single slot, 99 would overwrite 1 and the own insta-exit would be lost.
    emitPtyExit(1);
    emitPtyExit(99);
    await flushAsync();
    expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);

    // Recording id 1 must still detect its OWN stashed insta-exit (the Set is
    // clobber-proof) and drive exactly one restart.
    await act(async () => {
      resolveSpawn1?.();
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(2);
    });
    await flushAsync();
    // Exactly one restart — the foreign exit neither added a second restart nor
    // suppressed the own one.
    expect(getInvokeCallsFor("pty_spawn")).toHaveLength(2);
  });

  it("clears the early-output queue on a spawn reject so nothing leaks into a later session", async () => {
    // Session 1's spawn rejects. Any output queued during its (now-doomed) spawn
    // window must be dropped, not carried forward.
    let rejectSpawn1: ((reason: unknown) => void) | undefined;
    let spawnId = 0;
    mocks.invoke.mockImplementation((command) => {
      if (command === "pty_spawn") {
        spawnId += 1;
        const thisId = spawnId;
        if (thisId === 1) {
          return new Promise<number>((_resolve, reject) => {
            rejectSpawn1 = reject;
          });
        }
        return Promise.resolve(thisId);
      }
      return Promise.resolve(undefined);
    });

    render(<TerminalView />);

    await waitFor(() => {
      expect(capturedOutputHandlers.length).toBeGreaterThan(0);
    });
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    // Output arrives while spawn 1 is in flight (queued), then the spawn fails.
    emitPtyOutput(1, "PRE-REJECT-OUTPUT");
    await act(async () => {
      rejectSpawn1?.(new Error("spawn boom"));
      await new Promise((resolve) => window.setTimeout(resolve, 0));
    });
    await flushAsync();

    // The reject settle path surfaces the failure and clears the queue: the
    // queued chunk is never flushed to the terminal.
    expect(
      mocks.terminalWrite.mock.calls.some(
        ([data]) => typeof data === "string" && data.includes("Failed to start ConPTY session"),
      ),
    ).toBe(true);
    expect(joinedTerminalWrites()).not.toContain("PRE-REJECT-OUTPUT");
  });

  it("closes the spawn window when onPtyReady throws so a later foreign chunk is dropped", async () => {
    // If the ready callback (or the flush write-pipeline) throws AFTER the spawn
    // resolves, the settle-path closeSpawnWindow() below would be skipped,
    // stranding spawnInFlight=true — every sibling tab's output would then
    // accumulate here unbounded. The try/catch around onPtyReady closes the
    // window and rethrows; this test locks that in: with the window closed, a
    // foreign chunk arriving afterward is DROPPED, never enqueued.
    const onPtyReady = vi.fn<(sessionId: number) => void>(() => {
      throw new Error("ready boom");
    });
    render(<TerminalView onPtyReady={onPtyReady} />);

    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });
    // Let startPty settle: onPtyReady throws, the window closes, and the caller's
    // catch surfaces the failure.
    await flushAsync();

    expect(onPtyReady).toHaveBeenCalledWith(1);
    expect(
      mocks.terminalWrite.mock.calls.some(
        ([data]) => typeof data === "string" && data.includes("Failed to start ConPTY session"),
      ),
    ).toBe(true);

    // Spawn window is closed (spawnInFlight=false): a foreign session's output
    // must be dropped outright, not enqueued into the early-output queue.
    emitPtyOutput(99, "FOREIGN-AFTER-THROW");
    await flushAsync();
    expect(joinedTerminalWrites()).not.toContain("FOREIGN-AFTER-THROW");
  });
});
