import { describe, expect, it } from "vitest";
import { isPtyOutputPayload, PTY_OUTPUT_EVENT } from "./ptyClient";

describe("ptyClient", () => {
  it("uses a stable output event name", () => {
    expect(PTY_OUTPUT_EVENT).toBe("pty-output");
  });

  it("accepts only string output payloads", () => {
    expect(isPtyOutputPayload("hello")).toBe(true);
    expect(isPtyOutputPayload(new Uint8Array())).toBe(false);
    expect(isPtyOutputPayload(null)).toBe(false);
  });
});
