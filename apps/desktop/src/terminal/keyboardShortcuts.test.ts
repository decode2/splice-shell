import { describe, expect, it } from "vitest";
import {
  isTerminalInterruptShortcut,
  resolveTabKeyAction,
  resolveTerminalKeyAction,
} from "./keyboardShortcuts";

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

  it("does nothing for a bare key", () => {
    expect(resolveTerminalKeyAction(keyboardEvent({ ctrlKey: false }), true)).toBe("none");
  });

  it("pastes on Ctrl+V regardless of selection", () => {
    expect(resolveTerminalKeyAction(keyboardEvent({ key: "v" }), true)).toBe("paste");
    expect(resolveTerminalKeyAction(keyboardEvent({ key: "v" }), false)).toBe("paste");
  });

  it("pastes on Shift+Insert (Windows Terminal parity)", () => {
    expect(
      resolveTerminalKeyAction(keyboardEvent({ key: "Insert", ctrlKey: false, shiftKey: true }), true),
    ).toBe("paste");
    expect(
      resolveTerminalKeyAction(keyboardEvent({ key: "Insert", ctrlKey: false, shiftKey: true }), false),
    ).toBe("paste");
  });

  it("does not paste on Ctrl+Shift+V (only Ctrl+V and Shift+Insert paste)", () => {
    expect(
      resolveTerminalKeyAction(keyboardEvent({ key: "v", shiftKey: true }), true),
    ).toBe("none");
  });

  it("does nothing for platform or menu chord variants", () => {
    expect(resolveTerminalKeyAction(keyboardEvent({ metaKey: true }), true)).toBe("none");
    expect(resolveTerminalKeyAction(keyboardEvent({ altKey: true }), true)).toBe("none");
    expect(resolveTerminalKeyAction(keyboardEvent({ key: "Insert", altKey: true }), true)).toBe(
      "none",
    );
    // Ctrl+V with Alt or Meta must not paste — those are platform/menu chords.
    expect(resolveTerminalKeyAction(keyboardEvent({ key: "v", altKey: true }), true)).toBe("none");
    expect(resolveTerminalKeyAction(keyboardEvent({ key: "v", metaKey: true }), true)).toBe("none");
  });
});

describe("resolveTabKeyAction", () => {
  it("maps Ctrl+T to new-tab", () => {
    expect(resolveTabKeyAction(keyboardEvent({ key: "t" }))).toBe("new-tab");
  });

  it("maps Ctrl+W to close-tab", () => {
    expect(resolveTabKeyAction(keyboardEvent({ key: "w" }))).toBe("close-tab");
  });

  it("maps Ctrl+Tab to next-tab and Ctrl+Shift+Tab to prev-tab", () => {
    expect(resolveTabKeyAction(keyboardEvent({ key: "Tab" }))).toBe("next-tab");
    expect(resolveTabKeyAction(keyboardEvent({ key: "Tab", shiftKey: true }))).toBe("prev-tab");
  });

  it("returns none when the key auto-repeats (held key must not spam actions)", () => {
    // event.repeat === true means the key is being held; every chord must no-op
    // so holding Ctrl+T does not machine-gun new tabs.
    expect(resolveTabKeyAction(keyboardEvent({ key: "t", repeat: true }))).toBe("none");
    expect(resolveTabKeyAction(keyboardEvent({ key: "w", repeat: true }))).toBe("none");
    expect(resolveTabKeyAction(keyboardEvent({ key: "Tab", repeat: true }))).toBe("none");
    expect(
      resolveTabKeyAction(keyboardEvent({ key: "Tab", shiftKey: true, repeat: true })),
    ).toBe("none");
  });

  it("ignores tab chords without Ctrl, and platform/menu variants", () => {
    expect(resolveTabKeyAction(keyboardEvent({ key: "t", ctrlKey: false }))).toBe("none");
    expect(resolveTabKeyAction(keyboardEvent({ key: "w", ctrlKey: false }))).toBe("none");
    expect(resolveTabKeyAction(keyboardEvent({ key: "t", altKey: true }))).toBe("none");
    expect(resolveTabKeyAction(keyboardEvent({ key: "t", metaKey: true }))).toBe("none");
    // Ctrl+Shift+T / Ctrl+Shift+W are NOT tab chords in this design.
    expect(resolveTabKeyAction(keyboardEvent({ key: "t", shiftKey: true }))).toBe("none");
    expect(resolveTabKeyAction(keyboardEvent({ key: "w", shiftKey: true }))).toBe("none");
  });

  it("shares no key with a terminal chord (copy/paste/interrupt never doubles as a tab action)", () => {
    // Cross-check the two resolvers over the union of every chord key: no single
    // event may resolve to BOTH a terminal action and a tab action, or the
    // active terminal would eat tab navigation (and vice versa).
    const keys = ["c", "v", "t", "w", "insert", "tab"];
    for (const key of keys) {
      for (const shiftKey of [false, true]) {
        const event = keyboardEvent({ key, ctrlKey: true, shiftKey });
        const terminalAction = resolveTerminalKeyAction(event, true);
        const tabAction = resolveTabKeyAction(event);
        const isTerminal = terminalAction !== "none";
        const isTab = tabAction !== "none";
        expect(isTerminal && isTab).toBe(false);
      }
    }
  });
});
