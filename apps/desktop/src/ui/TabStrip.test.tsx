// @vitest-environment jsdom
import { cleanup, fireEvent, render } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { TabStrip } from "./TabStrip";
import { DEFAULT_TAB_ADAPTER_STATE, type TabState } from "./useSessions";

function makeTab(overrides: Partial<TabState> = {}): TabState {
  return {
    tabId: "tab-0",
    title: "shell",
    adapterState: DEFAULT_TAB_ADAPTER_STATE,
    health: "healthy",
    ...overrides,
  };
}

function renderStrip(overrides: Partial<Parameters<typeof TabStrip>[0]> = {}) {
  const props = {
    tabs: [makeTab()],
    activeId: "tab-0",
    onSelect: vi.fn(),
    onClose: vi.fn(),
    onCreate: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<TabStrip {...props} />) };
}

afterEach(cleanup);

describe("TabStrip rendering", () => {
  it("renders one tab per session with its adapter label, health dot, and active highlight", () => {
    const tabs: TabState[] = [
      makeTab({
        tabId: "tab-0",
        adapterState: { kind: "ready", processName: "codex.exe", adapterName: "codex" },
        health: "healthy",
      }),
      makeTab({ tabId: "tab-1", health: "reconnecting" }),
    ];
    const { getByText, getAllByRole, container } = renderStrip({ tabs, activeId: "tab-0" });

    // One ARIA tab per session, plus adapter label rendered per tab.
    const renderedTabs = getAllByRole("tab");
    expect(renderedTabs).toHaveLength(2);
    expect(getByText("codex")).toBeTruthy();

    // Active tab is aria-selected; the other is not.
    expect(renderedTabs[0].getAttribute("aria-selected")).toBe("true");
    expect(renderedTabs[1].getAttribute("aria-selected")).toBe("false");

    // Per-tab health dots are independent.
    const dots = container.querySelectorAll(".tabstrip-dot");
    expect(dots[0].getAttribute("data-health")).toBe("healthy");
    expect(dots[1].getAttribute("data-health")).toBe("reconnecting");
  });

  it("labels an unsupported process in amber semantics with the .exe stripped", () => {
    const { getByText, container } = renderStrip({
      tabs: [makeTab({ adapterState: { kind: "unsupported", processName: "notepad.exe" } })],
    });

    expect(getByText("notepad")).toBeTruthy();
    expect(container.textContent).not.toContain(".exe");
    expect(container.querySelector(".tabstrip-title--unsupported")).toBeTruthy();
  });

  it("applies roving tabindex: only the active tab is tabbable", () => {
    const tabs = [makeTab({ tabId: "tab-0" }), makeTab({ tabId: "tab-1" })];
    const { getAllByRole } = renderStrip({ tabs, activeId: "tab-1" });

    const renderedTabs = getAllByRole("tab");
    expect(renderedTabs[0].getAttribute("tabindex")).toBe("-1");
    expect(renderedTabs[1].getAttribute("tabindex")).toBe("0");
  });
});

describe("TabStrip controls", () => {
  it("selects a tab on click", () => {
    const tabs = [makeTab({ tabId: "tab-0" }), makeTab({ tabId: "tab-1" })];
    const { props, getAllByRole } = renderStrip({ tabs, activeId: "tab-0" });

    fireEvent.click(getAllByRole("tab")[1]);
    expect(props.onSelect).toHaveBeenCalledWith("tab-1");
  });

  it("closes only that tab on the close button and does NOT also select it", () => {
    const tabs = [makeTab({ tabId: "tab-0" }), makeTab({ tabId: "tab-1" })];
    const { props, getAllByRole } = renderStrip({ tabs, activeId: "tab-0" });

    fireEvent.click(getAllByRole("button", { name: "Close tab" })[1]);

    expect(props.onClose).toHaveBeenCalledWith("tab-1");
    expect(props.onClose).toHaveBeenCalledTimes(1);
    // The close click must be swallowed so it never triggers a tab select.
    expect(props.onSelect).not.toHaveBeenCalled();
  });

  it("creates a new tab on the + button", () => {
    const { props, getByRole } = renderStrip();

    fireEvent.click(getByRole("button", { name: "New tab" }));
    expect(props.onCreate).toHaveBeenCalledTimes(1);
  });

  it("activates the focused tab on Enter and Space", () => {
    const tabs = [makeTab({ tabId: "tab-0" }), makeTab({ tabId: "tab-1" })];
    const { props, getAllByRole } = renderStrip({ tabs, activeId: "tab-0" });

    fireEvent.keyDown(getAllByRole("tab")[1], { key: "Enter" });
    expect(props.onSelect).toHaveBeenCalledWith("tab-1");

    fireEvent.keyDown(getAllByRole("tab")[1], { key: " " });
    expect(props.onSelect).toHaveBeenCalledTimes(2);
  });
});

describe("TabStrip drag-region discipline", () => {
  it("marks only the empty strip area as draggable, never tabs or buttons", () => {
    const { container, getAllByRole, getByRole } = renderStrip({
      tabs: [makeTab({ tabId: "tab-0" })],
      activeId: "tab-0",
    });

    // The strip container (its empty area) drags the window.
    const strip = container.querySelector(".tabstrip");
    expect(strip?.hasAttribute("data-tauri-drag-region")).toBe(true);

    // Tabs and buttons must click, not drag.
    expect(getAllByRole("tab")[0].hasAttribute("data-tauri-drag-region")).toBe(false);
    expect(getByRole("button", { name: "Close tab" }).hasAttribute("data-tauri-drag-region")).toBe(
      false,
    );
    expect(getByRole("button", { name: "New tab" }).hasAttribute("data-tauri-drag-region")).toBe(
      false,
    );
  });
});
