// @vitest-environment jsdom
import React from "react";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ActivePasteTargetState } from "../paste/activePasteTarget";
import type { PastePreviewState } from "../paste/pastePreview";
import { PTY_EXIT_EVENT, PTY_OUTPUT_EVENT } from "../terminal/ptyClient";
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

const readyPasteTarget: ActivePasteTargetState = {
  kind: "ready",
  processName: "codex.exe",
  adapterName: "codex-cli",
};
const idlePasteState: PastePreviewState = {
  kind: "idle",
  message: "Paste preview idle",
};

let capturedOutputHandlers: PtyEventHandler[] = [];
let capturedExitHandlers: PtyEventHandler[] = [];
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
    } else {
      capturedOutputHandlers.push(handler);
    }
    return Promise.resolve(mocks.unlisten);
  });

  mocks.unlisten.mockReset();
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
    const { unmount } = render(
      <TerminalView activePasteTargetState={readyPasteTarget} pasteState={idlePasteState} />,
    );

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
      outputHandler?.({ event: PTY_OUTPUT_EVENT, id: 1, payload: "hello from the shell\r\n" });
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
    // Two listeners are registered (pty-output and pty-exit); both are torn
    // down on unmount.
    expect(mocks.unlisten).toHaveBeenCalledTimes(2);
    expect(mocks.terminalDispose).toHaveBeenCalledTimes(1);
    expect(mocks.fitAddonDispose).toHaveBeenCalledTimes(1);
  });

  it("collapses React 19 StrictMode's mount/cleanup/mount cycle into a single PTY spawn", async () => {
    const { unmount } = render(
      <React.StrictMode>
        <TerminalView activePasteTargetState={readyPasteTarget} pasteState={idlePasteState} />
      </React.StrictMode>,
    );

    // StrictMode mounts, cleans up, and re-mounts the effect once in development. TerminalView
    // guards its async listen()/spawnPty() chain with a `disposed` flag specifically so the
    // discarded first mount never reaches spawnPty — this is the invariant under test.
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    });

    // Two listeners (pty-output + pty-exit) registered per mount; StrictMode
    // mounts twice in development, so listen is called four times.
    expect(mocks.listen).toHaveBeenCalledTimes(4);

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
    render(
      <TerminalView activePasteTargetState={readyPasteTarget} pasteState={idlePasteState} />,
    );

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
    render(
      <TerminalView activePasteTargetState={readyPasteTarget} pasteState={idlePasteState} />,
    );

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
      outputHandler?.({ event: PTY_OUTPUT_EVENT, id: 1, payload: "\x1b[?25l\x1b[K" });
    });
    await flushAsync();

    // The session dies mid-span. The restart must flush the filter before
    // respawning, emitting the closing 2026l and resetting filter state.
    emitPtyExit(1);
    await waitFor(() => {
      expect(getInvokeCallsFor("pty_spawn")).toHaveLength(2);
    });
    await flushAsync();

    // The fed output contained no cursor-show, so a synthetic `\x1b[?2026l` can
    // only have been emitted by the restart flush. Its presence proves the
    // filter was flushed (and thus reset) before the new session started.
    const wroteSyntheticClose = mocks.terminalWrite.mock.calls.some(
      ([data]) => typeof data === "string" && data.includes("\x1b[?2026l"),
    );
    expect(wroteSyntheticClose).toBe(true);
  });

  it("ignores a stale pty-exit for an already-superseded session", async () => {
    render(
      <TerminalView activePasteTargetState={readyPasteTarget} pasteState={idlePasteState} />,
    );

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
    const { unmount } = render(
      <TerminalView activePasteTargetState={readyPasteTarget} pasteState={idlePasteState} />,
    );

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
    render(
      <TerminalView activePasteTargetState={readyPasteTarget} pasteState={idlePasteState} />,
    );

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

  it("does not reset the restart-storm cap for output that arrives before the minimum healthy uptime", async () => {
    // Pin the clock so every spawn and its output share the same timestamp:
    // the session's uptime is always 0, i.e. below MIN_HEALTHY_UPTIME_MS. A
    // shell that prints a banner THEN dies on every launch must NOT get its
    // storm counter reset by that pre-uptime output, otherwise the cap never
    // trips and restarts spin forever.
    const nowSpy = vi.spyOn(Date, "now").mockReturnValue(1_000);
    try {
      render(
        <TerminalView activePasteTargetState={readyPasteTarget} pasteState={idlePasteState} />,
      );

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
            payload: "boot banner\r\n",
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

    render(
      <TerminalView activePasteTargetState={readyPasteTarget} pasteState={idlePasteState} />,
    );

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

    render(
      <TerminalView activePasteTargetState={readyPasteTarget} pasteState={idlePasteState} />,
    );

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
});

describe("TerminalView copy / interrupt key handling", () => {
  // The keydown listener is registered on the terminal host element in the
  // capture phase (before xterm's own handling), so tests dispatch a real
  // KeyboardEvent on that element to exercise the production wiring.
  async function renderAndGetHost() {
    const { container } = render(
      <TerminalView activePasteTargetState={readyPasteTarget} pasteState={idlePasteState} />,
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
    expect(
      getInvokeCallsFor("pty_write").some(([, args]) => (args as { data: string }).data === "\x03"),
    ).toBe(false);
  });

  it("sends SIGINT on Ctrl+C when there is no selection and does not copy", async () => {
    mocks.terminalHasSelection.mockReturnValue(false);
    const host = await renderAndGetHost();

    dispatchKeyDown(host, { key: "c", ctrlKey: true });
    await flushAsync();

    expect(
      getInvokeCallsFor("pty_write").some(([, args]) => (args as { data: string }).data === "\x03"),
    ).toBe(true);
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
    expect(
      getInvokeCallsFor("pty_write").some(([, args]) => (args as { data: string }).data === "\x03"),
    ).toBe(false);
  });
});
