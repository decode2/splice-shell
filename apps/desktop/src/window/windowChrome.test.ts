// @vitest-environment jsdom
import { StrictMode } from "react";
import { act, renderHook, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { useWindowFocused, useWindowMaximized, type WindowChrome } from "./windowChrome";

function noopChrome(overrides: Partial<WindowChrome> = {}): WindowChrome {
  return {
    minimize: vi.fn(async () => {}),
    toggleMaximize: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
    isMaximized: vi.fn(async () => false),
    onResized: vi.fn(async () => () => {}),
    onFocusChanged: vi.fn(async () => () => {}),
    ...overrides,
  };
}

// Flush the microtask queue so the hook's isMaximized()/onResized() promises settle.
async function flush() {
  await act(async () => {
    await Promise.resolve();
  });
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("useWindowMaximized", () => {
  it("syncs the initial maximized state from chrome.isMaximized()", async () => {
    const chrome = noopChrome({ isMaximized: vi.fn(async () => true) });

    const { result } = renderHook(() => useWindowMaximized(chrome));

    await waitFor(() => expect(result.current).toBe(true));
  });

  it("re-checks the maximized state when the window is resized", async () => {
    let resizeHandler: (() => void) | undefined;
    let maximized = false;
    const chrome = noopChrome({
      isMaximized: vi.fn(async () => maximized),
      onResized: vi.fn(async (handler: () => void) => {
        resizeHandler = handler;
        return () => {};
      }),
    });

    const { result } = renderHook(() => useWindowMaximized(chrome));

    await waitFor(() => expect(resizeHandler).toBeDefined());
    expect(result.current).toBe(false);

    // The window is now maximized; the resize event must trigger a re-check.
    maximized = true;
    await act(async () => {
      resizeHandler?.();
      await Promise.resolve();
    });

    await waitFor(() => expect(result.current).toBe(true));
  });

  it("tears down every resize listener across StrictMode's double mount without leaking state updates", async () => {
    const unlistens: Array<ReturnType<typeof vi.fn>> = [];
    const onResized = vi.fn(async () => {
      const unlisten = vi.fn();
      unlistens.push(unlisten);
      return unlisten;
    });
    const chrome = noopChrome({ isMaximized: vi.fn(async () => false), onResized });
    // A setState after dispose or an unhandled rejection would surface as a
    // console.error; assert it stays silent to prove the disposed guard holds.
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    const { unmount } = renderHook(() => useWindowMaximized(chrome), {
      wrapper: StrictMode,
    });
    await flush();
    act(() => {
      unmount();
    });
    await flush();

    // StrictMode mounts the effect twice, so onResized is registered twice.
    expect(onResized).toHaveBeenCalledTimes(2);
    expect(unlistens).toHaveLength(2);
    // The discarded first mount is disposed by the resolved-after-dispose guard;
    // the live mount by the unmount cleanup. Each listener is torn down once.
    for (const unlisten of unlistens) {
      expect(unlisten).toHaveBeenCalledTimes(1);
    }
    expect(errorSpy).not.toHaveBeenCalled();
  });
});

describe("useWindowFocused", () => {
  it("assumes focused on mount (the app opens focused, no sync getter exists)", async () => {
    const chrome = noopChrome();

    const { result } = renderHook(() => useWindowFocused(chrome));

    // Initial state is true immediately, before any focus event arrives.
    expect(result.current).toBe(true);
    await flush();
    expect(result.current).toBe(true);
  });

  it("flips to false on blur and back to true on focus", async () => {
    let focusHandler: ((focused: boolean) => void) | undefined;
    const chrome = noopChrome({
      onFocusChanged: vi.fn(async (handler: (focused: boolean) => void) => {
        focusHandler = handler;
        return () => {};
      }),
    });

    const { result } = renderHook(() => useWindowFocused(chrome));

    await waitFor(() => expect(focusHandler).toBeDefined());
    expect(result.current).toBe(true);

    // The window lost OS focus.
    act(() => {
      focusHandler?.(false);
    });
    await waitFor(() => expect(result.current).toBe(false));

    // Focus returns.
    act(() => {
      focusHandler?.(true);
    });
    await waitFor(() => expect(result.current).toBe(true));
  });

  it("tears down every focus listener across StrictMode's double mount without leaking state updates", async () => {
    const unlistens: Array<ReturnType<typeof vi.fn>> = [];
    const onFocusChanged = vi.fn(async () => {
      const unlisten = vi.fn();
      unlistens.push(unlisten);
      return unlisten;
    });
    const chrome = noopChrome({ onFocusChanged });
    // A setState after dispose or an unhandled rejection would surface as a
    // console.error; assert it stays silent to prove the disposed guard holds.
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    const { unmount } = renderHook(() => useWindowFocused(chrome), {
      wrapper: StrictMode,
    });
    await flush();
    act(() => {
      unmount();
    });
    await flush();

    // StrictMode mounts the effect twice, so onFocusChanged is registered twice.
    expect(onFocusChanged).toHaveBeenCalledTimes(2);
    expect(unlistens).toHaveLength(2);
    // The discarded first mount is disposed by the resolved-after-dispose guard;
    // the live mount by the unmount cleanup. Each listener is torn down once.
    for (const unlisten of unlistens) {
      expect(unlisten).toHaveBeenCalledTimes(1);
    }
    expect(errorSpy).not.toHaveBeenCalled();
  });

  it("does not setState after dispose when a focus event fires post-unmount", async () => {
    let focusHandler: ((focused: boolean) => void) | undefined;
    const chrome = noopChrome({
      onFocusChanged: vi.fn(async (handler: (focused: boolean) => void) => {
        focusHandler = handler;
        return () => {};
      }),
    });
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});

    const { result, unmount } = renderHook(() => useWindowFocused(chrome));
    await waitFor(() => expect(focusHandler).toBeDefined());

    act(() => {
      unmount();
    });
    // A late event after unmount must be ignored (no setState-after-dispose).
    act(() => {
      focusHandler?.(false);
    });
    await flush();

    expect(result.current).toBe(true);
    expect(errorSpy).not.toHaveBeenCalled();
  });
});
