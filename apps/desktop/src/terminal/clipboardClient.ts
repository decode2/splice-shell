import { invoke } from "@tauri-apps/api/core";

// Write plain text to the system clipboard via the custom Tauri command, which
// is backed by the Win32 SetClipboardData(CF_UNICODETEXT) path in
// splice-clipboard. Mirrors the invoke-wrapper pattern in ptyClient.ts.
export function writeClipboardText(text: string) {
  return invoke<void>("clipboard_write_text", {
    text,
  });
}

// Read plain text from the system clipboard via the custom Tauri command, backed
// by the Win32 GetClipboardData(CF_UNICODETEXT) path in splice-clipboard. Resolves
// to an empty string when the clipboard holds no text (e.g. only an image), so
// callers can fall back to the image paste route.
export async function readClipboardText(): Promise<string> {
  return await invoke<string>("clipboard_read_text");
}
