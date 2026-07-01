import { describe, expect, it } from "vitest";
import { trimTrailingPathPunctuation } from "./fileLinks";

describe("file links", () => {
  it("keeps Windows paths intact", () => {
    expect(trimTrailingPathPunctuation("C:/Users/devel/image.png")).toBe(
      "C:/Users/devel/image.png",
    );
  });

  it("removes punctuation that commonly follows paths in terminal output", () => {
    expect(trimTrailingPathPunctuation("C:/Users/devel/image.png,")).toBe(
      "C:/Users/devel/image.png",
    );
  });
});
