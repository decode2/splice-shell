import type { ActivePasteTargetState } from "../paste/activePasteTarget";
import type { PastePreviewState } from "../paste/pastePreview";
import { getWindowChrome, type WindowChrome } from "../window/windowChrome";

// Session health drives the title bar's single bold element: the health dot.
// It is deliberately three-state — the dot speaks ONLY when not "healthy".
export type SessionHealth = "healthy" | "reconnecting" | "failed";

type TitleBarProps = {
  activePasteTargetState: ActivePasteTargetState;
  pasteState: PastePreviewState;
  sessionHealth: SessionHealth;
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
// child the user might grab (brand, chip, health dot/label, status wrapper).
// Buttons and the settings icon omit it so clicks act instead of starting a
// drag.
export function TitleBar({
  activePasteTargetState,
  pasteState,
  sessionHealth,
  settingsOpen,
  onToggleSettings,
  isMaximized,
  chrome = getWindowChrome(),
}: TitleBarProps) {
  return (
    <header className="titlebar" data-tauri-drag-region>
      <span className="titlebar-brand" data-tauri-drag-region>
        <span className="titlebar-brand-mark" data-tauri-drag-region aria-hidden="true">
          ◈
        </span>
        <span className="titlebar-brand-name" data-tauri-drag-region>
          splice
        </span>
      </span>
      <div className="titlebar-status" data-tauri-drag-region>
        <AdapterChip activePasteTargetState={activePasteTargetState} />
        <HealthDot sessionHealth={sessionHealth} />
        {pasteState.kind !== "idle" ? <PastePreviewPanel pasteState={pasteState} /> : null}
      </div>
      <button
        className="titlebar-icon-button"
        type="button"
        aria-label="Settings"
        aria-expanded={settingsOpen}
        onClick={onToggleSettings}
      >
        <GearGlyph />
      </button>
      <WindowControls chrome={chrome} isMaximized={isMaximized} />
    </header>
  );
}

// Strips a Windows executable suffix so the chip reads as a shell/tool name
// ("pwsh", "cmd", "codex") rather than a raw process path.
function stripExecutableSuffix(name: string) {
  return name.replace(/\.exe$/i, "");
}

// The adapter chip answers "what am I talking to?" using a user-recognizable
// name. It renders only once the target resolves; loading/error states stay
// silent rather than surfacing diagnostic text.
function AdapterChip({
  activePasteTargetState,
}: {
  activePasteTargetState: ActivePasteTargetState;
}) {
  if (activePasteTargetState.kind === "ready") {
    return (
      <span className="titlebar-chip" data-tauri-drag-region>
        {activePasteTargetState.adapterName}
      </span>
    );
  }

  if (activePasteTargetState.kind === "unsupported") {
    return (
      <span className="titlebar-chip titlebar-chip--unsupported" data-tauri-drag-region>
        {stripExecutableSuffix(activePasteTargetState.processName)}
      </span>
    );
  }

  return null;
}

// The signature element. Quiet (a low-opacity emerald dot, no label) while
// healthy; it earns attention only when the session is degraded.
function HealthDot({ sessionHealth }: { sessionHealth: SessionHealth }) {
  const label =
    sessionHealth === "reconnecting"
      ? "reconnecting…"
      : sessionHealth === "failed"
        ? "session failed"
        : null;

  return (
    <span className="titlebar-health" data-tauri-drag-region>
      <span
        className={`titlebar-health-dot titlebar-health-dot--${sessionHealth}`}
        data-health={sessionHealth}
        data-tauri-drag-region
        aria-hidden="true"
      />
      {label ? (
        <span className="titlebar-health-label" data-tauri-drag-region>
          {label}
        </span>
      ) : null}
    </span>
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

function GearGlyph() {
  return (
    <svg
      className="titlebar-icon-glyph"
      viewBox="0 0 24 24"
      aria-hidden="true"
      focusable="false"
    >
      <path
        fill="currentColor"
        d="M19.14 12.94c.04-.3.06-.61.06-.94 0-.32-.02-.64-.07-.94l2.03-1.58a.49.49 0 0 0 .12-.61l-1.92-3.32a.49.49 0 0 0-.59-.22l-2.39.96c-.5-.38-1.03-.7-1.62-.94l-.36-2.54a.49.49 0 0 0-.48-.41h-3.84a.49.49 0 0 0-.47.41l-.36 2.54c-.59.24-1.13.57-1.62.94l-2.39-.96a.49.49 0 0 0-.59.22L2.74 8.87a.49.49 0 0 0 .12.61l2.03 1.58c-.05.3-.09.63-.09.94s.02.64.07.94l-2.03 1.58a.49.49 0 0 0-.12.61l1.92 3.32c.12.22.37.29.59.22l2.39-.96c.5.38 1.03.7 1.62.94l.36 2.54c.05.24.24.41.48.41h3.84c.24 0 .44-.17.47-.41l.36-2.54c.59-.24 1.13-.56 1.62-.94l2.39.96c.22.08.47 0 .59-.22l1.92-3.32a.49.49 0 0 0-.12-.61l-2.01-1.58zM12 15.6a3.6 3.6 0 1 1 0-7.2 3.6 3.6 0 0 1 0 7.2z"
      />
    </svg>
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

// Transient paste feedback. Rendered only when NOT idle (the caller guards on
// `pasteState.kind`), so the bar has no permanent hint clutter — error and
// result states appear briefly, then the bar returns to its quiet resting look.
// Each inert root carries data-tauri-drag-region so grabbing the text still
// drags the window (Tauri matches the exact target, not ancestors).
function PastePreviewPanel({ pasteState }: { pasteState: PastePreviewState }) {
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
