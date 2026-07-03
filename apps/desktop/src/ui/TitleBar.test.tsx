// @vitest-environment jsdom
import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ActivePasteTargetState } from "../paste/activePasteTarget";
import type { PastePreviewState } from "../paste/pastePreview";
import type { WindowChrome } from "../window/windowChrome";
import { TitleBar } from "./TitleBar";

const activePasteTarget: ActivePasteTargetState = {
  kind: "ready",
  processName: "codex.exe",
  adapterName: "codex-cli",
};
const idlePasteState: PastePreviewState = {
  kind: "idle",
  message: "Paste preview idle",
};

function mockChrome(): WindowChrome {
  return {
    minimize: vi.fn(async () => {}),
    toggleMaximize: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
    isMaximized: vi.fn(async () => false),
    onResized: vi.fn(async () => () => {}),
  };
}

function renderTitleBar(
  overrides: Partial<Parameters<typeof TitleBar>[0]> = {},
) {
  const chrome = overrides.chrome ?? mockChrome();
  const onToggleSettings = overrides.onToggleSettings ?? vi.fn();
  const result = render(
    <TitleBar
      connection={{ input: false, output: false }}
      activePasteTargetState={activePasteTarget}
      pasteState={idlePasteState}
      settingsOpen={false}
      onToggleSettings={onToggleSettings}
      isMaximized={false}
      chrome={chrome}
      {...overrides}
    />,
  );
  return { chrome, onToggleSettings, ...result };
}

afterEach(cleanup);

describe("TitleBar window controls", () => {
  it("invokes the matching WindowChrome method for each control click", () => {
    const { chrome, getByRole } = renderTitleBar();

    fireEvent.click(getByRole("button", { name: "Minimize" }));
    fireEvent.click(getByRole("button", { name: "Maximize" }));
    fireEvent.click(getByRole("button", { name: "Close" }));

    expect(chrome.minimize).toHaveBeenCalledTimes(1);
    expect(chrome.toggleMaximize).toHaveBeenCalledTimes(1);
    expect(chrome.close).toHaveBeenCalledTimes(1);
  });

  it("shows the maximize glyph and label while the window is floating", () => {
    const { getByRole, queryByRole, container } = renderTitleBar({ isMaximized: false });

    expect(getByRole("button", { name: "Maximize" })).toBeTruthy();
    expect(queryByRole("button", { name: "Restore" })).toBeNull();
    expect(container.querySelector(".window-glyph-maximize")).toBeTruthy();
    expect(container.querySelector(".window-glyph-restore")).toBeNull();
  });

  it("swaps the maximize control to a restore glyph and label when maximized", () => {
    const { getByRole, queryByRole, container } = renderTitleBar({ isMaximized: true });

    expect(getByRole("button", { name: "Restore" })).toBeTruthy();
    expect(queryByRole("button", { name: "Maximize" })).toBeNull();
    expect(container.querySelector(".window-glyph-restore")).toBeTruthy();
    expect(container.querySelector(".window-glyph-maximize")).toBeNull();
  });
});

describe("TitleBar settings toggle", () => {
  it("invokes onToggleSettings on click and reflects settingsOpen via aria-expanded", () => {
    const onToggleSettings = vi.fn();
    const { getByRole } = renderTitleBar({ settingsOpen: true, onToggleSettings });

    const settings = getByRole("button", { name: "Settings" });
    expect(settings.getAttribute("aria-expanded")).toBe("true");

    fireEvent.click(settings);
    expect(onToggleSettings).toHaveBeenCalledTimes(1);
  });
});

describe("TitleBar drag regions", () => {
  it("marks the header and its inert children as drag regions but never the buttons", () => {
    const { container, getByRole, getByText } = renderTitleBar();

    const header = container.querySelector(".titlebar");
    expect(header?.hasAttribute("data-tauri-drag-region")).toBe(true);
    expect(
      container.querySelector(".titlebar-brand")?.hasAttribute("data-tauri-drag-region"),
    ).toBe(true);
    expect(
      container.querySelector(".titlebar-status")?.hasAttribute("data-tauri-drag-region"),
    ).toBe(true);
    // The ConPTY status text is an exact drag target: Tauri matches the event
    // target itself, not ancestors, so the text span must carry the attribute.
    expect(getByText(/ConPTY/).hasAttribute("data-tauri-drag-region")).toBe(true);

    // Buttons opt out of dragging by NOT carrying the attribute.
    for (const name of ["Settings", "Minimize", "Maximize", "Close"]) {
      expect(getByRole("button", { name }).hasAttribute("data-tauri-drag-region")).toBe(false);
    }
  });
});

describe("TitleBar status cluster", () => {
  it("renders the ConPTY connection status from the connection prop", () => {
    const { getByText } = renderTitleBar({ connection: { input: true, output: false } });

    expect(getByText(/input yes · output waiting/)).toBeTruthy();
  });
});
