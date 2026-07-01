import { invoke } from "@tauri-apps/api/core";

export const PREVIEW_ACTIVE_CLIPBOARD_IMAGE_PASTE_COMMAND = "preview_active_clipboard_image_paste";

export type PastePreview =
  | {
      status: "ready";
      text: string;
      processName: string;
      adapterName: string;
    }
  | {
      status: "unsupportedImage";
      path: string;
      processName: string;
    };

export type PastePreviewState =
  | {
      kind: "idle";
      message: string;
    }
  | {
      kind: "ready";
      text: string;
      processName: string;
      adapterName: string;
    }
  | {
      kind: "unsupported";
      path: string;
      processName: string;
    }
  | {
      kind: "error";
      message: string;
    };

export function isPasteShortcut(event: Pick<KeyboardEvent, "key" | "ctrlKey" | "metaKey">) {
  return (event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "v";
}

export function pastePreviewToState(preview: PastePreview): PastePreviewState {
  if (preview.status === "ready") {
    return {
      kind: "ready",
      text: preview.text,
      processName: preview.processName,
      adapterName: preview.adapterName,
    };
  }

  return {
    kind: "unsupported",
    path: preview.path,
    processName: preview.processName,
  };
}

export function pastePreviewToTerminalInput(preview: PastePreview) {
  return preview.status === "ready" ? preview.text : undefined;
}

export async function previewClipboardImagePaste(processName: string) {
  return invoke<PastePreview>("preview_clipboard_image_paste", {
    processName,
  });
}

export async function previewActiveClipboardImagePaste() {
  return invoke<PastePreview>(PREVIEW_ACTIVE_CLIPBOARD_IMAGE_PASTE_COMMAND);
}
