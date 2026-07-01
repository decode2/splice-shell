import { describe, expect, it } from "vitest";
import {
  ACTIVE_PASTE_TARGET_COMMAND,
  activePasteTargetToState,
} from "./activePasteTarget";

describe("active paste target helpers", () => {
  it("keeps the active paste target command name explicit", () => {
    expect(ACTIVE_PASTE_TARGET_COMMAND).toBe("active_paste_target");
  });

  it("maps supported targets to ready state", () => {
    expect(
      activePasteTargetToState({
        processName: "codex.exe",
        adapterName: "codex-cli",
        supported: true,
      }),
    ).toEqual({
      kind: "ready",
      processName: "codex.exe",
      adapterName: "codex-cli",
    });
  });

  it("maps missing adapters to unsupported state", () => {
    expect(
      activePasteTargetToState({
        processName: "unknown.exe",
        adapterName: null,
        supported: false,
      }),
    ).toEqual({
      kind: "unsupported",
      processName: "unknown.exe",
    });
  });
});
