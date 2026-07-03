import { describe, expect, it } from "vitest";
import {
  coalesceTerminalOutputActions,
  createTerminalOutputFilter,
  parseTerminalOutputActions,
  type TerminalOutputAction,
} from "./terminalOutputFilter";

const dataOf = (actions: TerminalOutputAction[]) => actions.map((action) => action.data).join("");

describe("terminal output filter", () => {
  it("passes alternate-screen enter through so TUIs render correctly", () => {
    expect(parseTerminalOutputActions("before\x1b[?1049hinside")).toEqual([
      { kind: "write", data: "before\x1b[?1049hinside" },
    ]);
  });

  it("passes alternate-screen exit through without injecting history snapshots", () => {
    expect(parseTerminalOutputActions("inside\x1b[?1049lafter")).toEqual([
      { kind: "write", data: "inside\x1b[?1049lafter" },
    ]);
  });

  it("preserves destructive clear sequences for active TUI correctness", () => {
    expect(parseTerminalOutputActions("\x1b[?1049h\x1b[2J\x1b[Hcodex")).toEqual([
      { kind: "write", data: "\x1b[?1049h\x1b[2J\x1b[Hcodex" },
    ]);
  });

  it("passes split alternate-screen sequences through byte-identical across output chunks", () => {
    const filter = createTerminalOutputFilter();

    // A private-mode CSI cut mid-parameter (`\x1b[?10`) is now held until it can
    // be classified, because its parameters could still resolve into a tracked
    // sequence (e.g. `\x1b[?10;25h`). Whatever the per-chunk split, the
    // concatenated output stays byte-identical to the input.
    const first = dataOf(filter.write("before\x1b[?10"));
    const second = dataOf(filter.write("49hinside\x1b[?1049lafter"));
    const tail = dataOf(filter.flush());
    expect(first + second + tail).toBe("before\x1b[?1049hinside\x1b[?1049lafter");
  });

  it("coalesces adjacent writes without crossing capture boundaries", () => {
    expect(
      coalesceTerminalOutputActions([
        { kind: "write", data: "a" },
        { kind: "write", data: "b" },
        { kind: "write", data: "c" },
        { kind: "write", data: "d" },
      ]),
    ).toEqual([{ kind: "write", data: "abcd" }]);
  });

  it("preserves synchronized output mode so xterm can batch modern TUI frames", () => {
    expect(parseTerminalOutputActions("\x1b[?2026hcodex\x1b[?2026l")).toEqual([
      { kind: "write", data: "\x1b[?2026hcodex\x1b[?2026l" },
    ]);
  });

  it("passes split synchronized output mode through byte-identical across output chunks", () => {
    const filter = createTerminalOutputFilter();

    // A real `\x1b[?2026h` cut across chunk boundaries is held so the filter can
    // recognize the real sync span and suppress synthetic injection inside it.
    // The concatenated output remains byte-identical to the input.
    const parts = [
      dataOf(filter.write("before\x1b[?202")),
      dataOf(filter.write("6hcodex\x1b[?20")),
      dataOf(filter.write("26lafter")),
      dataOf(filter.flush()),
    ];
    expect(parts.join("")).toBe("before\x1b[?2026hcodex\x1b[?2026lafter");
  });

  // ConPTY re-emits Codex's animation repaint decoupled from Codex's own
  // DECSET 2026 brackets, so ~1/3 of frames arrive unprotected and tear when
  // the rAF scheduler paints the erase-half before the rewrite-half. The
  // filter reconstructs frame atomicity by wrapping every cursor-hidden span
  // in synthetic 2026 sync brackets that xterm honors across write() calls.
  it("brackets a cursor-hidden span with synthetic 2026 sync across chunk boundaries", () => {
    const filter = createTerminalOutputFilter();

    // The 2026h is injected immediately AFTER the cursor-hide.
    expect(dataOf(filter.write("row34\x1b[?25l\x1b[K"))).toBe("row34\x1b[?25l\x1b[?2026h\x1b[K");
    // Sync state persists into a later chunk: the 2026l is injected immediately
    // BEFORE the cursor-show that arrives in a separate pty-output event.
    expect(dataOf(filter.write("rewrite\x1b[?25h"))).toBe("rewrite\x1b[?2026l\x1b[?25h");
    expect(filter.flush()).toEqual([]);
  });

  it("injects exactly one synthetic sync pair for nested cursor-hide spans", () => {
    const filter = createTerminalOutputFilter();

    const out = dataOf(filter.write("\x1b[?25la\x1b[?25lb\x1b[?25hc"));
    expect(out).toBe("\x1b[?25l\x1b[?2026ha\x1b[?25lb\x1b[?2026l\x1b[?25hc");
    expect(out.split("\x1b[?2026h").length - 1).toBe(1);
    expect(out.split("\x1b[?2026l").length - 1).toBe(1);
  });

  it("detects a cursor-hide sequence split across two chunks and brackets it", () => {
    const filter = createTerminalOutputFilter();

    // A `\x1b[?25l` cut mid-sequence must be held back, not passed through torn.
    expect(filter.write("text\x1b[?2")).toEqual([{ kind: "write", data: "text" }]);
    expect(dataOf(filter.write("5l\x1b[K"))).toBe("\x1b[?25l\x1b[?2026h\x1b[K");
    expect(filter.flush()).toEqual([{ kind: "write", data: "\x1b[?2026l" }]);
  });

  it("passes content without cursor hide/show through byte-identical", () => {
    const filter = createTerminalOutputFilter();

    const input = "\x1b[1mhello\x1b[0m world \x1b[?2026hframe\x1b[?2026l\x1b[?1049h\x1b[2J";
    expect(dataOf(filter.write(input))).toBe(input);
    expect(filter.flush()).toEqual([]);
  });

  it("closes an open synthetic sync on flush so teardown never leaves 2026 active", () => {
    const filter = createTerminalOutputFilter();

    filter.write("\x1b[?25l\x1b[K"); // opens sync, no matching cursor-show yet
    expect(filter.flush()).toEqual([{ kind: "write", data: "\x1b[?2026l" }]);
    // Idempotent: once closed, a second flush emits nothing.
    expect(filter.flush()).toEqual([]);
  });

  // FIX 1 — flush must leave the filter in a CLEAN reusable state so a filter
  // reused across a PTY restart cannot corrupt the next session's output.
  it("leaves a clean reusable state after flush so a reused filter is not corrupted", () => {
    const filter = createTerminalOutputFilter();

    // Session dies mid-escape: a partial `\x1b[?2` is held back.
    expect(dataOf(filter.write("row\x1b[?2"))).toBe("row");
    // Restart flushes: the held partial is released and state is reset.
    expect(dataOf(filter.flush())).toBe("\x1b[?2");

    // The next session's first output must NOT be prefixed by the stale partial
    // and must not be swallowed by a stale synthetic-sync state.
    expect(dataOf(filter.write("\x1b[?2PS C:\\>"))).toBe("\x1b[?2PS C:\\>");
    expect(filter.flush()).toEqual([]);
  });

  it("re-brackets the next hidden span after flush reset closes a stale synthetic sync", () => {
    const filter = createTerminalOutputFilter();

    // Open a synthetic span, then flush (emits closing 2026l and resets).
    expect(dataOf(filter.write("\x1b[?25la"))).toBe("\x1b[?25l\x1b[?2026ha");
    expect(dataOf(filter.flush())).toBe("\x1b[?2026l");

    // A fresh hidden span on the reused filter must open a NEW synthetic span,
    // proving syncActive was reset (a leaked syncActive=true would suppress it).
    expect(dataOf(filter.write("\x1b[?25lb"))).toBe("\x1b[?25l\x1b[?2026hb");
  });

  it("resets all state via reset() so a reused filter starts clean", () => {
    const filter = createTerminalOutputFilter();

    filter.write("row\x1b[?25l\x1b[?2"); // opens synthetic sync, holds a partial
    filter.reset();

    // Nothing held, no synthetic close emitted, and the next span brackets fresh.
    expect(filter.flush()).toEqual([]);
    expect(dataOf(filter.write("\x1b[?25lx"))).toBe("\x1b[?25l\x1b[?2026hx");
  });

  // FIX 2 — a REAL DECSET 2026 span already protects its content, so the filter
  // must NOT inject a synthetic 2026l inside it (that would close the real span
  // early and tear the tail).
  it("suppresses synthetic injection inside a real 2026 sync span", () => {
    const filter = createTerminalOutputFilter();

    const input = "\x1b[?2026h A \x1b[?25l B \x1b[?25h TAIL \x1b[?2026l";
    const out = dataOf(filter.write(input));

    // No synthetic brackets were inserted: output is byte-identical to input.
    expect(out).toBe(input);
    // Exactly the one real begin and one real end bracket survive.
    expect(out.split("\x1b[?2026h").length - 1).toBe(1);
    expect(out.split("\x1b[?2026l").length - 1).toBe(1);
    expect(filter.flush()).toEqual([]);
  });

  it("still brackets a normal hidden span that has no surrounding real sync", () => {
    const filter = createTerminalOutputFilter();

    const out = dataOf(filter.write("\x1b[?25l frame \x1b[?25h"));
    expect(out).toBe("\x1b[?25l\x1b[?2026h frame \x1b[?2026l\x1b[?25h");
    expect(filter.flush()).toEqual([]);
  });

  it("does not close a real sync span opened after a synthetic span", () => {
    const filter = createTerminalOutputFilter();

    // Synthetic span opens on hide; then a real 2026h takes authority over the
    // single sync boolean. The later cursor-show must NOT inject a 2026l, or it
    // would close the real span early.
    const input = "\x1b[?25l A \x1b[?2026h B \x1b[?25h C \x1b[?2026l";
    const out = dataOf(filter.write(input));

    // Only the synthetic begin (after the hide) is inserted; no synthetic end.
    expect(out).toBe("\x1b[?25l\x1b[?2026h A \x1b[?2026h B \x1b[?25h C \x1b[?2026l");
    expect(filter.flush()).toEqual([]);
  });

  // FIX 3 — recognize cursor SHOW/HIDE by parameter membership so combined-param
  // variants like cvvis (`\x1b[?12;25h`) close the synthetic span.
  it("closes the synthetic span on a cvvis combined-param cursor show (\\x1b[?12;25h)", () => {
    const filter = createTerminalOutputFilter();

    const out = dataOf(filter.write("\x1b[?25l frame \x1b[?12;25h"));
    expect(out).toBe("\x1b[?25l\x1b[?2026h frame \x1b[?2026l\x1b[?12;25h");
    expect(filter.flush()).toEqual([]);
  });

  it("closes the synthetic span on a reversed combined-param cursor show (\\x1b[?25;12h)", () => {
    const filter = createTerminalOutputFilter();

    const out = dataOf(filter.write("\x1b[?25l frame \x1b[?25;12h"));
    expect(out).toBe("\x1b[?25l\x1b[?2026h frame \x1b[?2026l\x1b[?25;12h");
    expect(filter.flush()).toEqual([]);
  });

  it("holds a combined-param cursor-show split across chunks until it can classify", () => {
    const filter = createTerminalOutputFilter();

    expect(dataOf(filter.write("\x1b[?25l frame \x1b[?12;2"))).toBe(
      "\x1b[?25l\x1b[?2026h frame ",
    );
    // The trailing `\x1b[?12;2` is held; the next chunk completes cvvis and the
    // synthetic close is injected immediately before it.
    expect(dataOf(filter.write("5h"))).toBe("\x1b[?2026l\x1b[?12;25h");
    expect(filter.flush()).toEqual([]);
  });

  it("does not treat an unrelated private CSI (\\x1b[?12h) as a cursor show", () => {
    const filter = createTerminalOutputFilter();

    // Param 12 alone (blink) is not cursor visibility: the synthetic span stays
    // open and only closes on the real cursor show.
    const out = dataOf(filter.write("\x1b[?25l frame \x1b[?12h more \x1b[?25h"));
    expect(out).toBe("\x1b[?25l\x1b[?2026h frame \x1b[?12h more \x1b[?2026l\x1b[?25h");
    expect(filter.flush()).toEqual([]);
  });
});
