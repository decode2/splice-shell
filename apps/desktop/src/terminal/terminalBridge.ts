export type Disposable = {
  dispose: () => void;
};

export type TerminalLike = {
  open: (element: HTMLElement) => void;
  focus: () => void;
  write: (data: string, callback?: () => void) => void;
  onData: (handler: (data: string) => void) => Disposable;
  onResize: (handler: (size: { cols: number; rows: number }) => void) => Disposable;
  dispose: () => void;
};

export type FitAddonLike = {
  fit: () => void;
  dispose: () => void;
};

export type TerminalBridgeOptions = {
  terminal: TerminalLike;
  fitAddon: FitAddonLike;
  container: HTMLElement;
  initialOutput?: string;
  onInput: (data: string) => void;
  onResize: (size: { cols: number; rows: number }) => void;
  addContainerFocusListener?: (handler: () => void) => Disposable;
  addContainerResizeObserver?: (handler: () => void) => Disposable;
  addWindowResizeListener?: (handler: () => void) => Disposable;
};

export function createTerminalBridge({
  terminal,
  fitAddon,
  container,
  initialOutput,
  onInput,
  onResize,
  addContainerFocusListener = (handler) => defaultContainerFocusListener(container, handler),
  addContainerResizeObserver = (handler) => defaultContainerResizeObserver(container, handler),
  addWindowResizeListener = defaultWindowResizeListener,
}: TerminalBridgeOptions): Disposable {
  terminal.open(container);
  fitAddon.fit();
  terminal.focus();

  if (initialOutput) {
    terminal.write(initialOutput);
  }

  const inputSubscription = terminal.onData(onInput);
  const resizeSubscription = terminal.onResize(onResize);
  const refit = () => {
    fitAddon.fit();
  };
  const containerFocusSubscription = addContainerFocusListener(() => {
    terminal.focus();
  });
  const containerResizeSubscription = addContainerResizeObserver(refit);
  const windowResizeSubscription = addWindowResizeListener(refit);

  return {
    dispose: () => {
      windowResizeSubscription.dispose();
      containerResizeSubscription.dispose();
      containerFocusSubscription.dispose();
      resizeSubscription.dispose();
      inputSubscription.dispose();
      fitAddon.dispose();
      terminal.dispose();
    },
  };
}

function defaultContainerFocusListener(container: HTMLElement, handler: () => void): Disposable {
  container.addEventListener("pointerdown", handler);

  return {
    dispose: () => {
      container.removeEventListener("pointerdown", handler);
    },
  };
}

function defaultContainerResizeObserver(container: HTMLElement, handler: () => void): Disposable {
  if (typeof ResizeObserver === "undefined") {
    return {
      dispose: () => undefined,
    };
  }

  const observer = new ResizeObserver(handler);
  observer.observe(container);

  return {
    dispose: () => {
      observer.disconnect();
    },
  };
}

function defaultWindowResizeListener(handler: () => void): Disposable {
  window.addEventListener("resize", handler);

  return {
    dispose: () => {
      window.removeEventListener("resize", handler);
    },
  };
}
