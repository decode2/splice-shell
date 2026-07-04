// @vitest-environment jsdom
import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import {
  createInitialSessionsState,
  sessionsReducer,
  useSessions,
  type SessionsState,
} from "./useSessions";

// Build a deterministic multi-tab state by replaying createTab from the initial
// state, so tab ids follow the same monotonic scheme the reducer produces in
// production.
function stateWithTabs(count: number): SessionsState {
  let state = createInitialSessionsState();
  for (let i = 1; i < count; i += 1) {
    state = sessionsReducer(state, { type: "createTab" });
  }
  return state;
}

describe("sessionsReducer — createTab", () => {
  it("starts with exactly one active tab carrying the defaults", () => {
    const state = createInitialSessionsState();
    expect(state.tabs).toHaveLength(1);
    expect(state.activeId).toBe(state.tabs[0].tabId);
    expect(state.tabs[0]).toMatchObject({
      title: expect.any(String),
      health: "healthy",
      adapterState: expect.objectContaining({ kind: "loading" }),
    });
    expect(state.tabs[0].sessionId).toBeUndefined();
  });

  it("appends a new tab with defaults and makes it active (spec: Create spawns session)", () => {
    const initial = createInitialSessionsState();
    const next = sessionsReducer(initial, { type: "createTab" });

    expect(next.tabs).toHaveLength(2);
    const appended = next.tabs[1];
    expect(appended.title).toEqual(expect.any(String));
    expect(appended.health).toBe("healthy");
    expect(appended.adapterState.kind).toBe("loading");
    expect(next.activeId).toBe(appended.tabId);
  });

  it("generates monotonic tab ids independent of sessionId", () => {
    const state = stateWithTabs(3);
    const ids = state.tabs.map((tab) => tab.tabId);
    expect(new Set(ids).size).toBe(3);
    // None of the ids depend on a session id (no session has been recorded yet).
    expect(state.tabs.every((tab) => tab.sessionId === undefined)).toBe(true);
  });
});

describe("sessionsReducer — closeTab", () => {
  it("removes the tab from the array only, leaving sibling sessions untouched", () => {
    let state = stateWithTabs(3);
    // Record distinct session ids so we can prove closeTab does not touch them.
    state = sessionsReducer(state, {
      type: "recordSession",
      tabId: state.tabs[0].tabId,
      sessionId: 10,
    });
    state = sessionsReducer(state, {
      type: "recordSession",
      tabId: state.tabs[2].tabId,
      sessionId: 30,
    });

    const closedId = state.tabs[1].tabId;
    const survivorFirst = state.tabs[0];
    const survivorLast = state.tabs[2];

    const next = sessionsReducer(state, { type: "closeTab", tabId: closedId });

    expect(next.tabs).toHaveLength(2);
    expect(next.tabs.some((tab) => tab.tabId === closedId)).toBe(false);
    // The reducer removes only — it never mutates the surviving sessions (kill
    // is owned by TerminalView's unmount effect, not the reducer).
    expect(next.tabs[0]).toEqual(survivorFirst);
    expect(next.tabs[1]).toEqual(survivorLast);
  });

  it("moves active to the right neighbor when the active tab is closed", () => {
    const state = stateWithTabs(3);
    const active = sessionsReducer(state, { type: "setActive", tabId: state.tabs[1].tabId });

    const next = sessionsReducer(active, { type: "closeTab", tabId: active.tabs[1].tabId });

    // Closing B (index 1) with A,B,C → active becomes C (the right neighbor).
    expect(next.activeId).toBe(state.tabs[2].tabId);
  });

  it("falls back to the left neighbor when the closed active tab was last", () => {
    const state = stateWithTabs(3);
    const active = sessionsReducer(state, { type: "setActive", tabId: state.tabs[2].tabId });

    const next = sessionsReducer(active, { type: "closeTab", tabId: active.tabs[2].tabId });

    // Closing C (last) → no right neighbor, so active falls back to B.
    expect(next.activeId).toBe(state.tabs[1].tabId);
  });

  it("keeps the active tab unchanged when a non-active tab is closed", () => {
    const state = stateWithTabs(3);
    const active = sessionsReducer(state, { type: "setActive", tabId: state.tabs[0].tabId });

    const next = sessionsReducer(active, { type: "closeTab", tabId: active.tabs[2].tabId });

    expect(next.activeId).toBe(state.tabs[0].tabId);
  });

  it("replaces the final tab with a fresh active tab so the window is never tab-less", () => {
    const state = createInitialSessionsState();
    const onlyId = state.tabs[0].tabId;

    const next = sessionsReducer(state, { type: "closeTab", tabId: onlyId });

    expect(next.tabs).toHaveLength(1);
    // A brand-new tab id (not the closed one) that is active.
    expect(next.tabs[0].tabId).not.toBe(onlyId);
    expect(next.activeId).toBe(next.tabs[0].tabId);
    expect(next.tabs[0].sessionId).toBeUndefined();
  });
});

describe("sessionsReducer — setActive and cycleTab", () => {
  it("setActive switches the active id to an existing tab", () => {
    const state = stateWithTabs(3);
    const next = sessionsReducer(state, { type: "setActive", tabId: state.tabs[1].tabId });
    expect(next.activeId).toBe(state.tabs[1].tabId);
  });

  it("setActive ignores unknown tab ids", () => {
    const state = stateWithTabs(2);
    const next = sessionsReducer(state, { type: "setActive", tabId: "tab-does-not-exist" });
    expect(next.activeId).toBe(state.activeId);
  });

  it("cycleTab next advances and wraps around", () => {
    let state = stateWithTabs(3);
    state = sessionsReducer(state, { type: "setActive", tabId: state.tabs[0].tabId });

    state = sessionsReducer(state, { type: "cycleTab", direction: "next" });
    expect(state.activeId).toBe(state.tabs[1].tabId);
    state = sessionsReducer(state, { type: "cycleTab", direction: "next" });
    expect(state.activeId).toBe(state.tabs[2].tabId);
    // Wrap forward from the last tab back to the first.
    state = sessionsReducer(state, { type: "cycleTab", direction: "next" });
    expect(state.activeId).toBe(state.tabs[0].tabId);
  });

  it("cycleTab prev retreats and wraps around", () => {
    let state = stateWithTabs(3);
    state = sessionsReducer(state, { type: "setActive", tabId: state.tabs[0].tabId });

    // Wrap backward from the first tab to the last.
    state = sessionsReducer(state, { type: "cycleTab", direction: "prev" });
    expect(state.activeId).toBe(state.tabs[2].tabId);
    state = sessionsReducer(state, { type: "cycleTab", direction: "prev" });
    expect(state.activeId).toBe(state.tabs[1].tabId);
  });
});

describe("sessionsReducer — per-tab recorders", () => {
  it("recordSession patches sessionId in place without changing tabId", () => {
    const state = stateWithTabs(2);
    const targetId = state.tabs[1].tabId;

    const next = sessionsReducer(state, {
      type: "recordSession",
      tabId: targetId,
      sessionId: 42,
    });

    const patched = next.tabs[1];
    expect(patched.tabId).toBe(targetId);
    expect(patched.sessionId).toBe(42);
    // Restart records a NEW session id on the SAME tab id (no remount key change).
    const restarted = sessionsReducer(next, {
      type: "recordSession",
      tabId: targetId,
      sessionId: 99,
    });
    expect(restarted.tabs[1].tabId).toBe(targetId);
    expect(restarted.tabs[1].sessionId).toBe(99);
  });

  it("recordHealth and recordAdapter update only the targeted tab", () => {
    const state = stateWithTabs(2);
    const targetId = state.tabs[0].tabId;
    const otherId = state.tabs[1].tabId;

    let next = sessionsReducer(state, {
      type: "recordHealth",
      tabId: targetId,
      health: "reconnecting",
    });
    next = sessionsReducer(next, {
      type: "recordAdapter",
      tabId: targetId,
      adapterState: { kind: "ready", processName: "codex.exe", adapterName: "codex" },
    });

    const target = next.tabs.find((tab) => tab.tabId === targetId);
    const other = next.tabs.find((tab) => tab.tabId === otherId);
    expect(target?.health).toBe("reconnecting");
    expect(target?.adapterState).toEqual({
      kind: "ready",
      processName: "codex.exe",
      adapterName: "codex",
    });
    // The sibling keeps its defaults — recorders are per-tab.
    expect(other?.health).toBe("healthy");
    expect(other?.adapterState.kind).toBe("loading");
  });
});

describe("useSessions hook", () => {
  it("exposes the initial single-tab state and bound actions", () => {
    const { result } = renderHook(() => useSessions());

    expect(result.current.tabs).toHaveLength(1);
    expect(result.current.activeId).toBe(result.current.tabs[0].tabId);

    act(() => {
      result.current.createTab();
    });
    expect(result.current.tabs).toHaveLength(2);
    expect(result.current.activeId).toBe(result.current.tabs[1].tabId);

    const secondId = result.current.tabs[1].tabId;
    act(() => {
      result.current.recordSession(secondId, 7);
    });
    expect(result.current.tabs[1].sessionId).toBe(7);

    act(() => {
      result.current.cycleTab("next");
    });
    expect(result.current.activeId).toBe(result.current.tabs[0].tabId);
  });
});
