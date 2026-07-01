type TerminalKeyboardEvent = {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
};

export function isTerminalInterruptShortcut(event: TerminalKeyboardEvent) {
  return (
    event.key.toLowerCase() === "c" &&
    event.ctrlKey &&
    !event.shiftKey &&
    !event.altKey &&
    !event.metaKey
  );
}
