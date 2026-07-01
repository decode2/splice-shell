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

const TerminalView = lazy(() =>
  import("./TerminalView").then((module) => ({ default: module.TerminalView })),
);

export function App() {
  const disposedRef = useRef(false);
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
    <main className="app-shell">
      <header className="app-toolbar">
        <div>
          <p className="eyebrow">Splice Shell</p>
          <h1>Terminal</h1>
        </div>
      </header>
      <Suspense fallback={<div className="terminal-frame terminal-loading">Loading terminal UI…</div>}>
        <TerminalView
          activePasteTargetState={activePasteTargetState}
          onClipboardImagePaste={pasteClipboardImageIntoTerminal}
          onInputActivity={debouncedActivePasteTargetRefresh.schedule}
          onTextPaste={pasteTextIntoTerminal}
          onPtyReady={refreshActivePasteTarget}
          pasteState={pasteState}
        />
      </Suspense>
    </main>
  );
}
