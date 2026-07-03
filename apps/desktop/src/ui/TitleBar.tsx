import type { ActivePasteTargetState } from "../paste/activePasteTarget";
import type { PastePreviewState } from "../paste/pastePreview";
import { getWindowChrome, type WindowChrome } from "../window/windowChrome";

export type ConnectionState = {
  input: boolean;
  output: boolean;
};

type TitleBarProps = {
  connection: ConnectionState;
  activePasteTargetState: ActivePasteTargetState;
  pasteState: PastePreviewState;
  settingsOpen: boolean;
  onToggleSettings: () => void;
  isMaximized: boolean;
  // Injectable for tests; defaults to the real (or no-op) window chrome.
  chrome?: WindowChrome;
};

// The custom (undecorated) window title bar. Because native decorations are
// off, this bar owns dragging AND the window controls.
//
// Dragging: Tauri's drag handler checks `data-tauri-drag-region` on the EXACT
// event target, not its ancestors, and it does NOT honor Electron's CSS
// `-webkit-app-region`. So the attribute goes on the header AND every inert
// child the user might grab (brand, status wrapper, status text). Buttons omit
// it so clicks act instead of starting a drag.
export function TitleBar({
  connection,
  activePasteTargetState,
  pasteState,
  settingsOpen,
  onToggleSettings,
  isMaximized,
  chrome = getWindowChrome(),
}: TitleBarProps) {
  return (
    <header className="titlebar" data-tauri-drag-region>
      <span className="titlebar-brand" data-tauri-drag-region>
        Splice Shell
      </span>
      <div className="titlebar-status" data-tauri-drag-region>
        <span data-tauri-drag-region>
          ConPTY · input {connection.input ? "yes" : "waiting"} · output{" "}
          {connection.output ? "yes" : "waiting"}
        </span>
        <ActivePasteTargetPanel activePasteTargetState={activePasteTargetState} />
        <PastePreviewPanel pasteState={pasteState} />
      </div>
      <button
        className="terminal-settings-toggle"
        type="button"
        aria-expanded={settingsOpen}
        onClick={onToggleSettings}
      >
        Settings
      </button>
      <WindowControls chrome={chrome} isMaximized={isMaximized} />
    </header>
  );
}

function WindowControls({
  chrome,
  isMaximized,
}: {
  chrome: WindowChrome;
  isMaximized: boolean;
}) {
  return (
    <div className="window-controls" data-tauri-drag-region>
      <button
        className="window-control"
        type="button"
        aria-label="Minimize"
        onClick={() => void chrome.minimize()}
      >
        <MinimizeGlyph />
      </button>
      <button
        className="window-control"
        type="button"
        aria-label={isMaximized ? "Restore" : "Maximize"}
        onClick={() => void chrome.toggleMaximize()}
      >
        {isMaximized ? <RestoreGlyph /> : <MaximizeGlyph />}
      </button>
      <button
        className="window-control window-control-close"
        type="button"
        aria-label="Close"
        onClick={() => void chrome.close()}
      >
        <CloseGlyph />
      </button>
    </div>
  );
}

function MinimizeGlyph() {
  return (
    <svg
      className="window-glyph window-glyph-minimize"
      viewBox="0 0 10 10"
      aria-hidden="true"
      focusable="false"
    >
      <line x1="0" y1="5" x2="10" y2="5" stroke="currentColor" strokeWidth="1" />
    </svg>
  );
}

function MaximizeGlyph() {
  return (
    <svg
      className="window-glyph window-glyph-maximize"
      viewBox="0 0 10 10"
      aria-hidden="true"
      focusable="false"
    >
      <rect
        x="0.5"
        y="0.5"
        width="9"
        height="9"
        fill="none"
        stroke="currentColor"
        strokeWidth="1"
      />
    </svg>
  );
}

function RestoreGlyph() {
  // Two offset squares: the classic Windows "restore down" glyph.
  return (
    <svg
      className="window-glyph window-glyph-restore"
      viewBox="0 0 10 10"
      aria-hidden="true"
      focusable="false"
    >
      <rect
        x="0.5"
        y="2.5"
        width="7"
        height="7"
        fill="none"
        stroke="currentColor"
        strokeWidth="1"
      />
      <path d="M2.5 2.5 V0.5 H9.5 V7.5 H7.5" fill="none" stroke="currentColor" strokeWidth="1" />
    </svg>
  );
}

function CloseGlyph() {
  return (
    <svg
      className="window-glyph window-glyph-close"
      viewBox="0 0 10 10"
      aria-hidden="true"
      focusable="false"
    >
      <line x1="0.5" y1="0.5" x2="9.5" y2="9.5" stroke="currentColor" strokeWidth="1" />
      <line x1="9.5" y1="0.5" x2="0.5" y2="9.5" stroke="currentColor" strokeWidth="1" />
    </svg>
  );
}

// Moved verbatim from TerminalView — these panels are pure, prop-fed status
// readouts that now live in the title bar's status cluster. Each root element
// carries data-tauri-drag-region so grabbing the status text still drags the
// window (Tauri matches the exact target, not ancestors).
function ActivePasteTargetPanel({
  activePasteTargetState,
}: {
  activePasteTargetState: ActivePasteTargetState;
}) {
  if (activePasteTargetState.kind === "ready") {
    return (
      <p className="paste-preview paste-target muted" data-tauri-drag-region>
        Active paste target: {activePasteTargetState.adapterName} /{" "}
        {activePasteTargetState.processName}
      </p>
    );
  }

  if (activePasteTargetState.kind === "unsupported") {
    return (
      <p className="paste-preview paste-target warning" data-tauri-drag-region>
        Active paste target unsupported: {activePasteTargetState.processName}
      </p>
    );
  }

  return (
    <p className="paste-preview paste-target muted" data-tauri-drag-region>
      {activePasteTargetState.message}
    </p>
  );
}

function PastePreviewPanel({ pasteState }: { pasteState: PastePreviewState }) {
  if (pasteState.kind === "idle") {
    return (
      <p className="paste-preview paste-route muted" data-tauri-drag-region>
        {pasteState.message}
      </p>
    );
  }

  if (pasteState.kind === "ready") {
    return (
      <div className="paste-preview paste-route success" data-tauri-drag-region>
        <span data-tauri-drag-region>
          Adapter {pasteState.adapterName} selected for {pasteState.processName}:
        </span>
        <code data-tauri-drag-region>{pasteState.text}</code>
      </div>
    );
  }

  if (pasteState.kind === "unsupported") {
    return (
      <p className="paste-preview paste-route warning" data-tauri-drag-region>
        Image was extracted, but the active process is unsupported: {pasteState.processName}.{" "}
        {pasteState.path}
      </p>
    );
  }

  return (
    <p className="paste-preview paste-route warning" data-tauri-drag-region>
      {pasteState.message}
    </p>
  );
}
