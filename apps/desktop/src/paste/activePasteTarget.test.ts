import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ACTIVE_PASTE_TARGET_COMMAND,
  activePasteTargetToState,
  getActivePasteTarget,
} from "./activePasteTarget";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

afterEach(() => {
  invokeMock.mockReset();
});

describe("active paste target helpers", () => {
  it("keeps the active paste target command name explicit", () => {
    expect(ACTIVE_PASTE_TARGET_COMMAND).toBe("active_paste_target");
  });

  it("forwards a provided session id to the backend command", () => {
    invokeMock.mockResolvedValue({ processName: "cmd.exe", supported: false });
    void getActivePasteTarget(5);
    expect(invokeMock).toHaveBeenCalledWith(ACTIVE_PASTE_TARGET_COMMAND, { sessionId: 5 });
  });

  it("omits the session id when none is provided (mount-time None fallback)", () => {
    invokeMock.mockResolvedValue({ processName: "cmd.exe", supported: false });
    void getActivePasteTarget();
    expect(invokeMock).toHaveBeenCalledWith(ACTIVE_PASTE_TARGET_COMMAND, { sessionId: undefined });
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
