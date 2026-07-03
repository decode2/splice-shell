import { afterEach, describe, expect, it, vi } from "vitest";
import {
  interruptPty,
  isPtyOutputPayload,
  killPty,
  PTY_OUTPUT_EVENT,
  resizePty,
  writePty,
} from "./ptyClient";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

afterEach(() => {
  invokeMock.mockReset();
});

describe("ptyClient", () => {
  it("uses a stable output event name", () => {
    expect(PTY_OUTPUT_EVENT).toBe("pty-output");
  });

  it("accepts only session-attributed object output payloads", () => {
    expect(isPtyOutputPayload({ sessionId: 1, data: "hello" })).toBe(true);
    // A bare string was the OLD payload shape; it must be rejected now.
    expect(isPtyOutputPayload("hello")).toBe(false);
    expect(isPtyOutputPayload({ data: "hello" })).toBe(false);
    expect(isPtyOutputPayload({ sessionId: 1 })).toBe(false);
    expect(isPtyOutputPayload({ sessionId: "1", data: "hello" })).toBe(false);
    expect(isPtyOutputPayload({ sessionId: 1, data: 2 })).toBe(false);
    expect(isPtyOutputPayload(null)).toBe(false);
  });

  it("forwards the session id when writing to the PTY", () => {
    invokeMock.mockResolvedValue(undefined);
    void writePty("ls\r", 5);
    expect(invokeMock).toHaveBeenCalledWith("pty_write", { data: "ls\r", sessionId: 5 });
  });

  it("forwards the session id when interrupting the PTY", () => {
    invokeMock.mockResolvedValue(undefined);
    void interruptPty(5);
    expect(invokeMock).toHaveBeenCalledWith("pty_interrupt", { sessionId: 5 });
  });

  it("forwards the session id when resizing the PTY", () => {
    invokeMock.mockResolvedValue(undefined);
    void resizePty({ cols: 80, rows: 24 }, 5);
    expect(invokeMock).toHaveBeenCalledWith("pty_resize", { cols: 80, rows: 24, sessionId: 5 });
  });

  it("forwards the session id when killing the PTY", () => {
    invokeMock.mockResolvedValue(undefined);
    void killPty(5);
    expect(invokeMock).toHaveBeenCalledWith("pty_kill", { sessionId: 5 });
  });
});
