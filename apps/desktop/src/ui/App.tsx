import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  activePasteTargetToState,
  getActivePasteTarget,
  type ActivePasteTargetState,
} from "../paste/activePasteTarget";
import { createDebouncedRefresh } from "../paste/debouncedRefresh";
import {
  pastePreviewToState,
  pastePreviewToTerminalInput,
  previewActiveClipboardImagePaste,
  type PastePreviewState,
} from "../paste/pastePreview";
import { writePty } from "../terminal/ptyClient";
import { getWindowChrome, useWindowFocused, useWindowMaximized } from "../window/windowChrome";
import { TitleBar, type SessionHealth } from "./TitleBar";

const TerminalView = lazy(() =>
  import("./TerminalView").then((module) => ({ default: module.TerminalView })),
);

export function App() {
  const disposedRef = useRef(false);
  // One window-chrome instance shared by the maximized hook and the title bar's
  // controls, so getCurrentWindow() is resolved once (lazily, Tauri-safe).
  const chrome = useMemo(() => getWindowChrome(), []);
  const isMaximized = useWindowMaximized(chrome);
  // Drives the title-bar dimming: on OS blur the chrome softens, on focus it
  // restores. Shares the one chrome instance created above.
  const isFocused = useWindowFocused(chrome);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const toggleSettings = useCallback(() => setSettingsOpen((current) => !current), []);
  const closeSettings = useCallback(() => setSettingsOpen(false), []);
  // Session health drives the title bar's health dot. It starts "healthy" and
  // only changes on real transitions (reconnecting / failed / back to healthy).
  // The setter no-ops when the status is unchanged, so a stream of healthy
  // output chunks cannot re-render the shell after the first transition.
  const [sessionHealth, setSessionHealth] = useState<SessionHealth>("healthy");
  const handleSessionHealth = useCallback((status: SessionHealth) => {
    setSessionHealth((current) => (current === status ? current : status));
  }, []);
  const [pasteState, setPasteState] = useState<PastePreviewState>({
    kind: "idle",
    message: "Press Ctrl+V with an image in the clipboard to preview the paste route.",
  });
  const [activePasteTargetState, setActivePasteTargetState] =
    useState<ActivePasteTargetState>({
      kind: "loading",
      message: "Detecting active paste target…",
    });
  const refreshActivePasteTarget = useCallback(async () => {
    try {
      const target = await getActivePasteTarget();
      if (!disposedRef.current) {
        setActivePasteTargetState(activePasteTargetToState(target));
      }
    } catch (error) {
      if (!disposedRef.current) {
        setActivePasteTargetState({
          kind: "error",
          message: error instanceof Error ? error.message : String(error),
        });
      }
    }
  }, []);
  const debouncedActivePasteTargetRefresh = useMemo(
    () =>
      createDebouncedRefresh({
        delayMs: 750,
        refresh: refreshActivePasteTarget,
      }),
    [refreshActivePasteTarget],
  );

  useEffect(() => {
    disposedRef.current = false;

    void refreshActivePasteTarget();

    return () => {
      disposedRef.current = true;
      debouncedActivePasteTargetRefresh.cancel();
    };
  }, [debouncedActivePasteTargetRefresh, refreshActivePasteTarget]);

  const pasteClipboardImageIntoTerminal = useCallback(async () => {
    try {
      const preview = await previewActiveClipboardImagePaste();
      setPasteState(pastePreviewToState(preview));
      const terminalInput = pastePreviewToTerminalInput(preview);
      if (terminalInput) {
        await writePty(terminalInput);
        debouncedActivePasteTargetRefresh.schedule();
      }
    } catch (error) {
      setPasteState({
        kind: "error",
        message: error instanceof Error ? error.message : String(error),
      });
    }
  }, [debouncedActivePasteTargetRefresh]);

  const pasteTextIntoTerminal = useCallback(
    async (text: string) => {
      await writePty(text);
      debouncedActivePasteTargetRefresh.schedule();
    },
    [debouncedActivePasteTargetRefresh],
  );

  return (
    <main
      className="app-shell"
      data-maximized={isMaximized || undefined}
      data-focused={isFocused || undefined}
    >
      <TitleBar
        activePasteTargetState={activePasteTargetState}
        pasteState={pasteState}
        sessionHealth={sessionHealth}
        settingsOpen={settingsOpen}
        onToggleSettings={toggleSettings}
        isMaximized={isMaximized}
        chrome={chrome}
      />
      <Suspense fallback={<div className="terminal-frame terminal-loading">Loading terminal UI…</div>}>
        <TerminalView
          onClipboardImagePaste={pasteClipboardImageIntoTerminal}
          onInputActivity={debouncedActivePasteTargetRefresh.schedule}
          onSessionHealth={handleSessionHealth}
          onTextPaste={pasteTextIntoTerminal}
          onPtyReady={refreshActivePasteTarget}
          settingsOpen={settingsOpen}
          onCloseSettings={closeSettings}
        />
      </Suspense>
    </main>
  );
}
