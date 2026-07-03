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
import { getWindowChrome, useWindowMaximized } from "../window/windowChrome";
import { TitleBar, type ConnectionState } from "./TitleBar";

const TerminalView = lazy(() =>
  import("./TerminalView").then((module) => ({ default: module.TerminalView })),
);

export function App() {
  const disposedRef = useRef(false);
  // One window-chrome instance shared by the maximized hook and the title bar's
  // controls, so getCurrentWindow() is resolved once (lazily, Tauri-safe).
  const chrome = useMemo(() => getWindowChrome(), []);
  const isMaximized = useWindowMaximized(chrome);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const toggleSettings = useCallback(() => setSettingsOpen((current) => !current), []);
  const closeSettings = useCallback(() => setSettingsOpen(false), []);
  // Latched ConPTY connection status shown in the title bar. The transition
  // guard returns the SAME object once a flag is already set, so a stream of
  // keystrokes/output chunks does not re-render the shell after the first one.
  const [connection, setConnection] = useState<ConnectionState>({
    input: false,
    output: false,
  });
  const handleConnectionActivity = useCallback((kind: "input" | "output") => {
    setConnection((current) => (current[kind] ? current : { ...current, [kind]: true }));
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
    <main className="app-shell" data-maximized={isMaximized || undefined}>
      <TitleBar
        connection={connection}
        activePasteTargetState={activePasteTargetState}
        pasteState={pasteState}
        settingsOpen={settingsOpen}
        onToggleSettings={toggleSettings}
        isMaximized={isMaximized}
        chrome={chrome}
      />
      <Suspense fallback={<div className="terminal-frame terminal-loading">Loading terminal UI…</div>}>
        <TerminalView
          onClipboardImagePaste={pasteClipboardImageIntoTerminal}
          onInputActivity={debouncedActivePasteTargetRefresh.schedule}
          onConnectionActivity={handleConnectionActivity}
          onTextPaste={pasteTextIntoTerminal}
          onPtyReady={refreshActivePasteTarget}
          settingsOpen={settingsOpen}
          onCloseSettings={closeSettings}
        />
      </Suspense>
    </main>
  );
}
