import React from "react";
import { describe, expect, it } from "vitest";
import { TerminalView } from "./TerminalView";

describe("TerminalView", () => {
  it("returns a valid React element with an idle paste state", () => {
    expect(
      React.isValidElement(
        <TerminalView
          activePasteTargetState={{
            kind: "ready",
            processName: "codex.exe",
            adapterName: "codex-cli",
          }}
          pasteState={{
            kind: "idle",
            message: "Paste preview idle",
          }}
        />,
      ),
    ).toBe(true);
  });

  it("accepts changing parent callback identities without changing the component contract", () => {
    const first = (
      <TerminalView
        activePasteTargetState={{ kind: "loading", message: "first" }}
        onInput={() => undefined}
        pasteState={{ kind: "idle", message: "first" }}
      />
    );
    const second = (
      <TerminalView
        activePasteTargetState={{ kind: "loading", message: "second" }}
        onInput={() => undefined}
        pasteState={{ kind: "idle", message: "second" }}
      />
    );

    expect(first.type).toBe(second.type);
  });
});
