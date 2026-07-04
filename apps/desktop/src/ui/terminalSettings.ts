// Shared terminal appearance settings. Kept in a non-component module so both
// TerminalView (which consumes them) and App (which owns them as global,
// per-window state and renders the single settings panel) can import the shape
// and its default without any component ↔ constant export coupling.
export type TerminalSettings = {
  background: string;
  foreground: string;
  fontSize: number;
};

// The single source of truth for default appearance. App seeds its shared
// `terminalSettings` state from this and passes it down to every tab, so a
// mounted TerminalView never owns divergent per-instance settings.
export const DEFAULT_TERMINAL_SETTINGS: TerminalSettings = {
  background: "#020617",
  foreground: "#dbeafe",
  fontSize: 14,
};
