import { describe, expect, it } from "vitest";
import { shouldRecoverClosedPtyInput } from "./ptyRecovery";

describe("PTY recovery", () => {
  it("recovers the first closed-input failure for the current generation", () => {
    expect(
      shouldRecoverClosedPtyInput({
        currentGeneration: 2,
        failedGeneration: 2,
        inputClosed: false,
      }),
    ).toBe(true);
  });

  it("ignores duplicate closed-input failures once recovery started", () => {
    expect(
      shouldRecoverClosedPtyInput({
        currentGeneration: 2,
        failedGeneration: 2,
        inputClosed: true,
      }),
    ).toBe(false);
  });

  it("ignores late failures from an older PTY generation", () => {
    expect(
      shouldRecoverClosedPtyInput({
        currentGeneration: 3,
        failedGeneration: 2,
        inputClosed: false,
      }),
    ).toBe(false);
  });
});
