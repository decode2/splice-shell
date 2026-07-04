import { useMemo, useReducer } from "react";
import type { ActivePasteTargetState } from "../paste/activePasteTarget";
import type { SessionHealth } from "./TitleBar";

// A single terminal tab. `tabId` is the client-side identity used as the React
// key and NEVER changes for the life of the tab; `sessionId` is the backend PTY
// id, which is (re)assigned on every spawn/restart (see `recordSession`). Keying
// the UI on `tabId` rather than `sessionId` is what lets `restartPty` swap the
// underlying session without remounting xterm and dropping the buffer.
export type TabState = {
  tabId: string;
  sessionId?: number;
  title: string;
  adapterState: ActivePasteTargetState;
  health: SessionHealth;
};

export type SessionsState = {
  tabs: TabState[];
  activeId: string;
  // Monotonic counter driving `tabId` generation. Kept in state so the reducer
  // stays PURE (no module-level mutable counter) while still producing unique,
  // ever-increasing ids that are independent of any backend session id.
  nextTabSeq: number;
};

export type SessionsAction =
  | { type: "createTab" }
  | { type: "closeTab"; tabId: string }
  | { type: "setActive"; tabId: string }
  | { type: "cycleTab"; direction: "next" | "prev" }
  | { type: "recordSession"; tabId: string; sessionId: number }
  | { type: "recordHealth"; tabId: string; health: SessionHealth }
  | { type: "recordAdapter"; tabId: string; adapterState: ActivePasteTargetState };

export const DEFAULT_TAB_TITLE = "shell";
export const DEFAULT_TAB_HEALTH: SessionHealth = "healthy";
// Matches App's initial paste-target state so a tab reads as "detecting" until
// its adapter resolves, rather than flashing an empty chip.
export const DEFAULT_TAB_ADAPTER_STATE: ActivePasteTargetState = {
  kind: "loading",
  message: "Detecting active paste target…",
};

// Build a fresh tab with the shared defaults. The caller owns id sequencing so
// this stays a pure value factory.
function makeTab(seq: number): TabState {
  return {
    tabId: `tab-${seq}`,
    title: DEFAULT_TAB_TITLE,
    adapterState: DEFAULT_TAB_ADAPTER_STATE,
    health: DEFAULT_TAB_HEALTH,
  };
}

// Immutable per-tab patch: replaces the matching tab with a shallow-merged copy
// and leaves every sibling untouched. Shared by all three per-tab recorders.
function patchTab(
  state: SessionsState,
  tabId: string,
  patch: Partial<TabState>,
): SessionsState {
  return {
    ...state,
    tabs: state.tabs.map((tab) => (tab.tabId === tabId ? { ...tab, ...patch } : tab)),
  };
}

export function createInitialSessionsState(): SessionsState {
  return {
    tabs: [makeTab(0)],
    activeId: "tab-0",
    nextTabSeq: 1,
  };
}

export function sessionsReducer(
  state: SessionsState,
  action: SessionsAction,
): SessionsState {
  switch (action.type) {
    case "createTab": {
      const tab = makeTab(state.nextTabSeq);
      return {
        tabs: [...state.tabs, tab],
        activeId: tab.tabId,
        nextTabSeq: state.nextTabSeq + 1,
      };
    }

    case "closeTab": {
      const index = state.tabs.findIndex((tab) => tab.tabId === action.tabId);
      if (index === -1) {
        return state;
      }

      // Closing the final tab must never leave the window tab-less: replace it
      // atomically with a fresh, active tab. Only the window X exits the app.
      if (state.tabs.length === 1) {
        const tab = makeTab(state.nextTabSeq);
        return {
          tabs: [tab],
          activeId: tab.tabId,
          nextTabSeq: state.nextTabSeq + 1,
        };
      }

      // Remove ONLY — killing the PTY is owned by TerminalView's unmount effect
      // (keyed by tabId). The reducer never calls killPty, avoiding a double
      // kill / restart race.
      const remaining = state.tabs.filter((tab) => tab.tabId !== action.tabId);

      let activeId = state.activeId;
      if (state.activeId === action.tabId) {
        // Prefer the right neighbor (now shifted into `index`), else fall back
        // to the left neighbor when the closed tab was last (browser convention).
        const rightIndex = index < remaining.length ? index : remaining.length - 1;
        activeId = remaining[rightIndex].tabId;
      }

      return { ...state, tabs: remaining, activeId };
    }

    case "setActive": {
      if (!state.tabs.some((tab) => tab.tabId === action.tabId)) {
        return state;
      }
      return { ...state, activeId: action.tabId };
    }

    case "cycleTab": {
      if (state.tabs.length === 0) {
        return state;
      }
      const index = state.tabs.findIndex((tab) => tab.tabId === state.activeId);
      if (index === -1) {
        return state;
      }
      const delta = action.direction === "next" ? 1 : -1;
      // +length before the modulo keeps `prev` from going negative so both
      // directions wrap around cleanly.
      const nextIndex = (index + delta + state.tabs.length) % state.tabs.length;
      return { ...state, activeId: state.tabs[nextIndex].tabId };
    }

    case "recordSession":
      return patchTab(state, action.tabId, { sessionId: action.sessionId });

    case "recordHealth":
      return patchTab(state, action.tabId, { health: action.health });

    case "recordAdapter":
      return patchTab(state, action.tabId, { adapterState: action.adapterState });

    default:
      return state;
  }
}

// App-level hook wrapping the pure reducer with stable, ergonomic action
// creators so the component tree never dispatches raw action objects.
export function useSessions() {
  const [state, dispatch] = useReducer(
    sessionsReducer,
    undefined,
    createInitialSessionsState,
  );

  const actions = useMemo(
    () => ({
      createTab: () => dispatch({ type: "createTab" }),
      closeTab: (tabId: string) => dispatch({ type: "closeTab", tabId }),
      setActive: (tabId: string) => dispatch({ type: "setActive", tabId }),
      cycleTab: (direction: "next" | "prev") => dispatch({ type: "cycleTab", direction }),
      // Bound in App to TerminalView's `onPtyReady(sessionId)` per tab so a
      // (re)spawn patches sessionId in place without changing tabId.
      recordSession: (tabId: string, sessionId: number) =>
        dispatch({ type: "recordSession", tabId, sessionId }),
      recordHealth: (tabId: string, health: SessionHealth) =>
        dispatch({ type: "recordHealth", tabId, health }),
      recordAdapter: (tabId: string, adapterState: ActivePasteTargetState) =>
        dispatch({ type: "recordAdapter", tabId, adapterState }),
    }),
    [],
  );

  return {
    tabs: state.tabs,
    activeId: state.activeId,
    ...actions,
  };
}
