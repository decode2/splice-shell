export type TerminalOutputAction = {
  kind: "write";
  data: string;
};

export type TerminalOutputFilter = {
  write: (chunk: string) => TerminalOutputAction[];
  flush: () => TerminalOutputAction[];
  reset: () => void;
};

// The synthetic DECSET 2026 brackets we synthesize around each cursor-hidden
// span. These are the ONLY bytes the filter ever inserts; everything else
// passes through byte-identical and in order.
const BEGIN_SYNC = "\x1b[?2026h";
const END_SYNC = "\x1b[?2026l";

// Parameter section of a private-mode CSI: a run of digits and semicolons.
const PARAM_BYTE = /[0-9;]/;

type PrivateCsiMatch =
  // A proper prefix of a private-mode CSI sitting at the buffer tail: it could
  // still complete into a tracked sequence once the next chunk arrives, so it
  // must be held back verbatim.
  | { kind: "incomplete" }
  // A complete private-mode CSI `\x1b[?<params>(h|l)`.
  | { kind: "csi"; length: number; final: "h" | "l"; params: string[] }
  // An escape that does not begin a private-mode SM/RM sequence: pass the ESC
  // byte through and keep scanning after it.
  | { kind: "other" };

// Classify the escape starting at `start` (which must point at an ESC byte).
// Recognizes the private-mode Set/Reset Mode grammar `\x1b [ ? [0-9;]* (h|l)`,
// which covers every sequence the filter acts on: cursor show/hide (any
// parameter list that includes `25`, e.g. `\x1b[?25h`, cvvis `\x1b[?12;25h`)
// and the real DECSET 2026 sync brackets (`\x1b[?2026h` / `\x1b[?2026l`).
function matchPrivateCsi(buffer: string, start: number): PrivateCsiMatch {
  const length = buffer.length;

  if (start + 1 >= length) {
    return { kind: "incomplete" }; // only the ESC byte so far
  }
  if (buffer[start + 1] !== "[") {
    return { kind: "other" };
  }
  if (start + 2 >= length) {
    return { kind: "incomplete" }; // `\x1b[` — could still grow into `\x1b[?…`
  }
  if (buffer[start + 2] !== "?") {
    return { kind: "other" }; // a non-private CSI (e.g. `\x1b[0m`, `\x1b[2J`)
  }

  let index = start + 3;
  while (index < length && PARAM_BYTE.test(buffer[index])) {
    index += 1;
  }

  if (index >= length) {
    // Ran off the end still inside `\x1b[?<params>`: hold it, the final byte
    // (and thus the classification) has not arrived yet.
    return { kind: "incomplete" };
  }

  const final = buffer[index];
  if (final === "h" || final === "l") {
    const params = buffer.slice(start + 3, index).split(";");
    return { kind: "csi", length: index - start + 1, final, params };
  }

  // A non-parameter, non-final byte terminates the run without producing an
  // SM/RM sequence we track (e.g. a DECRQM query `\x1b[?1000$p`). Treat the ESC
  // as opaque: pass it through and rescan after it.
  return { kind: "other" };
}

// A cursor visibility command toggles DEC private mode 25. Match by parameter
// membership so combined-param variants like cvvis (`\x1b[?12;25h`) and its
// reverse (`\x1b[?25;12h`) are recognized, not just the bare `\x1b[?25h`/`?25l`.
const togglesCursorVisibility = (params: string[]) => params.includes("25");

// The real synchronized-output brackets carry mode 2026 as their sole parameter.
const isRealSyncBracket = (params: string[]) =>
  params.length === 1 && params[0] === "2026";

// Synchronized-output reconstruction seam.
//
// ConPTY re-emits Codex's animation repaint content DECOUPLED from Codex's own
// DECSET 2026 synchronized-output brackets, so ~1/3 of animation frames arrive
// UNPROTECTED. The backend also splits a single frame across multiple
// `pty-output` events, and the rAF output scheduler can flush the
// cursor-hidden erase-half, let xterm paint it (no active sync), then apply the
// rewrite-half a frame (~16.7ms) later — a visible blank-then-repaint tear.
//
// To restore frame atomicity host-side, wrap every cursor-hidden span
// (`\x1b[?25l` … `\x1b[?25h`) in synthetic DECSET 2026 brackets: xterm 6 honors
// mode 2026 and buffers the whole span, painting it atomically even when it
// spans multiple write() calls.
//
// Mode 2026 in xterm 6 is a SINGLE boolean (no nesting) armed with a one-shot
// ~1s safety timeout from the first buffered refresh. Consequences honored here:
//   - We never inject a synthetic bracket inside a REAL 2026 span (tracked via
//     `realSyncActive`); the real span already protects that content and its
//     real `2026l` will close it. Injecting a synthetic `2026l` there would
//     close the shared boolean early and tear the tail.
//   - A long-lived hidden span (a synthetic span that stays open, e.g. a
//     full-run cursor-hidden TUI, or a cursor-show variant we somehow miss)
//     costs at most a ONE-TIME stall of up to ~1s at its start: xterm's
//     safety timeout force-clears the mode and painting resumes normally. It
//     is a bounded startup stall, never a freeze.
//
// State (`syncActive`, `realSyncActive`, `pending`) persists across chunks
// because the filter instance lives for the terminal's lifetime. `flush()`
// and `reset()` return the filter to a clean, reusable state so it can be
// safely reused across a PTY restart. Everything except the injected 8-byte
// brackets passes through byte-identical and in order.
export function createTerminalOutputFilter(): TerminalOutputFilter {
  // Whether a SYNTHETIC 2026 span (one we injected) is currently open.
  // Invariant: `syncActive` is only ever true while `realSyncActive` is false —
  // any real 2026 event takes authority over the shared boolean and clears it.
  let syncActive = false;
  // Whether a REAL 2026 span (the app's own bracket, passed through) is open.
  // While true, synthetic injection is fully suppressed.
  let realSyncActive = false;
  // Held-back trailing partial: only ever a proper prefix of a private-mode CSI
  // that was cut at a chunk boundary, so a `\x1b[?2026h` or `\x1b[?12;25h`
  // split across two `pty-output` events is still recognized once the next
  // chunk arrives. Held bytes are emitted verbatim (never dropped or reordered).
  let pending = "";

  const write = (chunk: string): TerminalOutputAction[] => {
    const buffer = pending + chunk;
    pending = "";
    let out = "";
    let index = 0;

    while (index < buffer.length) {
      const escIndex = buffer.indexOf("\x1b", index);
      if (escIndex === -1) {
        out += buffer.slice(index);
        break;
      }

      out += buffer.slice(index, escIndex);
      const match = matchPrivateCsi(buffer, escIndex);

      if (match.kind === "incomplete") {
        // A trailing partial that could still complete into a tracked private
        // CSI: hold it back byte-for-byte until the next chunk arrives.
        pending = buffer.slice(escIndex);
        break;
      }

      if (match.kind === "other") {
        out += "\x1b";
        index = escIndex + 1;
        continue;
      }

      const sequence = buffer.slice(escIndex, escIndex + match.length);

      if (isRealSyncBracket(match.params)) {
        // A REAL 2026 bracket passes through untouched and takes authority over
        // the single sync boolean. Any synthetic span we were tracking is now
        // governed by the real one, so drop our synthetic bookkeeping — we must
        // never inject a closing 2026l that would tear the real span.
        out += sequence;
        realSyncActive = match.final === "h";
        syncActive = false;
        index = escIndex + match.length;
        continue;
      }

      if (togglesCursorVisibility(match.params)) {
        if (match.final === "l") {
          // Cursor HIDE. Open a synthetic sync unless a real one already
          // protects the span, or one is already open (idempotent under nesting).
          out += sequence;
          if (!realSyncActive && !syncActive) {
            out += BEGIN_SYNC;
            syncActive = true;
          }
        } else {
          // Cursor SHOW. Close our synthetic sync (if any) immediately BEFORE
          // the show, releasing the buffered frame to paint atomically. When a
          // real span is in charge, `syncActive` is false, so nothing is
          // injected and the real `2026l` remains the sole closer.
          if (syncActive) {
            out += END_SYNC;
            syncActive = false;
          }
          out += sequence;
        }
        index = escIndex + match.length;
        continue;
      }

      // A private-mode CSI we recognize the shape of but take no action on
      // (e.g. alt-screen `\x1b[?1049h`, blink `\x1b[?12h`): pass it through
      // byte-identical.
      out += sequence;
      index = escIndex + match.length;
    }

    return out ? [{ kind: "write", data: out }] : [];
  };

  const reset = () => {
    pending = "";
    syncActive = false;
    realSyncActive = false;
  };

  const flush = (): TerminalOutputAction[] => {
    let out = "";
    // Release any held partial escape verbatim — it never completed a target.
    if (pending) {
      out += pending;
    }
    // Never leave a torn-down (or about-to-be-reused) terminal in a synthetic
    // synced state.
    if (syncActive) {
      out += END_SYNC;
    }
    // Leave a clean, reusable state so the same filter can be safely reused
    // across a PTY restart without leaking pending bytes or a stale sync flag.
    reset();
    return out ? [{ kind: "write", data: out }] : [];
  };

  return { write, flush, reset };
}

export function parseTerminalOutputActions(output: string): TerminalOutputAction[] {
  return output ? [{ kind: "write", data: output }] : [];
}

export function coalesceTerminalOutputActions(actions: TerminalOutputAction[]) {
  const coalesced: TerminalOutputAction[] = [];

  for (const action of actions) {
    const previous = coalesced.at(-1);
    if (action.kind === "write" && previous?.kind === "write") {
      previous.data += action.data;
      continue;
    }

    coalesced.push({ ...action });
  }

  return coalesced;
}
