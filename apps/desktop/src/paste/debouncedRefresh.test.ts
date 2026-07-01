import { describe, expect, it, vi } from "vitest";
import { createDebouncedRefresh } from "./debouncedRefresh";

describe("createDebouncedRefresh", () => {
  it("keeps only the latest scheduled refresh", () => {
    const refresh = vi.fn();
    const clearTimeoutFn = vi.fn();
    const handlers: Array<() => void> = [];
    const setTimeoutFn = vi.fn((handler: () => void) => {
      handlers.push(handler);
      return handlers.length;
    });

    const debouncedRefresh = createDebouncedRefresh({
      delayMs: 250,
      refresh,
      setTimeoutFn,
      clearTimeoutFn,
    });

    debouncedRefresh.schedule();
    debouncedRefresh.schedule();

    expect(setTimeoutFn).toHaveBeenCalledTimes(2);
    expect(clearTimeoutFn).toHaveBeenCalledWith(1);

    handlers[1]();

    expect(refresh).toHaveBeenCalledTimes(1);
  });

  it("cancels a pending refresh", () => {
    const refresh = vi.fn();
    const clearTimeoutFn = vi.fn();
    const setTimeoutFn = vi.fn((handler: () => void) => {
      void handler;
      return setTimeoutFn.mock.calls.length;
    });

    const debouncedRefresh = createDebouncedRefresh({
      delayMs: 250,
      refresh,
      setTimeoutFn,
      clearTimeoutFn,
    });

    debouncedRefresh.schedule();
    debouncedRefresh.cancel();
    debouncedRefresh.schedule();

    expect(clearTimeoutFn).toHaveBeenCalledWith(1);
    expect(clearTimeoutFn).toHaveBeenCalledTimes(1);
  });
});
