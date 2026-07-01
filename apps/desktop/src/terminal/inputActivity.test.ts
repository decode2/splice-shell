import { describe, expect, it } from "vitest";
import { shouldRefreshTargetAfterInput } from "./inputActivity";

describe("input activity", () => {
  it("refreshes active target after command submission", () => {
    expect(shouldRefreshTargetAfterInput("\r")).toBe(true);
  });

  it("refreshes active target after Ctrl+C", () => {
    expect(shouldRefreshTargetAfterInput("\x03")).toBe(true);
  });

  it("refreshes active target after Escape", () => {
    expect(shouldRefreshTargetAfterInput("\x1b")).toBe(true);
  });

  it("does not refresh on regular text input", () => {
    expect(shouldRefreshTargetAfterInput("abc")).toBe(false);
  });
});
