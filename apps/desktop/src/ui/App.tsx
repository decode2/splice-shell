import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { activePasteTargetToState, getActivePasteTarget } from "../paste/activePasteTarget";
import { createDebouncedRefresh } from "../paste/debouncedRefresh";
import {
  pastePreviewToState,
  pastePreviewToTerminalInput,
  previewActiveClipboardImagePaste,
  type PastePreviewState,
} from "../paste/pastePreview";
import { resolveTabKeyAction } from "../terminal/keyboardShortcuts";
import { writePty } from "../terminal/ptyClient";
import { getWindowChrome, useWindowFocused, useWindowMaximized } from "../window/windowChrome";
import { TerminalSettingsPanel } from "./TerminalSettingsPanel";
import { DEFAULT_TERMINAL_SETTINGS, type TerminalSettings } from "./terminalSettings";
import { TitleBar, type SessionHealth } from "./TitleBar";
import { useSessions } from "./useSessions";

const TerminalView = lazy(() =>
  import("./TerminalView").then((module) => ({ default: module.TerminalView })),
);

export function App() {
  const disposedRef = useRef(false);
  // The tab model: an ordered list of tabs (each with its own sessionId,
  // adapter, health) plus the active tab id. All tabs stay mounted; only the
  // active one is visible/focused.
  const {
    tabs,
    activeId,
    createTab,
    closeTab,
    setActive,
    cycleTab,
    recordSession,
    recordHealth,
    recordAdapter,
  } = useSessions();

  // One window-chrome instance shared by the maximized/focused hooks and the
  // title bar's controls, so getCurrentWindow() is resolved once (lazily).
  const chrome = useMemo(() => getWindowChrome(), []);
  const isMaximized = useWindowMaximized(chrome);
  const isFocused = useWindowFocused(chrome);

  // Settings are GLOBAL/shared: one source of truth in App, passed to every
  // mounted terminal, edited through a single TerminalSettingsPanel. A change
  // refits ALL terminals (each TerminalView's settings effect refits on change).
  const [terminalSettings, setTerminalSettings] = useState<TerminalSettings>(
    DEFAULT_TERMINAL_SETTINGS,
  );
  const [settingsOpen, setSettingsOpen] = useState(false);
  const toggleSettings = useCallback(() => setSettingsOpen((current) => !current), []);
  const closeSettings = useCallback(() => setSettingsOpen(false), []);

  const [pasteState, setPasteState] = useState<PastePreviewState>({
    kind: "idle",
    message: "Press Ctrl+V with an image in the clipboard to preview the paste route.",
  });

  // Refs mirror the ACTIVE tab so stable callbacks (paste, adapter refresh, the
  // window chord handler) always target the current active tab/session without
  // being rebound on every switch. Assigned in effects below so the mirror
  // tracks state commits.
  const activeIdRef = useRef(activeId);
  const activeSessionIdRef = useRef<number | undefined>(undefined);
  const activeTab = tabs.find((tab) => tab.tabId === activeId);
  const activeSessionId = activeTab?.sessionId;
  // Declared BEFORE the refresh effect so the mirrors are up to date before any
  // refresh reads them.
  useEffect(() => {
    activeIdRef.current = activeId;
  }, [activeId]);
  useEffect(() => {
    activeSessionIdRef.current = activeSessionId;
  }, [activeSessionId]);

  // Refresh the ACTIVE tab's adapter chip only (inactive tabs keep their
  // last-known adapter). Records into the active tab via the reducer so each
  // tab owns its chip independently.
  const refreshActivePasteTarget = useCallback(async () => {
    const tabId = activeIdRef.current;
    try {
      const target = await getActivePasteTarget(activeSessionIdRef.current);
      if (!disposedRef.current) {
        recordAdapter(tabId, activePasteTargetToState(target));
      }
    } catch (error) {
      if (!disposedRef.current) {
        recordAdapter(tabId, {
          kind: "error",
          message: error instanceof Error ? error.message : String(error),
        });
      }
    }
  }, [recordAdapter]);

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
    return () => {
      disposedRef.current = true;
      debouncedActivePasteTargetRefresh.cancel();
    };
  }, [debouncedActivePasteTargetRefresh]);

  // Refresh on mount and whenever the active tab changes, so switching tabs
  // updates the newly-active tab's chip.
  useEffect(() => {
    void refreshActivePasteTarget();
  }, [activeId, refreshActivePasteTarget]);

  const pasteClipboardImageIntoTerminal = useCallback(async () => {
    try {
      const preview = await previewActiveClipboardImagePaste(activeSessionIdRef.current);
      setPasteState(pastePreviewToState(preview));
      const terminalInput = pastePreviewToTerminalInput(preview);
      if (terminalInput) {
        await writePty(terminalInput, activeSessionIdRef.current ?? 0);
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
      await writePty(text, activeSessionIdRef.current ?? 0);
      debouncedActivePasteTargetRefresh.schedule();
    },
    [debouncedActivePasteTargetRefresh],
  );

  // Per-tab PTY-ready: record the (re)spawned session id for that tab. When it
  // is the active tab, mirror the session id immediately and refresh its chip.
  const handlePtyReady = useCallback(
    (tabId: string, sessionId: number) => {
      recordSession(tabId, sessionId);
      if (tabId === activeIdRef.current) {
        activeSessionIdRef.current = sessionId;
        void refreshActivePasteTarget();
      }
    },
    [recordSession, refreshActivePasteTarget],
  );

  const handleTabSessionHealth = useCallback(
    (tabId: string, status: SessionHealth) => {
      recordHealth(tabId, status);
    },
    [recordHealth],
  );

  // Tab chords are app-scoped: a window-level capture listener fires BEFORE
  // TerminalView's element listener, so the active terminal never swallows tab
  // navigation. The resolver already ignores auto-repeat and disjoint keys.
  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      const action = resolveTabKeyAction(event);
      if (action === "none") {
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      switch (action) {
        case "new-tab":
          createTab();
          break;
        case "close-tab":
          closeTab(activeIdRef.current);
          break;
        case "next-tab":
          cycleTab("next");
          break;
        case "prev-tab":
          cycleTab("prev");
          break;
      }
    };

    window.addEventListener("keydown", handleKeyDown, { capture: true });
    return () => window.removeEventListener("keydown", handleKeyDown, { capture: true });
  }, [createTab, closeTab, cycleTab]);

  return (
    <main
      className="app-shell"
      data-maximized={isMaximized || undefined}
      data-focused={isFocused || undefined}
    >
      <TitleBar
        tabs={tabs}
        activeId={activeId}
        onSelectTab={setActive}
        onCloseTab={closeTab}
        onCreateTab={createTab}
        pasteState={pasteState}
        settingsOpen={settingsOpen}
        onToggleSettings={toggleSettings}
        isMaximized={isMaximized}
        chrome={chrome}
      />
      <div className="terminal-stack">
        <Suspense
          fallback={<div className="terminal-frame terminal-loading">Loading terminal UI…</div>}
        >
          {tabs.map((tab) => (
            <TerminalView
              key={tab.tabId}
              active={tab.tabId === activeId}
              visible={tab.tabId === activeId}
              settings={terminalSettings}
              onClipboardImagePaste={pasteClipboardImageIntoTerminal}
              onInputActivity={debouncedActivePasteTargetRefresh.schedule}
              onSessionHealth={(status) => handleTabSessionHealth(tab.tabId, status)}
              onTextPaste={pasteTextIntoTerminal}
              onPtyReady={(sessionId) => handlePtyReady(tab.tabId, sessionId)}
            />
          ))}
        </Suspense>
        {settingsOpen ? (
          <TerminalSettingsPanel
            settings={terminalSettings}
            onChange={setTerminalSettings}
            onClose={closeSettings}
          />
        ) : null}
      </div>
    </main>
  );
}
