import { afterEach, describe, expect, it, vi } from "vitest";
import {
  isPasteShortcut,
  pastePreviewToState,
  pastePreviewToTerminalInput,
  previewActiveClipboardImagePaste,
  PREVIEW_ACTIVE_CLIPBOARD_IMAGE_PASTE_COMMAND,
} from "./pastePreview";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

afterEach(() => {
  invokeMock.mockReset();
});

describe("paste preview helpers", () => {
  it("keeps the active paste command name explicit", () => {
    expect(PREVIEW_ACTIVE_CLIPBOARD_IMAGE_PASTE_COMMAND).toBe(
      "preview_active_clipboard_image_paste",
    );
  });

  it("forwards a provided session id to the active preview command", () => {
    invokeMock.mockResolvedValue({ status: "unsupportedImage", path: "x", processName: "cmd.exe" });
    void previewActiveClipboardImagePaste(5);
    expect(invokeMock).toHaveBeenCalledWith(PREVIEW_ACTIVE_CLIPBOARD_IMAGE_PASTE_COMMAND, {
      sessionId: 5,
    });
  });

  it("omits the session id when none is provided", () => {
    invokeMock.mockResolvedValue({ status: "unsupportedImage", path: "x", processName: "cmd.exe" });
    void previewActiveClipboardImagePaste();
    expect(invokeMock).toHaveBeenCalledWith(PREVIEW_ACTIVE_CLIPBOARD_IMAGE_PASTE_COMMAND, {
      sessionId: undefined,
    });
  });

  it("detects Ctrl+V and Cmd+V paste shortcuts", () => {
    expect(isPasteShortcut({ key: "v", ctrlKey: true, metaKey: false })).toBe(true);
    expect(isPasteShortcut({ key: "V", ctrlKey: false, metaKey: true })).toBe(true);
    expect(isPasteShortcut({ key: "c", ctrlKey: true, metaKey: false })).toBe(false);
  });

  it("maps ready preview responses to terminal-ready text state", () => {
    expect(
      pastePreviewToState({
        status: "ready",
        text: "Image file: C:/Temp/image.png\r",
        processName: "codex.exe",
        adapterName: "codex-cli",
      }),
    ).toEqual({
      kind: "ready",
      text: "Image file: C:/Temp/image.png\r",
      processName: "codex.exe",
      adapterName: "codex-cli",
    });
  });

  it("maps unsupported preview responses without guessing terminal input", () => {
    expect(
      pastePreviewToState({
        status: "unsupportedImage",
        path: "C:/Temp/image.png",
        processName: "unknown.exe",
      }),
    ).toEqual({
      kind: "unsupported",
      path: "C:/Temp/image.png",
      processName: "unknown.exe",
    });
  });

  it("returns terminal input only for supported paste previews", () => {
    expect(
      pastePreviewToTerminalInput({
        status: "ready",
        text: "Image file: C:/Temp/image.png\r",
        processName: "codex.exe",
        adapterName: "codex-cli",
      }),
    ).toBe("Image file: C:/Temp/image.png\r");

    expect(
      pastePreviewToTerminalInput({
        status: "unsupportedImage",
        path: "C:/Temp/image.png",
        processName: "unknown.exe",
      }),
    ).toBeUndefined();
  });
});
