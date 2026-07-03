import { useEffect, useState } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";

// The window operations the custom title bar needs, narrowed to a plain,
// injectable interface. Keeping this an interface (rather than reaching for
// getCurrentWindow() directly in components) makes the controls unit-testable
// with a mock and keeps the Tauri boundary in one place.
export type WindowChrome = {
  minimize: () => Promise<void>;
  toggleMaximize: () => Promise<void>;
  close: () => Promise<void>;
  isMaximized: () => Promise<boolean>;
  // Subscribes to window resize/maximize/restore; resolves to an unlisten fn.
  onResized: (handler: () => void) => Promise<() => void>;
};

// A no-op chrome so the app still renders (and tests still mount) outside a
// Tauri runtime — e.g. Vite dev in a plain browser or jsdom, where
// getCurrentWindow() throws because the IPC globals are absent.
const NOOP_WINDOW_CHROME: WindowChrome = {
  minimize: async () => {},
  toggleMaximize: async () => {},
  close: async () => {},
  isMaximized: async () => false,
  onResized: async () => () => {},
};

// Resolve the real window chrome lazily. getCurrentWindow() must never run at
// module scope: it throws when the Tauri IPC bridge is missing, which would
// crash the bundle in browser-dev/jsdom. Call this only inside effects/handlers
// and it degrades to a no-op when the runtime is not Tauri.
export function getWindowChrome(): WindowChrome {
  try {
    const win = getCurrentWindow();
    return {
      minimize: () => win.minimize(),
      toggleMaximize: () => win.toggleMaximize(),
      close: () => win.close(),
      isMaximized: () => win.isMaximized(),
      onResized: (handler) => win.onResized(() => handler()),
    };
  } catch {
    return NOOP_WINDOW_CHROME;
  }
}

// Tracks whether the OS window is maximized so the shell can toggle chrome
// (square vs. rounded corners, maximized inset) and the caption button can swap
// its glyph. Syncs once on mount, then re-checks on every resize event.
//
// StrictMode-safe: the same disposed-flag + unlisten-if-resolved-after-dispose
// pattern TerminalView uses for its listen() chain. The discarded first mount
// must never setState after cleanup, and its resize listener must be torn down
// even if onResized() resolves after the effect was already disposed.
export function useWindowMaximized(chrome: WindowChrome): boolean {
  const [isMaximized, setIsMaximized] = useState(false);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;

    const syncMaximized = () => {
      void chrome
        .isMaximized()
        .then((value) => {
          if (!disposed) {
            setIsMaximized(value);
          }
        })
        .catch(() => {});
    };

    syncMaximized();

    void chrome
      .onResized(syncMaximized)
      .then((dispose) => {
        // The effect may have been disposed while onResized() was still pending
        // (StrictMode's synchronous mount → cleanup → mount). Tear the listener
        // down immediately instead of leaking it.
        if (disposed) {
          dispose();
          return;
        }
        unlisten = dispose;
      })
      .catch(() => {});

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [chrome]);

  return isMaximized;
}
