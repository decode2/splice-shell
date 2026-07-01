export type TerminalOutputScheduler = {
  write: (chunk: string) => void;
  flush: () => void;
  dispose: () => void;
};

type TerminalOutputSchedulerOptions = {
  write: (chunk: string) => void;
  requestFrame?: (callback: FrameRequestCallback) => number;
  cancelFrame?: (handle: number) => void;
};

export function createTerminalOutputScheduler({
  cancelFrame = (handle) => window.cancelAnimationFrame(handle),
  requestFrame = (callback) => window.requestAnimationFrame(callback),
  write,
}: TerminalOutputSchedulerOptions): TerminalOutputScheduler {
  let disposed = false;
  let frameHandle: number | undefined;
  let pendingOutput = "";

  const flush = () => {
    frameHandle = undefined;
    if (!pendingOutput || disposed) {
      return;
    }

    const output = pendingOutput;
    pendingOutput = "";
    write(output);
  };

  return {
    write: (chunk) => {
      if (disposed || !chunk) {
        return;
      }

      pendingOutput += chunk;
      frameHandle ??= requestFrame(flush);
    },
    flush,
    dispose: () => {
      disposed = true;
      if (frameHandle !== undefined) {
        cancelFrame(frameHandle);
        frameHandle = undefined;
      }
      pendingOutput = "";
    },
  };
}
