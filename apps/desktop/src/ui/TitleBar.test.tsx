// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { PastePreviewState } from "../paste/pastePreview";
import { useWindowFocused, type WindowChrome } from "../window/windowChrome";
import { TitleBar } from "./TitleBar";
import { DEFAULT_TAB_ADAPTER_STATE, type TabState } from "./useSessions";

const idlePasteState: PastePreviewState = {
  kind: "idle",
  message: "Paste preview idle",
};

function tab(overrides: Partial<TabState> = {}): TabState {
  return {
    tabId: "tab-0",
    title: "shell",
    adapterState: DEFAULT_TAB_ADAPTER_STATE,
    health: "healthy",
    ...overrides,
  };
}

function mockChrome(): WindowChrome {
  return {
    minimize: vi.fn(async () => {}),
    toggleMaximize: vi.fn(async () => {}),
    close: vi.fn(async () => {}),
    isMaximized: vi.fn(async () => false),
    onResized: vi.fn(async () => () => {}),
    onFocusChanged: vi.fn(async () => () => {}),
  };
}

function renderTitleBar(overrides: Partial<Parameters<typeof TitleBar>[0]> = {}) {
  const chrome = overrides.chrome ?? mockChrome();
  const onToggleSettings = overrides.onToggleSettings ?? vi.fn();
  const result = render(
    <TitleBar
      tabs={[tab()]}
      activeId="tab-0"
      onSelectTab={vi.fn()}
      onCloseTab={vi.fn()}
      onCreateTab={vi.fn()}
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

  it("renders settings as an icon button (inline SVG), not a text pill", () => {
    const { getByRole } = renderTitleBar();

    const settings = getByRole("button", { name: "Settings" });
    // The redesigned settings control uses an inline gear glyph, matching the
    // window controls' visual language rather than a "Settings" text pill.
    expect(settings.querySelector("svg")).toBeTruthy();
    expect(settings.textContent?.trim()).toBe("");
  });
});

describe("TitleBar tab strip", () => {
  it("hosts the per-tab strip (tablist) instead of a single global adapter chip + health dot", () => {
    const { getByRole, container } = renderTitleBar({
      tabs: [
        tab({
          tabId: "tab-0",
          adapterState: { kind: "ready", processName: "codex.exe", adapterName: "codex" },
        }),
      ],
      activeId: "tab-0",
    });

    // The strip is present…
    expect(getByRole("tablist", { name: "Terminal tabs" })).toBeTruthy();
    // …and the OLD global title-bar chip / health dot are gone (they moved into
    // each tab).
    expect(container.querySelector(".titlebar-chip")).toBeNull();
    expect(container.querySelector(".titlebar-health-dot")).toBeNull();
  });

  it("forwards strip interactions to its tab callbacks", () => {
    const onSelectTab = vi.fn();
    const onCloseTab = vi.fn();
    const onCreateTab = vi.fn();
    const { getByRole } = renderTitleBar({
      tabs: [tab({ tabId: "tab-0" })],
      activeId: "tab-0",
      onSelectTab,
      onCloseTab,
      onCreateTab,
    });

    fireEvent.click(getByRole("button", { name: "New tab" }));
    expect(onCreateTab).toHaveBeenCalledTimes(1);

    fireEvent.click(getByRole("button", { name: "Close tab" }));
    expect(onCloseTab).toHaveBeenCalledWith("tab-0");
  });
});

describe("TitleBar paste feedback", () => {
  it("renders nothing for the paste feedback while idle", () => {
    const { container } = renderTitleBar({ pasteState: idlePasteState });

    expect(container.querySelector(".paste-preview")).toBeNull();
  });

  it("renders the transient paste feedback for a non-idle state", () => {
    const { container } = renderTitleBar({
      pasteState: {
        kind: "error",
        message: "Image paste failed",
      },
    });

    expect(container.querySelector(".paste-preview")).toBeTruthy();
  });
});

describe("TitleBar drag regions", () => {
  it("marks the header, brand, and empty strip area as drag regions but never the buttons", () => {
    const { container, getByRole } = renderTitleBar();

    const header = container.querySelector(".titlebar");
    expect(header?.hasAttribute("data-tauri-drag-region")).toBe(true);
    expect(
      container.querySelector(".titlebar-brand")?.hasAttribute("data-tauri-drag-region"),
    ).toBe(true);
    // The elastic center wrapper and the strip's empty area drag the window.
    expect(
      container.querySelector(".titlebar-center")?.hasAttribute("data-tauri-drag-region"),
    ).toBe(true);
    expect(
      container.querySelector(".tabstrip")?.hasAttribute("data-tauri-drag-region"),
    ).toBe(true);

    // Buttons (window controls, the settings icon, and the tab strip's own
    // controls) opt out of dragging so a click acts instead of dragging.
    for (const name of ["Settings", "Minimize", "Maximize", "Close", "New tab", "Close tab"]) {
      expect(getByRole("button", { name }).hasAttribute("data-tauri-drag-region")).toBe(false);
    }
  });
});

// The App wires window focus onto the shell as `data-focused` so the CSS can
// dim the title-bar chrome on blur. This harness mirrors that exact wiring
// (`data-focused={focused || undefined}`) without pulling in App's terminal /
// paste pipeline, so we test the focus→attribute contract in isolation.
function FocusShellHarness({ chrome }: { chrome: WindowChrome }) {
  const focused = useWindowFocused(chrome);
  return (
    <main className="app-shell" data-focused={focused || undefined}>
      <TitleBar
        tabs={[tab()]}
        activeId="tab-0"
        onSelectTab={vi.fn()}
        onCloseTab={vi.fn()}
        onCreateTab={vi.fn()}
        pasteState={idlePasteState}
        settingsOpen={false}
        onToggleSettings={vi.fn()}
        isMaximized={false}
        chrome={chrome}
      />
    </main>
  );
}

describe("App shell focus dimming", () => {
  it("sets data-focused on the shell while focused and removes it on blur", async () => {
    let focusHandler: ((focused: boolean) => void) | undefined;
    const chrome = mockChrome();
    chrome.onFocusChanged = vi.fn(async (handler: (focused: boolean) => void) => {
      focusHandler = handler;
      return () => {};
    });

    const { container } = render(<FocusShellHarness chrome={chrome} />);
    const shell = container.querySelector(".app-shell");

    // Focused on mount → attribute present.
    expect(shell?.hasAttribute("data-focused")).toBe(true);

    await waitFor(() => expect(focusHandler).toBeDefined());

    // Blur → attribute removed so `.app-shell:not([data-focused])` matches.
    act(() => {
      focusHandler?.(false);
    });
    await waitFor(() => expect(shell?.hasAttribute("data-focused")).toBe(false));

    // Focus restored → attribute returns.
    act(() => {
      focusHandler?.(true);
    });
    await waitFor(() => expect(shell?.hasAttribute("data-focused")).toBe(true));
  });
});
