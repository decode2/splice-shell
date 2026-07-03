import { describe, expect, it } from "vitest";
import { isTerminalInterruptShortcut, resolveTerminalKeyAction } from "./keyboardShortcuts";

const keyboardEvent = (
  overrides: Partial<Parameters<typeof isTerminalInterruptShortcut>[0]> = {},
) => ({
  key: "c",
  ctrlKey: true,
  shiftKey: false,
  altKey: false,
  metaKey: false,
  ...overrides,
});

describe("keyboard shortcuts", () => {
  it("treats Ctrl+C as a terminal interrupt", () => {
    expect(isTerminalInterruptShortcut(keyboardEvent())).toBe(true);
  });

  it("does not intercept Ctrl+Shift+C so copy can keep working", () => {
    expect(isTerminalInterruptShortcut(keyboardEvent({ shiftKey: true }))).toBe(false);
  });

  it("does not intercept platform or menu shortcuts", () => {
    expect(isTerminalInterruptShortcut(keyboardEvent({ metaKey: true }))).toBe(false);
    expect(isTerminalInterruptShortcut(keyboardEvent({ altKey: true }))).toBe(false);
  });
});

describe("resolveTerminalKeyAction", () => {
  it("copies on Ctrl+C when there is a selection", () => {
    expect(resolveTerminalKeyAction(keyboardEvent(), true)).toBe("copy");
  });

  it("interrupts on Ctrl+C when there is no selection", () => {
    expect(resolveTerminalKeyAction(keyboardEvent(), false)).toBe("interrupt");
  });

  it("copies on Ctrl+Shift+C regardless of selection", () => {
    expect(resolveTerminalKeyAction(keyboardEvent({ shiftKey: true }), true)).toBe("copy");
    expect(resolveTerminalKeyAction(keyboardEvent({ shiftKey: true }), false)).toBe("copy");
  });

  it("copies on Ctrl+Insert regardless of selection", () => {
    expect(resolveTerminalKeyAction(keyboardEvent({ key: "Insert" }), true)).toBe("copy");
    expect(resolveTerminalKeyAction(keyboardEvent({ key: "Insert" }), false)).toBe("copy");
  });

  it("does nothing for Shift+Insert (paste) or a bare key", () => {
    expect(
      resolveTerminalKeyAction(keyboardEvent({ key: "Insert", ctrlKey: false, shiftKey: true }), true),
    ).toBe("none");
    expect(resolveTerminalKeyAction(keyboardEvent({ ctrlKey: false }), true)).toBe("none");
  });

  it("does nothing for platform or menu chord variants", () => {
    expect(resolveTerminalKeyAction(keyboardEvent({ metaKey: true }), true)).toBe("none");
    expect(resolveTerminalKeyAction(keyboardEvent({ altKey: true }), true)).toBe("none");
    expect(resolveTerminalKeyAction(keyboardEvent({ key: "Insert", altKey: true }), true)).toBe(
      "none",
    );
  });
});
