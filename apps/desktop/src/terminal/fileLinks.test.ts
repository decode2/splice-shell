import { describe, expect, it } from "vitest";
import { extractLocalFileLinks, trimTrailingPathPunctuation } from "./fileLinks";

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

  it("extracts bare Windows paths without surrounding markdown punctuation", () => {
    expect(extractLocalFileLinks("Created [C:/Users/devel/image.png].")).toEqual([
      {
        endIndex: 33,
        startIndex: 9,
        text: "C:/Users/devel/image.png",
      },
    ]);
  });

  it("extracts quoted Windows paths with spaces", () => {
    expect(extractLocalFileLinks('Created "C:/Users/devel/My Images/logo.png"')).toEqual([
      {
        endIndex: 42,
        startIndex: 9,
        text: "C:/Users/devel/My Images/logo.png",
      },
    ]);
  });

  it("normalizes file URLs to local Windows paths", () => {
    expect(extractLocalFileLinks("Open file:///C:/Users/devel/logo%201.png")).toEqual([
      {
        endIndex: 40,
        startIndex: 5,
        text: "C:/Users/devel/logo 1.png",
      },
    ]);
  });
});
