import { invoke } from "@tauri-apps/api/core";
import type { ILink, ILinkProvider, Terminal } from "@xterm/xterm";

const WINDOWS_ABSOLUTE_PATH = /[A-Za-z]:[\\/][^\s"'<>|)]+/g;

export function createLocalFileLinkProvider(terminal: Terminal): ILinkProvider {
  return {
    provideLinks(bufferLineNumber, callback) {
      const line = terminal.buffer.active.getLine(bufferLineNumber - 1)?.translateToString(true);
      if (!line) {
        callback(undefined);
        return;
      }

      const links = [...line.matchAll(WINDOWS_ABSOLUTE_PATH)].map((match): ILink => {
        const text = trimTrailingPathPunctuation(match[0]);
        const startIndex = match.index ?? 0;

        return {
          range: {
            start: {
              x: startIndex + 1,
              y: bufferLineNumber,
            },
            end: {
              x: startIndex + text.length + 1,
              y: bufferLineNumber,
            },
          },
          text,
          decorations: {
            pointerCursor: true,
            underline: true,
          },
          activate: (_event, path) => {
            void openLocalPath(path);
          },
        };
      });

      callback(links.length > 0 ? links : undefined);
    },
  };
}

export function trimTrailingPathPunctuation(path: string) {
  return path.replace(/[.,;:]+$/u, "");
}

export function openLocalPath(path: string) {
  return invoke<void>("open_path", { path });
}
