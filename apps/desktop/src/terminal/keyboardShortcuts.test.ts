import { describe, expect, it } from "vitest";
import { isTerminalInterruptShortcut } from "./keyboardShortcuts";

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
