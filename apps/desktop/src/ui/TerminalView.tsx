import { usePtySession } from "./usePtySession";
import { DEFAULT_TERMINAL_SETTINGS, type TerminalSettings } from "./terminalSettings";
import type { SessionHealth } from "./TitleBar";

type TerminalViewProps = {
  // Whether this instance is the visible/foreground tab. When it flips true the
  // instance re-fits (it may have been hidden and resized) and takes focus.
  // Defaults to false so existing single-instance callers keep exactly one
  // mount-time focus (from the bridge) and no extra activation fit.
  active?: boolean;
  // Whether the instance's frame is shown. Inactive tabs stay MOUNTED (so their
  // session keeps draining) but hidden via CSS; App owns the stacking layout in
  // a later slice. Defaults to true so current callers render unchanged.
  visible?: boolean;
  // Global terminal settings, owned by App so every tab shares ONE source of
  // truth (spec: settings MUST NOT diverge per tab). App always supplies this;
  // it defaults to the module constant `DEFAULT_TERMINAL_SETTINGS` so there is
  // no per-instance mutable settings state to drift — the local settings
  // `useState` and the embedded panel were removed when the panel was lifted to
  // App (App renders a single `TerminalSettingsPanel`).
  settings?: TerminalSettings;
  onClipboardImagePaste?: () => void | Promise<void>;
  onInput?: (data: string) => void | Promise<void>;
  onInputActivity?: () => void;
  onSessionHealth?: (status: SessionHealth) => void;
  onTextPaste?: (text: string) => void | Promise<void>;
  onResize?: (size: { cols: number; rows: number }) => void;
  onPtyReady?: (sessionId: number) => void;
};

// Presentational shell for a single terminal tab. All PTY session lifecycle
// state (spawn/restart machine, storm guard, early-output queue, demux, health,
// disposal) lives in `usePtySession`; this component only mounts the host
// element the hook owns and reflects `active`/`visible` into the frame.
export function TerminalView({
  active = false,
  visible = true,
  settings = DEFAULT_TERMINAL_SETTINGS,
  onClipboardImagePaste,
  onInput,
  onInputActivity,
  onSessionHealth,
  onTextPaste,
  onPtyReady,
  onResize,
}: TerminalViewProps) {
  const { terminalElementRef } = usePtySession({
    active,
    settings,
    onClipboardImagePaste,
    onInput,
    onInputActivity,
    onSessionHealth,
    onTextPaste,
    onPtyReady,
    onResize,
  });

  return (
    <section
      className={visible ? "terminal-frame" : "terminal-frame terminal-frame--hidden"}
      aria-label="Terminal"
      aria-hidden={!visible || undefined}
      data-active={active || undefined}
      // Inactive tabs stay MOUNTED so their session keeps draining; they are
      // hidden with `visibility` (never `display:none`, which would collapse the
      // fit to 0×0). App owns the stacked-frame layout in a later slice; this
      // inline fallback keeps the prop meaningful until then.
      style={visible ? undefined : { visibility: "hidden" }}
    >
      <div className="terminal-host" ref={terminalElementRef} />
    </section>
  );
}
