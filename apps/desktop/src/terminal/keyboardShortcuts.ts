type TerminalKeyboardEvent = {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
};

export type TerminalKeyAction = "copy" | "interrupt" | "none";

// Resolve the intent of a terminal keydown. Copy vs. interrupt is selection-aware
// so a bare Ctrl+C still sends SIGINT (\x03) when nothing is selected — matching
// Windows Terminal's default "copy falls through to the app when there's no
// selection" behavior. Ctrl+Shift+C and Ctrl+Insert are unconditional copy
// chords. Modifier-laden variants (Alt/Meta) and unrelated keys are left alone.
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

  return "none";
}

export function isTerminalInterruptShortcut(event: TerminalKeyboardEvent) {
  return resolveTerminalKeyAction(event, false) === "interrupt";
}
