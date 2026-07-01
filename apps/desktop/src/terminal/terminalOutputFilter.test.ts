import { describe, expect, it } from "vitest";
import {
  coalesceTerminalOutputActions,
  createTerminalOutputFilter,
  parseTerminalOutputActions,
} from "./terminalOutputFilter";

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

  it("passes split alternate-screen sequences through unchanged across output chunks", () => {
    const filter = createTerminalOutputFilter();

    expect(filter.write("before\x1b[?10")).toEqual([{ kind: "write", data: "before\x1b[?10" }]);
    expect(filter.write("49hinside\x1b[?1049lafter")).toEqual([
      { kind: "write", data: "49hinside\x1b[?1049lafter" },
    ]);
    expect(filter.flush()).toEqual([]);
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

  it("passes split synchronized output mode through unchanged across output chunks", () => {
    const filter = createTerminalOutputFilter();

    expect(filter.write("before\x1b[?202")).toEqual([
      { kind: "write", data: "before\x1b[?202" },
    ]);
    expect(filter.write("6hcodex\x1b[?20")).toEqual([
      { kind: "write", data: "6hcodex\x1b[?20" },
    ]);
    expect(filter.write("26lafter")).toEqual([{ kind: "write", data: "26lafter" }]);
    expect(filter.flush()).toEqual([]);
  });
});
