import type { KeyboardEvent as ReactKeyboardEvent, MouseEvent as ReactMouseEvent } from "react";
import { useRef } from "react";
import type { TabState } from "./useSessions";

type TabStripProps = {
  tabs: TabState[];
  activeId: string;
  onSelect: (tabId: string) => void;
  onClose: (tabId: string) => void;
  onCreate: () => void;
};

// Strips a Windows executable suffix so the tab reads as a shell/tool name
// ("pwsh", "codex") rather than a raw process path — mirrors the old global
// AdapterChip semantics, now per-tab.
function stripExecutableSuffix(name: string) {
  return name.replace(/\.exe$/i, "");
}

// The tab's visible label follows the adapter chip semantics: the resolved
// adapter name when ready, the (de-suffixed) process name in amber when the
// process is unsupported, and the tab's default title otherwise (loading/error).
function tabLabel(tab: TabState): { text: string; unsupported: boolean } {
  const adapter = tab.adapterState;
  if (adapter.kind === "ready") {
    return { text: adapter.adapterName, unsupported: false };
  }
  if (adapter.kind === "unsupported") {
    return { text: stripExecutableSuffix(adapter.processName), unsupported: true };
  }
  return { text: tab.title, unsupported: false };
}

// The per-tab strip that replaces the single global adapter chip + health dot.
// Each tab owns its adapter label, health dot, and close affordance; a trailing
// `+` creates a new tab. ARIA tablist semantics with roving tabindex: only the
// active tab is tabbable, arrow keys move focus, Enter/Space activate.
//
// Drag discipline: the strip container carries `data-tauri-drag-region` so its
// empty area drags the window; tabs and buttons deliberately omit it so a click
// on them acts instead of starting a window drag (Tauri matches the exact event
// target, not ancestors).
export function TabStrip({ tabs, activeId, onSelect, onClose, onCreate }: TabStripProps) {
  const listRef = useRef<HTMLDivElement | null>(null);

  const focusTabAt = (index: number) => {
    const elements = listRef.current?.querySelectorAll<HTMLElement>('[role="tab"]');
    if (!elements || elements.length === 0) {
      return;
    }
    const clamped = (index + elements.length) % elements.length;
    elements[clamped]?.focus();
  };

  const handleTabKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>, index: number, tabId: string) => {
    if (event.key === "Enter" || event.key === " " || event.key === "Spacebar") {
      // Activate the focused tab. preventDefault stops Space from scrolling.
      event.preventDefault();
      onSelect(tabId);
      return;
    }

    if (event.key === "ArrowRight" || event.key === "ArrowLeft") {
      // Roving focus: move focus without selecting (selection follows Enter/Space).
      event.preventDefault();
      focusTabAt(index + (event.key === "ArrowRight" ? 1 : -1));
    }
  };

  const handleCloseClick = (event: ReactMouseEvent<HTMLButtonElement>, tabId: string) => {
    // A close click must never bubble up to the tab's select handler.
    event.stopPropagation();
    onClose(tabId);
  };

  return (
    <div
      ref={listRef}
      className="tabstrip"
      role="tablist"
      aria-label="Terminal tabs"
      data-tauri-drag-region
    >
      {tabs.map((tab, index) => {
        const isActive = tab.tabId === activeId;
        const { text, unsupported } = tabLabel(tab);
        return (
          <div
            key={tab.tabId}
            role="tab"
            aria-selected={isActive}
            tabIndex={isActive ? 0 : -1}
            className={`tabstrip-tab${isActive ? " tabstrip-tab--active" : ""}`}
            data-active={isActive || undefined}
            onClick={() => onSelect(tab.tabId)}
            onKeyDown={(event) => handleTabKeyDown(event, index, tab.tabId)}
          >
            <span
              className={`tabstrip-dot tabstrip-dot--${tab.health}`}
              data-health={tab.health}
              aria-hidden="true"
            />
            <span
              className={`tabstrip-title${unsupported ? " tabstrip-title--unsupported" : ""}`}
            >
              {text}
            </span>
            <button
              type="button"
              className="tabstrip-close"
              aria-label="Close tab"
              onClick={(event) => handleCloseClick(event, tab.tabId)}
            >
              <CloseGlyph />
            </button>
          </div>
        );
      })}
      <button
        type="button"
        className="tabstrip-new"
        aria-label="New tab"
        onClick={onCreate}
      >
        <PlusGlyph />
      </button>
    </div>
  );
}

function CloseGlyph() {
  return (
    <svg className="tabstrip-glyph" viewBox="0 0 10 10" aria-hidden="true" focusable="false">
      <line x1="1.5" y1="1.5" x2="8.5" y2="8.5" stroke="currentColor" strokeWidth="1.2" />
      <line x1="8.5" y1="1.5" x2="1.5" y2="8.5" stroke="currentColor" strokeWidth="1.2" />
    </svg>
  );
}

function PlusGlyph() {
  return (
    <svg className="tabstrip-glyph" viewBox="0 0 10 10" aria-hidden="true" focusable="false">
      <line x1="5" y1="1.5" x2="5" y2="8.5" stroke="currentColor" strokeWidth="1.2" />
      <line x1="1.5" y1="5" x2="8.5" y2="5" stroke="currentColor" strokeWidth="1.2" />
    </svg>
  );
}
