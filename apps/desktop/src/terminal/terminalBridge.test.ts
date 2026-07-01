import { describe, expect, it, vi } from "vitest";
import {
  createTerminalBridge,
  type Disposable,
  type FitAddonLike,
  type TerminalLike,
} from "./terminalBridge";

describe("createTerminalBridge", () => {
  it("opens, fits, writes initial output, and forwards terminal events", () => {
    const terminal = new FakeTerminal();
    const fitAddon = new FakeFitAddon();
    const container = {} as HTMLElement;
    const onInput = vi.fn();
    const onResize = vi.fn();
    const focusListener = new FakeDisposable();
    const containerResizeObserver = new FakeDisposable();

    createTerminalBridge({
      terminal,
      fitAddon,
      container,
      initialOutput: "Splice Shell\r\n",
      onInput,
      onResize,
      addContainerFocusListener: () => focusListener,
      addContainerResizeObserver: () => containerResizeObserver,
      addWindowResizeListener: () => ({ dispose: vi.fn() }),
    });

    terminal.emitData("abc");
    terminal.emitResize({ cols: 100, rows: 32 });

    expect(terminal.openedWith).toBe(container);
    expect(fitAddon.fitCalls).toBe(1);
    expect(terminal.focusCalls).toBe(1);
    expect(terminal.writes).toEqual(["Splice Shell\r\n"]);
    expect(onInput).toHaveBeenCalledWith("abc");
    expect(onResize).toHaveBeenCalledWith({ cols: 100, rows: 32 });
  });

  it("disposes every subscription and terminal resource exactly once", () => {
    const terminal = new FakeTerminal();
    const fitAddon = new FakeFitAddon();
    const resizeListener = new FakeDisposable();
    const focusListener = new FakeDisposable();
    const containerResizeObserver = new FakeDisposable();
    const bridge = createTerminalBridge({
      terminal,
      fitAddon,
      container: {} as HTMLElement,
      onInput: vi.fn(),
      onResize: vi.fn(),
      addContainerFocusListener: () => focusListener,
      addContainerResizeObserver: () => containerResizeObserver,
      addWindowResizeListener: () => resizeListener,
    });

    bridge.dispose();

    expect(resizeListener.disposeCalls).toBe(1);
    expect(containerResizeObserver.disposeCalls).toBe(1);
    expect(focusListener.disposeCalls).toBe(1);
    expect(terminal.dataSubscription.disposeCalls).toBe(1);
    expect(terminal.resizeSubscription.disposeCalls).toBe(1);
    expect(fitAddon.disposeCalls).toBe(1);
    expect(terminal.disposeCalls).toBe(1);
  });

  it("refits when the window resize listener fires", () => {
    const terminal = new FakeTerminal();
    const fitAddon = new FakeFitAddon();
    let resizeHandler: (() => void) | undefined;

    createTerminalBridge({
      terminal,
      fitAddon,
      container: {} as HTMLElement,
      onInput: vi.fn(),
      onResize: vi.fn(),
      addContainerFocusListener: () => ({ dispose: vi.fn() }),
      addContainerResizeObserver: () => ({ dispose: vi.fn() }),
      addWindowResizeListener: (handler) => {
        resizeHandler = handler;
        return { dispose: vi.fn() };
      },
    });

    resizeHandler?.();

    expect(fitAddon.fitCalls).toBe(2);
  });

  it("focuses when the terminal container receives pointer input", () => {
    const terminal = new FakeTerminal();
    const fitAddon = new FakeFitAddon();
    let focusHandler: (() => void) | undefined;

    createTerminalBridge({
      terminal,
      fitAddon,
      container: {} as HTMLElement,
      onInput: vi.fn(),
      onResize: vi.fn(),
      addContainerFocusListener: (handler) => {
        focusHandler = handler;
        return { dispose: vi.fn() };
      },
      addContainerResizeObserver: () => ({ dispose: vi.fn() }),
      addWindowResizeListener: () => ({ dispose: vi.fn() }),
    });

    focusHandler?.();

    expect(terminal.focusCalls).toBe(2);
  });

  it("refits when the terminal container resizes", () => {
    const terminal = new FakeTerminal();
    const fitAddon = new FakeFitAddon();
    let resizeHandler: (() => void) | undefined;

    createTerminalBridge({
      terminal,
      fitAddon,
      container: {} as HTMLElement,
      onInput: vi.fn(),
      onResize: vi.fn(),
      addContainerFocusListener: () => ({ dispose: vi.fn() }),
      addContainerResizeObserver: (handler) => {
        resizeHandler = handler;
        return { dispose: vi.fn() };
      },
      addWindowResizeListener: () => ({ dispose: vi.fn() }),
    });

    resizeHandler?.();

    expect(fitAddon.fitCalls).toBe(2);
  });
});

class FakeTerminal implements TerminalLike {
  openedWith: HTMLElement | undefined;
  writes: string[] = [];
  disposeCalls = 0;
  focusCalls = 0;
  dataSubscription = new FakeDisposable();
  resizeSubscription = new FakeDisposable();
  private dataHandler: ((data: string) => void) | undefined;
  private resizeHandler: ((size: { cols: number; rows: number }) => void) | undefined;

  open(element: HTMLElement) {
    this.openedWith = element;
  }

  focus() {
    this.focusCalls += 1;
  }

  write(data: string, callback?: () => void) {
    this.writes.push(data);
    callback?.();
  }

  onData(handler: (data: string) => void): Disposable {
    this.dataHandler = handler;
    return this.dataSubscription;
  }

  onResize(handler: (size: { cols: number; rows: number }) => void): Disposable {
    this.resizeHandler = handler;
    return this.resizeSubscription;
  }

  dispose() {
    this.disposeCalls += 1;
  }

  emitData(data: string) {
    this.dataHandler?.(data);
  }

  emitResize(size: { cols: number; rows: number }) {
    this.resizeHandler?.(size);
  }

}

class FakeFitAddon implements FitAddonLike {
  fitCalls = 0;
  disposeCalls = 0;

  fit() {
    this.fitCalls += 1;
  }

  dispose() {
    this.disposeCalls += 1;
  }
}

class FakeDisposable implements Disposable {
  disposeCalls = 0;

  dispose() {
    this.disposeCalls += 1;
  }
}
