import { invoke } from "@tauri-apps/api/core";
import type { ILink, ILinkProvider, Terminal } from "@xterm/xterm";

const BARE_WINDOWS_ABSOLUTE_PATH = /[A-Za-z]:[\\/][^\s"'<>|)\]}]+/g;
const FILE_URL_WINDOWS_PATH = /file:\/\/\/?[A-Za-z]:\/[^\s"'<>|)\]}]+/gi;
const QUOTED_WINDOWS_ABSOLUTE_PATH = /(["'`])([A-Za-z]:[\\/][^"'`<>|]+)\1/g;

export type LocalFileLinkMatch = {
  endIndex: number;
  startIndex: number;
  text: string;
};

export function createLocalFileLinkProvider(terminal: Terminal): ILinkProvider {
  return {
    provideLinks(bufferLineNumber, callback) {
      const line = terminal.buffer.active.getLine(bufferLineNumber - 1)?.translateToString(true);
      if (!line) {
        callback(undefined);
        return;
      }

      const links = extractLocalFileLinks(line).map(
        ({ endIndex, startIndex, text }): ILink => ({
          range: {
            start: {
              x: startIndex + 1,
              y: bufferLineNumber,
            },
            end: {
              x: endIndex + 1,
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
        }),
      );

      callback(links.length > 0 ? links : undefined);
    },
  };
}

export function extractLocalFileLinks(line: string): LocalFileLinkMatch[] {
  const links: LocalFileLinkMatch[] = [];
  const occupiedRanges: Array<{ endIndex: number; startIndex: number }> = [];

  for (const match of line.matchAll(QUOTED_WINDOWS_ABSOLUTE_PATH)) {
    const quotedPath = match[2];
    const quoteOffset = match[1].length;
    const startIndex = (match.index ?? 0) + quoteOffset;
    const text = trimTrailingPathPunctuation(quotedPath);
    const endIndex = startIndex + text.length;

    pushLocalFileLink(links, occupiedRanges, { endIndex, startIndex, text });
  }

  for (const match of line.matchAll(FILE_URL_WINDOWS_PATH)) {
    const rawUrl = match[0];
    const startIndex = match.index ?? 0;
    const text = normalizeFileUrlPath(rawUrl);
    const endIndex = startIndex + rawUrl.length;

    pushLocalFileLink(links, occupiedRanges, { endIndex, startIndex, text });
  }

  for (const match of line.matchAll(BARE_WINDOWS_ABSOLUTE_PATH)) {
    const startIndex = match.index ?? 0;
    const text = trimTrailingPathPunctuation(match[0]);
    const endIndex = startIndex + text.length;

    if (!overlapsAnyRange({ endIndex, startIndex }, occupiedRanges)) {
      pushLocalFileLink(links, occupiedRanges, { endIndex, startIndex, text });
    }
  }

  return links.sort((left, right) => left.startIndex - right.startIndex);
}

export function trimTrailingPathPunctuation(path: string) {
  return path.replace(/[.,;:!?]+$/u, "");
}

export function openLocalPath(path: string) {
  return invoke<void>("open_path", { path });
}

function normalizeFileUrlPath(rawUrl: string) {
  const path = rawUrl.replace(/^file:\/\/\/?/iu, "");
  return trimTrailingPathPunctuation(decodeURIComponent(path));
}

function pushLocalFileLink(
  links: LocalFileLinkMatch[],
  occupiedRanges: Array<{ endIndex: number; startIndex: number }>,
  link: LocalFileLinkMatch,
) {
  if (!link.text) {
    return;
  }

  links.push(link);
  occupiedRanges.push({
    endIndex: link.endIndex,
    startIndex: link.startIndex,
  });
}

function overlapsAnyRange(
  candidate: { endIndex: number; startIndex: number },
  ranges: Array<{ endIndex: number; startIndex: number }>,
) {
  return ranges.some(
    (range) => candidate.startIndex < range.endIndex && candidate.endIndex > range.startIndex,
  );
}
