import { invoke } from "@tauri-apps/api/core";

// Write plain text to the system clipboard via the custom Tauri command, which
// is backed by the Win32 SetClipboardData(CF_UNICODETEXT) path in
// splice-clipboard. Mirrors the invoke-wrapper pattern in ptyClient.ts.
export function writeClipboardText(text: string) {
  return invoke<void>("clipboard_write_text", {
    text,
  });
}
