import { describe, expect, it } from "vitest";
import { createTerminalOutputFilter } from "./terminalOutputFilter";

// Byte-integrity guard for the synthetic DECSET 2026 filter.
//
// The filter's ONLY licensed mutation is inserting whole 8-byte
// `\x1b[?2026h`/`\x1b[?2026l` brackets. This fuzz test proves that property
// holds across many pseudo-random byte streams split at random chunk
// boundaries: it feeds each stream through a fresh filter, concatenates the
// output, and verifies (via an insertion-only alignment) that the output is the
// input with ONLY whole 2026 brackets inserted — no real byte dropped, altered,
// or reordered. This is what keeps Fixes 2/3 from ever breaking byte integrity.
//
// A FIXED-SEED deterministic PRNG (mulberry32) is used instead of Math.random so
// the corpus is reproducible run-to-run and does not depend on a global RNG that
// is unavailable in some harnesses.

const BEGIN_SYNC = "\x1b[?2026h";
const END_SYNC = "\x1b[?2026l";

// Small deterministic PRNG. Returns a float in [0, 1).
function mulberry32(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state = (state + 0x6d2b79f5) | 0;
    let t = Math.imul(state ^ (state >>> 15), 1 | state);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// Vocabulary of fragments: individual escape-forming bytes (so random
// combinations can accidentally build or corrupt CSI sequences), plus the exact
// sequences the filter cares about and real 2026 brackets.
const VOCAB = [
  "\x1b",
  "[",
  "?",
  "0",
  "1",
  "2",
  "5",
  "6",
  "9",
  ";",
  "h",
  "l",
  "a",
  "Z",
  "X",
  " ",
  "\r\n",
  // Target cursor show/hide sequences, including combined-param variants.
  "\x1b[?25l",
  "\x1b[?25h",
  "\x1b[?12;25h",
  "\x1b[?25;12h",
  // Real synchronized-output brackets.
  "\x1b[?2026h",
  "\x1b[?2026l",
  // Untracked private CSI + a couple of nested hides.
  "\x1b[?1049h",
  "\x1b[?1049l",
  "\x1b[?25l\x1b[?25l",
  "\x1b[K",
];

// Insertion-only alignment: assert `output` equals `input` with ONLY whole
// `\x1b[?2026h`/`\x1b[?2026l` brackets inserted. When an output position starts
// a 2026 bracket, it is either the same bracket present in the input (consume
// both) or an inserted one (skip 8 output bytes). Any other divergence means a
// real byte was dropped/altered/reordered — a byte-integrity violation.
function isInsertionOnly(input: string, output: string): boolean {
  const startsBracket = (s: string, at: number) =>
    s.startsWith(BEGIN_SYNC, at) || s.startsWith(END_SYNC, at);

  let i = 0;
  let j = 0;
  while (j < output.length) {
    if (startsBracket(output, j)) {
      // A real bracket present in both advances both cursors; otherwise the
      // output bracket is an insertion and only the output cursor advances.
      if (input.substr(i, 8) === output.substr(j, 8)) {
        i += 8;
      }
      j += 8;
      continue;
    }
    if (i < input.length && output[j] === input[i]) {
      i += 1;
      j += 1;
      continue;
    }
    return false;
  }
  return i === input.length;
}

function buildStream(rand: () => number): string {
  const tokenCount = 1 + Math.floor(rand() * 40);
  let stream = "";
  for (let index = 0; index < tokenCount; index += 1) {
    stream += VOCAB[Math.floor(rand() * VOCAB.length)];
  }
  return stream;
}

function splitIntoChunks(stream: string, rand: () => number): string[] {
  const chunks: string[] = [];
  let cursor = 0;
  while (cursor < stream.length) {
    // Chunk size 1..6 so target sequences are frequently cut mid-escape.
    const size = 1 + Math.floor(rand() * 6);
    chunks.push(stream.slice(cursor, cursor + size));
    cursor += size;
  }
  return chunks;
}

describe("terminal output filter byte-integrity fuzz", () => {
  it("only ever inserts whole 2026 brackets across 2000 random split streams", () => {
    const rand = mulberry32(0x9e3779b9); // fixed seed → reproducible corpus
    let checked = 0;

    for (let iteration = 0; iteration < 2000; iteration += 1) {
      const stream = buildStream(rand);
      const chunks = splitIntoChunks(stream, rand);

      const filter = createTerminalOutputFilter();
      let output = "";
      for (const chunk of chunks) {
        for (const action of filter.write(chunk)) {
          output += action.data;
        }
      }
      for (const action of filter.flush()) {
        output += action.data;
      }

      if (!isInsertionOnly(stream, output)) {
        // Surface the offending case with escaped control bytes.
        const escape = (s: string) => s.split("\x1b").join("\\x1b");
        throw new Error(
          `Byte integrity violated on iteration ${iteration}:\n` +
            `  input:  ${escape(stream)}\n` +
            `  output: ${escape(output)}`,
        );
      }
      checked += 1;
    }

    expect(checked).toBe(2000);
  });
});
