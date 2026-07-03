import { describe, expect, it } from "vitest";
import {
  createTerminalOutputFilter,
  type TerminalOutputAction,
} from "./terminalOutputFilter";

// Byte-integrity guard for the synthetic DECSET 2026 filter WITH cursor-show
// holdback.
//
// The filter's licensed mutations are exactly two: (1) inserting whole 8-byte
// `\x1b[?2026h`/`\x1b[?2026l` brackets, and (2) DEFERRING or RE-EMITTING mode-25
// cursor-shows (never dropping them). Every other byte — crucially including
// every HIDE — must pass through byte-identical and in positional order. This
// fuzz test proves that property across many pseudo-random byte streams split at
// random chunk boundaries, with the injected quiet timer fired at random points
// (and sometimes never), the deferred sink spliced into the transcript at its
// actual emission time, and always a final flush().
//
// A FIXED-SEED deterministic PRNG (mulberry32) keeps the corpus reproducible
// run-to-run without depending on a global RNG unavailable in some harnesses.

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

// --- Private-CSI classification helpers (self-contained, mirrors the filter's
// grammar without importing its internals). ---

type CsiAt = { length: number; final: "h" | "l"; params: string[] };

function matchCsiAt(s: string, p: number): CsiAt | null {
  if (s[p] !== "\x1b" || s[p + 1] !== "[" || s[p + 2] !== "?") {
    return null;
  }
  let k = p + 3;
  while (k < s.length && /[0-9;]/.test(s[k])) {
    k += 1;
  }
  if (k >= s.length) {
    return null;
  }
  const final = s[k];
  if (final !== "h" && final !== "l") {
    return null;
  }
  return { length: k - p + 1, final, params: s.slice(p + 3, k).split(";") };
}

const isBracketAt = (s: string, p: number) =>
  s.startsWith(BEGIN_SYNC, p) || s.startsWith(END_SYNC, p);

// A mode-25 SHOW: private CSI whose params include 25 with final `h`.
function showAt(s: string, p: number): CsiAt | null {
  const m = matchCsiAt(s, p);
  return m && m.final === "h" && m.params.includes("25") ? m : null;
}

// A mode-25 HIDE: private CSI whose params include 25 with final `l`.
function hideAt(s: string, p: number): CsiAt | null {
  const m = matchCsiAt(s, p);
  return m && m.final === "l" && m.params.includes("25") ? m : null;
}

// The normative conservation checker (replaces the old insertion-only alignment).
//
// Two-pointer alignment (input cursor `i`, output cursor `j`) with a FIFO
// deferred-show queue `Q` of exact SHOW byte strings. Per iteration the FIRST
// matching rule wins:
//   1. output[j] starts a SHOW and Q non-empty → it MUST byte-equal Q head; pop,
//      advance j. (Queue takes precedence over an identical input SHOW — FIFO.)
//   2. input[i] starts a SHOW, Q empty, output[j] is the identical SHOW →
//      in-place passthrough (consume both; covers no-sink/disabled mode).
//   3. input[i] starts a SHOW otherwise unmatched → push its bytes to Q, advance
//      i (deferral). This is attempted BEFORE the bracket rule so a show that is
//      deferred past a REAL 2026 bracket lets that bracket realign at input[i+…]
//      instead of being misread as a synthetic insertion.
//   4. output[j] starts a 2026 bracket → if input[i] is the identical bracket,
//      passthrough (consume both); otherwise a licensed insertion (advance j 8).
//   5. input[i] starts a HIDE → bytes MUST match at j; consume both; ASSERT Q is
//      EMPTY (emit-before-hide guarantees the held show was re-emitted first —
//      this catches a show leaking past a hide, the walk-reintroducing bug).
//   6. otherwise input[i] MUST equal output[j] → consume both; mismatch = FAIL.
// End state: i === input.length AND j === output.length AND Q empty.
function conservesBytes(input: string, output: string): boolean {
  let i = 0;
  let j = 0;
  const queue: string[] = [];

  while (i < input.length || j < output.length) {
    // Rule 1 — deferred-show release matches the FIFO queue head.
    if (j < output.length && queue.length > 0 && showAt(output, j)) {
      const head = queue[0];
      if (!output.startsWith(head, j)) {
        return false;
      }
      queue.shift();
      j += head.length;
      continue;
    }

    // Rule 2 — in-place show passthrough (no deferral pending).
    if (i < input.length && queue.length === 0) {
      const inShow = showAt(input, i);
      if (inShow) {
        const bytes = input.substr(i, inShow.length);
        if (j < output.length && output.startsWith(bytes, j)) {
          i += inShow.length;
          j += inShow.length;
          continue;
        }
      }
    }

    // Rule 3 — input SHOW otherwise unmatched: defer. Attempted before the
    // bracket rule so a real 2026 bracket following a deferred show realigns.
    if (i < input.length) {
      const inShow = showAt(input, i);
      if (inShow) {
        queue.push(input.substr(i, inShow.length));
        i += inShow.length;
        continue;
      }
    }

    // Rule 4 — output bracket (insertion or passthrough).
    if (j < output.length && isBracketAt(output, j)) {
      if (input.substr(i, 8) === output.substr(j, 8)) {
        i += 8;
      }
      j += 8;
      continue;
    }

    // Rule 5 — input HIDE: must match at output, and Q must be empty.
    if (i < input.length) {
      const inHide = hideAt(input, i);
      if (inHide) {
        const bytes = input.substr(i, inHide.length);
        if (j >= output.length || !output.startsWith(bytes, j)) {
          return false;
        }
        if (queue.length > 0) {
          return false; // a show leaked past a hide — byte loss / walk revival
        }
        i += inHide.length;
        j += inHide.length;
        continue;
      }
    }

    // Rule 6 — literal byte match.
    if (i < input.length && j < output.length && input[i] === output[j]) {
      i += 1;
      j += 1;
      continue;
    }

    return false;
  }

  return i === input.length && j === output.length && queue.length === 0;
}

// Final intended mode-25 visibility: scan mode-25 SHOW/HIDE events in order and
// return the last one's final byte ("h" visible, "l" hidden), or null if none.
// 2026 brackets are ignored (their param is 2026, never 25).
function netMode25Visibility(s: string): "h" | "l" | null {
  let last: "h" | "l" | null = null;
  let p = 0;
  while (p < s.length) {
    const escIndex = s.indexOf("\x1b", p);
    if (escIndex === -1) {
      break;
    }
    const show = showAt(s, escIndex);
    const hide = hideAt(s, escIndex);
    if (show) {
      last = "h";
      p = escIndex + show.length;
      continue;
    }
    if (hide) {
      last = "l";
      p = escIndex + hide.length;
      continue;
    }
    p = escIndex + 1;
  }
  return last;
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

// A deterministic single-slot fake timer for the fuzz harness. The filter arms
// at most one quiet timer at a time (it clears before setting), which this
// models. `fire()` runs the pending callback exactly as a real timer would.
function createFuzzTimer() {
  let pending: { fn: () => void; id: number } | null = null;
  let nextId = 1;
  return {
    timer: {
      set: (fn: () => void) => {
        const id = nextId;
        nextId += 1;
        pending = { fn, id };
        return id;
      },
      clear: (handle: unknown) => {
        if (pending && pending.id === handle) {
          pending = null;
        }
      },
    },
    isArmed: () => pending !== null,
    fire: () => {
      if (pending) {
        const { fn } = pending;
        pending = null;
        fn();
      }
    },
  };
}

// Negative-control self-tests (RED-first): prove the conservation checker
// actually catches byte loss, so a green fuzz run means something. Each feeds a
// hand-mutated output the filter must NEVER produce; the checker MUST reject it.
describe("conservation checker negative controls", () => {
  it("accepts a legitimate deferred-then-released show (sanity baseline)", () => {
    // Input show, held, released later mid-stream after content: conserved.
    expect(conservesBytes("\x1b[?25habc", "abc\x1b[?25h")).toBe(true);
    // Emit-before-hide: input show-then-hide, output re-emits show inline before
    // the hide, then a synthetic BEGIN_SYNC.
    expect(
      conservesBytes("\x1b[?25h\x1b[?25l", "\x1b[?25h\x1b[?25l\x1b[?2026h"),
    ).toBe(true);
  });

  it("rejects a dropped hide", () => {
    // The hide between 'x' and 'y' vanished from the output — the walk-reintroducing
    // bug class. class (a) requires every hide to survive positionally.
    expect(conservesBytes("x\x1b[?25ly", "xy")).toBe(false);
  });

  it("rejects a duplicated show", () => {
    // The show was emitted twice; only one exists in the input.
    expect(conservesBytes("\x1b[?25h", "\x1b[?25h\x1b[?25h")).toBe(false);
  });

  it("rejects a show emitted after its corresponding hide", () => {
    // Input is show-then-hide; a correct filter re-emits the held show INLINE
    // before the hide. Here the show leaks to AFTER the hide — the exact bug the
    // assert-Q-empty-on-HIDE rule exists to catch.
    expect(conservesBytes("\x1b[?25h\x1b[?25l", "\x1b[?25l\x1b[?25h")).toBe(false);
  });
});

describe("terminal output filter byte-integrity fuzz", () => {
  it("conserves bytes and net visibility across 2000 random split streams with holdback", () => {
    const rand = mulberry32(0x9e3779b9); // fixed seed → reproducible corpus
    let checked = 0;

    for (let iteration = 0; iteration < 2000; iteration += 1) {
      const stream = buildStream(rand);
      const chunks = splitIntoChunks(stream, rand);

      const fake = createFuzzTimer();
      let output = "";
      const emit = (actions: TerminalOutputAction[]) => {
        for (const action of actions) {
          output += action.data;
        }
      };
      // The sink splices deferred emissions into the transcript at their ACTUAL
      // emission time (whenever fire() runs), never appended at the end.
      const filter = createTerminalOutputFilter({
        onDeferredOutput: emit,
        timer: fake.timer,
      });

      for (const chunk of chunks) {
        emit(filter.write(chunk));
        // Fire the quiet timer at random points between chunks (and, by not
        // firing, sometimes never before flush).
        if (fake.isArmed() && rand() < 0.35) {
          fake.fire();
        }
      }
      // Sometimes fire once more just before flush; sometimes leave it for flush.
      if (fake.isArmed() && rand() < 0.5) {
        fake.fire();
      }
      emit(filter.flush());

      if (!conservesBytes(stream, output)) {
        const escape = (s: string) => s.split("\x1b").join("\\x1b");
        throw new Error(
          `Byte conservation violated on iteration ${iteration}:\n` +
            `  input:  ${escape(stream)}\n` +
            `  output: ${escape(output)}`,
        );
      }

      // Net cursor visibility after quiet must equal the input's final intent.
      if (netMode25Visibility(output) !== netMode25Visibility(stream)) {
        const escape = (s: string) => s.split("\x1b").join("\\x1b");
        throw new Error(
          `Net mode-25 visibility diverged on iteration ${iteration}:\n` +
            `  input:  ${escape(stream)} (${netMode25Visibility(stream)})\n` +
            `  output: ${escape(output)} (${netMode25Visibility(output)})`,
        );
      }

      checked += 1;
    }

    expect(checked).toBe(2000);
  });
});
