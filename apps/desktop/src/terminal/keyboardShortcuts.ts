type TerminalKeyboardEvent = {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
  // Auto-repeat flag set by the browser while a key is held down. Only the tab
  // chords consult it (a held Ctrl+T must not spam new tabs); the terminal
  // chords ignore it because holding Ctrl+C to send repeated SIGINTs is valid.
  repeat?: boolean;
};

export type TerminalKeyAction = "copy" | "paste" | "interrupt" | "none";

// Resolve the intent of a terminal keydown. Copy vs. interrupt is selection-aware
// so a bare Ctrl+C still sends SIGINT (\x03) when nothing is selected — matching
// Windows Terminal's default "copy falls through to the app when there's no
// selection" behavior. Ctrl+Shift+C and Ctrl+Insert are unconditional copy
// chords. Paste is Ctrl+V or Shift+Insert (Windows Terminal parity); it must be
// intercepted here because xterm otherwise maps Ctrl+V to the C0 byte \x16 and
// cancels the keydown, so no DOM paste event ever fires. Ctrl+Shift+V is
// deliberately NOT a paste chord. Modifier-laden variants (Alt/Meta) and
// unrelated keys are left alone.
export function resolveTerminalKeyAction(
  event: TerminalKeyboardEvent,
  hasSelection: boolean,
): TerminalKeyAction {
  if (event.altKey || event.metaKey) {
    return "none";
  }

  const key = event.key.toLowerCase();

  if (event.ctrlKey && key === "c") {
    if (event.shiftKey) {
      return "copy";
    }

    return hasSelection ? "copy" : "interrupt";
  }

  if (event.ctrlKey && !event.shiftKey && key === "insert") {
    return "copy";
  }

  if (event.ctrlKey && !event.shiftKey && key === "v") {
    return "paste";
  }

  if (!event.ctrlKey && event.shiftKey && key === "insert") {
    return "paste";
  }

  return "none";
}

export function isTerminalInterruptShortcut(event: TerminalKeyboardEvent) {
  return resolveTerminalKeyAction(event, false) === "interrupt";
}

// App-scoped tab chords, resolved separately from the terminal chords so the
// two key-sets can be proven disjoint (a key must never be both a terminal
// action AND a tab action, or the active terminal would swallow tab navigation).
//   Ctrl+T           → new-tab
//   Ctrl+W           → close-tab
//   Ctrl+Tab         → next-tab
//   Ctrl+Shift+Tab   → prev-tab
// These fire on a window-level capture listener BEFORE TerminalView's element
// listener, so preventDefault/stopPropagation in App keeps them out of the PTY.
export type TabKeyAction = "new-tab" | "close-tab" | "next-tab" | "prev-tab" | "none";

export function resolveTabKeyAction(event: TerminalKeyboardEvent): TabKeyAction {
  // A held key auto-repeats: guard here (unit-testable) so holding Ctrl+T does
  // not machine-gun new tabs. The guard lives in the resolver, not the handler.
  if (event.repeat) {
    return "none";
  }

  // Alt/Meta variants are platform/menu chords, never tab actions.
  if (event.altKey || event.metaKey) {
    return "none";
  }

  if (!event.ctrlKey) {
    return "none";
  }

  const key = event.key.toLowerCase();

  // Ctrl+Tab / Ctrl+Shift+Tab cycle; Shift only distinguishes direction here.
  if (key === "tab") {
    return event.shiftKey ? "prev-tab" : "next-tab";
  }

  // New/close are plain Ctrl chords (no Shift), matching browser conventions.
  if (event.shiftKey) {
    return "none";
  }

  if (key === "t") {
    return "new-tab";
  }

  if (key === "w") {
    return "close-tab";
  }

  return "none";
}
