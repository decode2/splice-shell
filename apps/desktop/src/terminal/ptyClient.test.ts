import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ackPty,
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

  it("accepts only session-attributed output payloads carrying a byte cost", () => {
    expect(isPtyOutputPayload({ sessionId: 1, bytes: 5, data: "hello" })).toBe(true);
    // A bare string was the OLDEST payload shape; it must be rejected now.
    expect(isPtyOutputPayload("hello")).toBe(false);
    // `bytes` is the credit cost the backend charged for this payload. Without
    // it the frontend cannot ack the right amount, so a payload missing it is
    // not a valid payload.
    expect(isPtyOutputPayload({ sessionId: 1, data: "hello" })).toBe(false);
    expect(isPtyOutputPayload({ bytes: 5, data: "hello" })).toBe(false);
    expect(isPtyOutputPayload({ sessionId: 1, bytes: 5 })).toBe(false);
    expect(isPtyOutputPayload({ sessionId: "1", bytes: 5, data: "hello" })).toBe(false);
    expect(isPtyOutputPayload({ sessionId: 1, bytes: "5", data: "hello" })).toBe(false);
    expect(isPtyOutputPayload({ sessionId: 1, bytes: 5, data: 2 })).toBe(false);
    expect(isPtyOutputPayload(null)).toBe(false);
  });

  it("acks consumed bytes back to the owning session", () => {
    invokeMock.mockResolvedValue(undefined);
    void ackPty(5, 262_144);
    expect(invokeMock).toHaveBeenCalledWith("pty_ack", { sessionId: 5, bytes: 262_144 });
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
