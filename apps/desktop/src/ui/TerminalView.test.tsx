// @vitest-environment jsdom
import React from "react";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ActivePasteTargetState } from "../paste/activePasteTarget";
import type { PastePreviewState } from "../paste/pastePreview";
import { PTY_OUTPUT_EVENT } from "../terminal/ptyClient";
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
type PtyOutputHandler = (event: { event: string; id: number; payload: unknown }) => void;
type ListenMock = (event: string, handler: PtyOutputHandler) => Promise<() => void>;

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

let capturedOutputHandlers: PtyOutputHandler[] = [];

function getInvokeCallsFor(command: string) {
  return mocks.invoke.mock.calls.filter(([calledCommand]) => calledCommand === command);
}

beforeEach(() => {
  capturedOutputHandlers = [];

  mocks.invoke.mockReset();
  mocks.invoke.mockImplementation((command) => {
    if (command === "pty_read") {
      return Promise.resolve<string[]>([]);
    }
    return Promise.resolve(undefined);
  });

  mocks.listen.mockReset();
  mocks.listen.mockImplementation((_event, handler) => {
    capturedOutputHandlers.push(handler);
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

    expect(getInvokeCallsFor("pty_kill")).toHaveLength(1);
    expect(mocks.unlisten).toHaveBeenCalledTimes(1);
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

    expect(mocks.listen).toHaveBeenCalledTimes(2);

    act(() => {
      unmount();
    });

    expect(getInvokeCallsFor("pty_spawn")).toHaveLength(1);
    expect(getInvokeCallsFor("pty_kill").length).toBeGreaterThanOrEqual(1);
    expect(mocks.terminalDispose.mock.calls.length).toBeGreaterThanOrEqual(1);
  });
});
