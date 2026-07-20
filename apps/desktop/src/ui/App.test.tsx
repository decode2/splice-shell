// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type InvokeMock = (command: string, args?: unknown) => Promise<unknown>;

const mocks = vi.hoisted(() => ({
  invoke: vi.fn<InvokeMock>(),
}));

// Only the Tauri IPC boundary is mocked; getActivePasteTarget + writePty run
// their real wiring on top of it, so paste routing is exercised for real.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: mocks.invoke,
}));

// Window chrome touches getCurrentWindow(), which throws outside Tauri. Stub the
// whole module so App renders in jsdom with a stable focused/floating window.
vi.mock("../window/windowChrome", () => ({
  getWindowChrome: () => ({
    minimize: async () => {},
    toggleMaximize: async () => {},
    close: async () => {},
    isMaximized: async () => false,
    onResized: async () => () => {},
    onFocusChanged: async () => () => {},
  }),
  useWindowMaximized: () => false,
  useWindowFocused: () => true,
}));

// Capture every TerminalView render's props so tests can drive per-tab callbacks
// (onPtyReady / onTextPaste) and assert the settings/active/visible props App
// threads down. The real xterm-backed TerminalView is replaced by a light stub.
const terminalViewRenders: Array<Record<string, unknown>> = [];

// The settings panel lives in its own light module; stub it with controllable
// buttons so tests can drive onChange (font bump) and onClose deterministically.
vi.mock("./TerminalSettingsPanel", () => ({
  TerminalSettingsPanel: ({
    onChange,
    onClose,
  }: {
    onChange: (next: { background: string; foreground: string; fontSize: number }) => void;
    onClose: () => void;
  }) => (
    <div data-testid="settings-panel" className="terminal-settings-panel">
      <button
        type="button"
        data-testid="bump-font"
        onClick={() => onChange({ background: "#000", foreground: "#fff", fontSize: 20 })}
      >
        bump
      </button>
      <button type="button" data-testid="close-settings" onClick={onClose}>
        close
      </button>
    </div>
  ),
}));

vi.mock("./TerminalView", () => ({
  TerminalView: (props: Record<string, unknown>) => {
    terminalViewRenders.push(props);
    const settings = props.settings as { fontSize: number } | undefined;
    return (
      <div
        data-testid="terminal-view"
        data-active={props.active ? "true" : "false"}
        data-visible={props.visible ? "true" : "false"}
        data-fontsize={String(settings?.fontSize)}
        style={{ visibility: props.visible ? "visible" : "hidden" }}
      />
    );
  },
}));

// Imported AFTER the mocks are registered.
const { App } = await import("./App");

function activeViewProps() {
  return [...terminalViewRenders].reverse().find((props) => props.active === true);
}

function pressChord(key: string, options: { shiftKey?: boolean } = {}) {
  act(() => {
    window.dispatchEvent(
      new KeyboardEvent("keydown", { key, ctrlKey: true, shiftKey: options.shiftKey ?? false }),
    );
  });
}

beforeEach(() => {
  terminalViewRenders.length = 0;
  mocks.invoke.mockReset();
  mocks.invoke.mockImplementation((command) => {
    if (command === "active_paste_target") {
      return Promise.resolve({ processName: "pwsh.exe", adapterName: null, supported: false });
    }
    return Promise.resolve(undefined);
  });
});

afterEach(cleanup);

describe("App tab mounting", () => {
  it("mounts one TerminalView per tab; only the active one is visible (inactive hidden, still mounted)", async () => {
    render(<App />);
    // The lazy TerminalView resolves through Suspense: one tab to start.
    expect(await screen.findAllByTestId("terminal-view")).toHaveLength(1);

    // Ctrl+T opens a second tab; BOTH stay mounted.
    pressChord("t");
    const views = screen.getAllByTestId("terminal-view");
    expect(views).toHaveLength(2);

    // Exactly one is visible; the other is hidden via visibility:hidden (never
    // unmounted, so its session keeps draining).
    const visible = views.filter((view) => view.getAttribute("data-visible") === "true");
    expect(visible).toHaveLength(1);
    const hidden = views.filter((view) => view.style.visibility === "hidden");
    expect(hidden).toHaveLength(1);
  });
});

describe("App workspace controls", () => {
  it("renders workspace controls separately from terminal tabs without issuing pty_spawn", async () => {
    mocks.invoke.mockImplementation((command) => {
      if (command === "active_paste_target") {
        return Promise.resolve({ processName: "pwsh.exe", adapterName: null, supported: false });
      }
      if (command === "workspace_list") {
        return Promise.resolve([
          {
            id: "project-alpha",
            name: "Project Alpha",
            working_directory: "/projects/alpha",
            environment: { profile: "default", variable_names: [] },
            agent: { id: "generic-tui", command: "bash" },
            session_ids: [],
          },
        ]);
      }
      return Promise.resolve(undefined);
    });

    render(<App />);

    expect(await screen.findByRole("button", { name: "Project Alpha" })).toBeTruthy();
    expect(mocks.invoke).toHaveBeenCalledWith("workspace_list");
    expect(mocks.invoke.mock.calls.some(([command]) => command === "pty_spawn")).toBe(false);
  });
});

describe("App tab chords (window capture)", () => {
  it("Ctrl+T creates, Ctrl+W closes, and Ctrl+W on the last tab yields a fresh one (app stays open)", async () => {
    render(<App />);
    expect(await screen.findAllByTestId("terminal-view")).toHaveLength(1);

    pressChord("t");
    expect(screen.getAllByTestId("terminal-view")).toHaveLength(2);

    pressChord("w");
    expect(screen.getAllByTestId("terminal-view")).toHaveLength(1);

    // Closing the final tab must NOT drop to zero — it auto-opens a fresh tab.
    pressChord("w");
    expect(screen.getAllByTestId("terminal-view")).toHaveLength(1);
  });

  it("Ctrl+Tab cycles the active tab and wraps", async () => {
    render(<App />);
    expect(await screen.findAllByTestId("terminal-view")).toHaveLength(1);

    // Second tab becomes active on creation.
    pressChord("t");
    let views = screen.getAllByTestId("terminal-view");
    expect(views[1].getAttribute("data-active")).toBe("true");

    // Ctrl+Tab from the last tab wraps forward to the first.
    pressChord("Tab");
    views = screen.getAllByTestId("terminal-view");
    expect(views[0].getAttribute("data-active")).toBe("true");
  });
});

describe("App global terminal settings", () => {
  it("propagates a settings change to ALL mounted terminals, keeps ONE panel, and new tabs inherit", async () => {
    render(<App />);
    expect(await screen.findAllByTestId("terminal-view")).toHaveLength(1);

    // Open the single settings panel via the gear.
    fireEvent.click(screen.getByRole("button", { name: "Settings" }));
    expect(screen.getAllByTestId("settings-panel")).toHaveLength(1);

    // Two mounted terminals, both at the default font size.
    pressChord("t");
    let views = screen.getAllByTestId("terminal-view");
    expect(views).toHaveLength(2);
    expect(views.every((view) => view.getAttribute("data-fontsize") === "14")).toBe(true);

    // A settings change flows to EVERY mounted terminal (shared prop).
    fireEvent.click(screen.getByTestId("bump-font"));
    views = screen.getAllByTestId("terminal-view");
    expect(views.every((view) => view.getAttribute("data-fontsize") === "20")).toBe(true);

    // A newly created tab inherits the current (changed) settings.
    pressChord("t");
    views = screen.getAllByTestId("terminal-view");
    expect(views).toHaveLength(3);
    expect(views.every((view) => view.getAttribute("data-fontsize") === "20")).toBe(true);

    // Still exactly one settings panel instance across all tabs.
    expect(screen.getAllByTestId("settings-panel")).toHaveLength(1);
  });
});

describe("App paste routing", () => {
  it("routes a text paste to the ACTIVE tab's recorded session id", async () => {
    render(<App />);
    await screen.findAllByTestId("terminal-view");

    const props = activeViewProps();
    expect(props).toBeDefined();

    // The active tab's PTY becomes ready with session id 42.
    act(() => {
      (props?.onPtyReady as (sessionId: number) => void)(42);
    });

    // A text paste must write to session 42 (the active tab), not the 0 sentinel.
    await act(async () => {
      await (props?.onTextPaste as (text: string) => Promise<void>)("hello");
    });

    const writeCall = mocks.invoke.mock.calls.find(([command]) => command === "pty_write");
    expect(writeCall?.[1]).toEqual({ data: "hello", sessionId: 42 });
  });
});
