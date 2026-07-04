import type { TerminalSettings } from "./terminalSettings";

// The single, global settings overlay. Rendered ONCE by App (not per tab), so
// one panel edits the shared terminal settings that every mounted terminal
// consumes. Kept in its own module (no xterm imports) so App can statically
// import it without pulling the heavy TerminalView chunk into the main bundle —
// TerminalView stays lazy-loaded.
export function TerminalSettingsPanel({
  onChange,
  onClose,
  settings,
}: {
  onChange: (settings: TerminalSettings) => void;
  onClose: () => void;
  settings: TerminalSettings;
}) {
  return (
    <aside className="terminal-settings-panel" aria-label="Terminal settings">
      <div className="settings-panel-header">
        <strong>Terminal settings</strong>
        <button type="button" onClick={onClose}>
          Close
        </button>
      </div>
      <label>
        Text
        <input
          type="color"
          value={settings.foreground}
          onChange={(event) => onChange({ ...settings, foreground: event.currentTarget.value })}
        />
      </label>
      <label>
        Background
        <input
          type="color"
          value={settings.background}
          onChange={(event) => onChange({ ...settings, background: event.currentTarget.value })}
        />
      </label>
      <label>
        Font size
        <input
          max="22"
          min="10"
          type="number"
          value={settings.fontSize}
          onChange={(event) =>
            onChange({
              ...settings,
              fontSize: Number(event.currentTarget.value),
            })
          }
        />
      </label>
    </aside>
  );
}
