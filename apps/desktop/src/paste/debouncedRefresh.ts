type TimeoutHandle = ReturnType<typeof setTimeout>;

export type DebouncedRefresh = {
  schedule: () => void;
  cancel: () => void;
};

export type DebouncedRefreshOptions = {
  delayMs: number;
  refresh: () => void | Promise<void>;
  setTimeoutFn?: (handler: () => void, delayMs: number) => TimeoutHandle;
  clearTimeoutFn?: (handle: TimeoutHandle) => void;
};

export function createDebouncedRefresh({
  delayMs,
  refresh,
  setTimeoutFn = setTimeout,
  clearTimeoutFn = clearTimeout,
}: DebouncedRefreshOptions): DebouncedRefresh {
  let pendingTimeout: TimeoutHandle | undefined;

  return {
    schedule: () => {
      if (pendingTimeout) {
        clearTimeoutFn(pendingTimeout);
      }

      pendingTimeout = setTimeoutFn(() => {
        pendingTimeout = undefined;
        void refresh();
      }, delayMs);
    },
    cancel: () => {
      if (!pendingTimeout) {
        return;
      }

      clearTimeoutFn(pendingTimeout);
      pendingTimeout = undefined;
    },
  };
}
