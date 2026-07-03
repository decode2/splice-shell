type TerminalKeyboardEvent = {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
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
