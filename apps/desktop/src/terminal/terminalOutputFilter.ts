export type TerminalOutputAction = {
  kind: "write";
  data: string;
};

export type TerminalOutputFilter = {
  write: (chunk: string) => TerminalOutputAction[];
  flush: () => TerminalOutputAction[];
  reset: () => void;
};

// Injected timer seam so the quiet-timer is deterministic under test. The filter
// NEVER calls the global `setTimeout` directly; it drives this instead, which
// tests replace with a fake that fires the callback on demand. `set` returns an
// opaque handle that is later passed back to `clear`.
export type TerminalOutputTimer = {
  set: (callback: () => void, ms: number) => unknown;
  clear: (handle: unknown) => void;
};

export type TerminalOutputFilterOptions = {
  // Sink for timer-driven deferred emissions (the released held cursor-show).
  // When ABSENT, cursor-show holdback is DISABLED and the filter behaves exactly
  // as it did before this feature: the show passes through inline. This keeps
  // every existing caller/test untouched and guarantees held bytes can never
  // strand (there is no held state without a sink).
  onDeferredOutput?: (actions: TerminalOutputAction[]) => void;
  // Deterministic timer seam; defaults to the real setTimeout/clearTimeout.
  timer?: TerminalOutputTimer;
};

// The quiet-timer interval: how long a cursor-show is held after the last show
// before it is released to paint. Tunable in the 150–200ms band; must exceed the
// shimmer inter-frame gap so consecutive frames' hides cancel it before it fires.
// Exported so tests reference the single source of truth rather than a literal.
export const CURSOR_SHOW_HOLDBACK_MS = 175;

const DEFAULT_TIMER: TerminalOutputTimer = {
  set: (callback, ms) => setTimeout(callback, ms),
  clear: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
};

// The synthetic DECSET 2026 brackets we synthesize around each cursor-hidden
// span. Synthetic 2026 brackets are the only bytes the filter ever INSERTS.
// Everything else passes through byte-identical and in order, except cursor-show
// (mode-25 `h`) sequences under the holdback feature: those are conserved, not
// dropped — deferred behind the quiet timer and re-emitted verbatim inline on
// the next show/hide or released by the timer / flush().
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
// State (`syncActive`, `realSyncActive`, `pending`, plus `heldShow`/`timerHandle`
// for the cursor-show holdback) persists across chunks because the filter
// instance lives for the terminal's lifetime. `flush()` and `reset()` return the
// filter to a clean, reusable state so it can be safely reused across a PTY
// restart. Everything except the injected 8-byte 2026 brackets AND deferred /
// re-emitted cursor-shows passes through byte-identical and in order — and the
// shows themselves are conserved (deferred or re-emitted inline), never dropped.
export function createTerminalOutputFilter(
  options: TerminalOutputFilterOptions = {},
): TerminalOutputFilter {
  const onDeferredOutput = options.onDeferredOutput;
  const timer = options.timer ?? DEFAULT_TIMER;
  // Holdback is only active when a deferred-output sink exists; without one there
  // is nowhere to release a held show, so the show must pass through inline (the
  // pre-feature behavior) and no held state is ever created.
  const holdbackEnabled = typeof onDeferredOutput === "function";

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
  // The exact bytes of the latest cursor-show being held behind the quiet timer,
  // or null when none is held. Conserved: it is re-emitted inline on the next
  // show/hide, released by the timer, or released by flush() — never discarded
  // except by the discard-only reset().
  let heldShow: string | null = null;
  // The opaque handle of the armed quiet timer, or null when disarmed.
  let timerHandle: unknown | null = null;

  // Release the held show through the sink when the quiet interval elapses with
  // no intervening show/hide. Ordered after all prior write() output because it
  // only ever fires between chunks, once output has gone quiet.
  const fireDeferred = () => {
    const show = heldShow;
    heldShow = null;
    timerHandle = null;
    if (show !== null) {
      onDeferredOutput?.([{ kind: "write", data: show }]);
    }
  };

  const armTimer = () => {
    if (timerHandle !== null) {
      timer.clear(timerHandle);
    }
    timerHandle = timer.set(fireDeferred, CURSOR_SHOW_HOLDBACK_MS);
  };

  const cancelTimer = () => {
    if (timerHandle !== null) {
      timer.clear(timerHandle);
      timerHandle = null;
    }
  };

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
          // Cursor HIDE.
          if (holdbackEnabled && heldShow !== null) {
            // A show was held behind the quiet timer; a hide arrived first.
            // Re-emit the held show verbatim INLINE, immediately before the hide
            // and WITHIN THIS SAME output action (load-bearing: byte-conserving
            // cancel that never depends on downstream coalescing). The transient
            // show never samples visible because paints are rAF-coalesced per
            // chunk. Cancel the quiet timer and clear the held state.
            out += heldShow;
            heldShow = null;
            cancelTimer();
          }
          // Open a synthetic sync unless a real one already protects the span, or
          // one is already open (idempotent under nesting).
          out += sequence;
          if (!realSyncActive && !syncActive) {
            out += BEGIN_SYNC;
            syncActive = true;
          }
        } else if (holdbackEnabled) {
          // Cursor SHOW with holdback active. Close our synthetic sync (if any)
          // immediately so the buffered frame still paints atomically — but do
          // NOT emit the show inline. Instead hold it behind the quiet timer.
          if (syncActive) {
            out += END_SYNC;
            syncActive = false;
          }
          if (heldShow !== null) {
            // A show is already held with no intervening hide: re-emit the old
            // one inline (latest-wins, byte-conserving) before superseding it.
            out += heldShow;
          }
          // Hold regardless of syncActive/realSyncActive — the shimmer walk
          // exists inside real 2026 spans too, and real brackets continue to
          // govern the sync flags untouched. (Re)arm the quiet timer.
          heldShow = sequence;
          armTimer();
        } else {
          // Cursor SHOW without a sink: pre-feature behavior. Close the synthetic
          // sync immediately BEFORE the show and pass the show through inline.
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
    // Discard-only API: it drops `pending` and a held show WITHOUT emitting them,
    // and cancels the quiet timer so no deferred emission can fire later. This is
    // safe for production because teardown/restart use flush() (which releases a
    // held show) — reset cannot strand a hidden cursor on those paths.
    cancelTimer();
    pending = "";
    heldShow = null;
    syncActive = false;
    realSyncActive = false;
  };

  const flush = (): TerminalOutputAction[] => {
    let out = "";
    // Release any held cursor-show verbatim first. In stream order the held show
    // was toggled before any trailing partial of the final chunk, so it leads.
    // (A held show implies `syncActive` is false — a show always closes an open
    // synthetic sync — so it never straddles the END_SYNC below.)
    if (heldShow !== null) {
      out += heldShow;
    }
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
    // across a PTY restart without leaking pending bytes, a held show, an armed
    // timer, or a stale sync flag.
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
