import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import type { ActivePasteTargetState } from "../paste/activePasteTarget";
import type { PastePreviewState } from "../paste/pastePreview";
import { createLocalFileLinkProvider } from "../terminal/fileLinks";
import { shouldRefreshTargetAfterInput } from "../terminal/inputActivity";
import { resolveTerminalKeyAction } from "../terminal/keyboardShortcuts";
import { writeClipboardText } from "../terminal/clipboardClient";
import {
  killPty,
  isPtyExitPayload,
  isPtyOutputPayload,
  PTY_EXIT_EVENT,
  PTY_OUTPUT_EVENT,
  resizePty,
  spawnPty,
  writePty,
} from "../terminal/ptyClient";
import { createTerminalBridge } from "../terminal/terminalBridge";
import {
  coalesceTerminalOutputActions,
  createTerminalOutputFilter,
  type TerminalOutputAction,
} from "../terminal/terminalOutputFilter";
import { createTerminalOutputScheduler } from "../terminal/terminalOutputScheduler";
import { shouldRecoverClosedPtyInput } from "../terminal/ptyRecovery";

type TerminalSettings = {
  background: string;
  foreground: string;
  fontSize: number;
};

const DEFAULT_TERMINAL_SETTINGS: TerminalSettings = {
  background: "#020617",
  foreground: "#dbeafe",
  fontSize: 14,
};

type TerminalViewProps = {
  activePasteTargetState: ActivePasteTargetState;
  pasteState: PastePreviewState;
  onClipboardImagePaste?: () => void | Promise<void>;
  onInput?: (data: string) => void | Promise<void>;
  onInputActivity?: () => void;
  onTextPaste?: (text: string) => void | Promise<void>;
  onResize?: (size: { cols: number; rows: number }) => void;
  onPtyReady?: () => void;
};

export function TerminalView({
  activePasteTargetState,
  pasteState,
  onClipboardImagePaste,
  onInput = writePty,
  onInputActivity,
  onTextPaste,
  onPtyReady,
  onResize = resizePty,
}: TerminalViewProps) {
  const terminalElementRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const [hasInputActivity, setHasInputActivity] = useState(false);
  const [hasPtyOutput, setHasPtyOutput] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [terminalSettings, setTerminalSettings] = useState(DEFAULT_TERMINAL_SETTINGS);
  const handlersRef = useRef({
    onClipboardImagePaste,
    onInput,
    onInputActivity,
    onTextPaste,
    onPtyReady,
    onResize,
  });

  handlersRef.current = {
    onClipboardImagePaste,
    onInput,
    onInputActivity,
    onTextPaste,
    onPtyReady,
    onResize,
  };

  useEffect(() => {
    const terminal = terminalRef.current;
    if (!terminal) {
      return;
    }

    terminal.options.fontSize = terminalSettings.fontSize;
    terminal.options.theme = {
      background: terminalSettings.background,
      foreground: terminalSettings.foreground,
      cursor: "#38bdf8",
      selectionBackground: "#1e3a8a",
    };

    // Changing the font size changes the cell dimensions, so the grid must be
    // refit or the row/col count goes stale — leaving a growing unpainted band
    // and a ConPTY size mismatch. fit() flows through terminal.onResize ->
    // the bridge's onResize -> resizePty, keeping the backend in sync.
    fitAddonRef.current?.fit();
  }, [terminalSettings]);

  useEffect(() => {
    const terminalElement = terminalElementRef.current;
    if (!terminalElement) {
      return undefined;
    }

    const terminal = new Terminal({
      allowProposedApi: true,
      cursorBlink: true,
      fontFamily:
        '"CaskaydiaCove Nerd Font", "CaskaydiaCove NF", "JetBrainsMono Nerd Font", "FiraCode Nerd Font", "Cascadia Code", "Fira Code", Consolas, monospace',
      fontSize: terminalSettings.fontSize,
      theme: {
        background: terminalSettings.background,
        foreground: terminalSettings.foreground,
        cursor: "#38bdf8",
        selectionBackground: "#1e3a8a",
      },
      windowOptions: {
        getCellSizePixels: true,
        getWinSizePixels: true,
        getWinSizeChars: true,
      },
      windowsPty: {
        backend: "conpty",
      },
    });
    terminalRef.current = terminal;
    const fitAddon = new FitAddon();
    fitAddonRef.current = fitAddon;
    terminal.loadAddon(fitAddon);
    const fileLinkProvider = terminal.registerLinkProvider(createLocalFileLinkProvider(terminal));
    let disposed = false;
    let outputSeen = false;
    let inputClosed = false;
    let restartInFlight = false;
    let ptyGeneration = 0;
    let currentSessionId: number | undefined;
    // Restart-storm guard. The removed 500ms liveness poll was an implicit
    // throttle; without it, a shell that exits instantly on every launch would
    // spin `restartPty` forever. Cap consecutive restarts and reset the counter
    // only when a session proves healthy — output alone is not enough, because
    // a shell that prints a banner and immediately dies would otherwise reset
    // the counter every cycle and defeat the cap. The reset is gated on a
    // minimum uptime (see `writePtyOutput` / `MIN_HEALTHY_UPTIME_MS`).
    let consecutiveRestarts = 0;
    // Wall-clock timestamp of the last spawn, used to measure session uptime
    // for the storm-guard reset gate above. Set in `startPty`.
    let spawnTime = 0;
    // A `pty-exit` can arrive for a session id we have not recorded yet: an
    // instant-exit child emits `pty-exit(N)` BEFORE `spawnPty` resolves and
    // sets `currentSessionId = N`. The (FnOnce) event would be dropped forever,
    // leaving a dead terminal. Stash such a newer-than-recorded id here so
    // `startPty` can act on it the moment it records that id.
    let lastUnmatchedExitId: number | undefined;
    // Set when a `pty-exit` for the live session lands while a restart is
    // already in flight, so the restart is coalesced (see `restartPty`).
    let pendingExit = false;
    let unlistenPtyOutput: UnlistenFn | undefined;
    let unlistenPtyExit: UnlistenFn | undefined;
    const MAX_CONSECUTIVE_RESTARTS = 5;
    // A session must stay up at least this long before its output is taken as
    // proof of health. Output before this window is likely a dying shell's
    // banner/error and must not reset the restart-storm counter.
    const MIN_HEALTHY_UPTIME_MS = 2000;

    const writeTerminalChunk = (chunk: string) => {
      terminal.write(chunk);
    };
    const outputScheduler = createTerminalOutputScheduler({
      write: writeTerminalChunk,
    });
    const writeTerminalActions = (actions: TerminalOutputAction[]) => {
      for (const action of coalesceTerminalOutputActions(actions)) {
        outputScheduler.write(action.data);
      }
    };
    // Created here (after `writeTerminalActions`) so the cursor-show holdback's
    // deferred emissions can route through the exact same actions → scheduler →
    // xterm pipeline as normal write() output. The sink is disposed-guarded: a
    // quiet-timer fire that somehow lands after teardown must not touch a
    // disposed terminal. In practice teardown's `outputFilter.flush()` cancels
    // the timer, so this guard is defensive. A real setTimeout/clearTimeout is
    // injected for production timing.
    const outputFilter = createTerminalOutputFilter({
      onDeferredOutput: (actions) => {
        if (!disposed) {
          writeTerminalActions(actions);
        }
      },
      timer: {
        set: (callback, ms) => setTimeout(callback, ms),
        clear: (handle) => clearTimeout(handle as ReturnType<typeof setTimeout>),
      },
    });
    const writePtyOutput = (chunk: string) => {
      if (!outputSeen) {
        outputSeen = true;
        setHasPtyOutput(true);
      }

      // A session that has stayed up past the min-uptime threshold and is now
      // producing output is genuinely healthy, so clear the restart-storm
      // counter: a later, legitimate one-off restart should not be counted
      // against a burst of instant-exit failures from long ago. Output that
      // arrives within the first `MIN_HEALTHY_UPTIME_MS` is NOT trusted — a
      // shell that prints then instantly dies must keep accumulating restarts
      // toward the cap, otherwise the storm guard is defeated by any output.
      if (Date.now() - spawnTime > MIN_HEALTHY_UPTIME_MS) {
        consecutiveRestarts = 0;
      }

      writeTerminalActions(outputFilter.write(chunk));
    };
    const startPty = async () => {
      const spawnedSessionId = await spawnPty({ cols: terminal.cols, rows: terminal.rows });
      currentSessionId = spawnedSessionId;
      spawnTime = Date.now();
      ptyGeneration += 1;
      inputClosed = false;
      handlersRef.current.onPtyReady?.();

      // A `pty-exit` for this exact session may have raced ahead of `spawnPty`
      // resolving (instant-exit child) and been stashed by `handlePtyExit`.
      // Now that the id is recorded, treat it as an immediate exit and restart.
      // This shares the restart path, so the storm cap still bounds an
      // instant-exit shell instead of letting it loop forever.
      if (lastUnmatchedExitId === spawnedSessionId) {
        lastUnmatchedExitId = undefined;
        inputClosed = true;
        void restartPty();
      }
    };
    const restartPty = async () => {
      if (disposed) {
        return;
      }

      // A restart already running: coalesce this request. `restartPty`'s
      // finally re-checks `pendingExit` and runs exactly one more restart, so
      // an exit that races an in-flight restart is never dropped nor allowed
      // to stack into overlapping spawns.
      if (restartInFlight) {
        pendingExit = true;
        return;
      }

      restartInFlight = true;
      try {
        consecutiveRestarts += 1;
        if (consecutiveRestarts > MAX_CONSECUTIVE_RESTARTS) {
          terminal.write(
            "\r\nPTY session keeps exiting immediately; automatic restart stopped.\r\n",
          );
          return;
        }

        terminal.write("\r\nPTY session ended. Starting a new shell...\r\n");
        // Flush AND reset the output filter before the new session starts. A
        // session that died mid-escape can leave a held partial (e.g.
        // `\x1b[?2`) and a stale open synthetic sync in the filter; carrying
        // that state into the next session would corrupt its first bytes
        // (malformed CSI swallowing the prompt) and suppress its first
        // synthetic span. flush() emits any held bytes plus the closing 2026l
        // and returns the filter to a clean, reusable state.
        writeTerminalActions(outputFilter.flush());
        await startPty();
        terminal.write("\r\nNew shell session started.\r\n");
      } catch (error) {
        terminal.write(`\r\nFailed to restart ConPTY session: ${String(error)}\r\n`);
      } finally {
        restartInFlight = false;
        if (pendingExit && !disposed) {
          pendingExit = false;
          void restartPty();
        }
      }
    };

    const sendTerminalInput = (data: string) => {
      if (inputClosed) {
        return;
      }

      const inputGeneration = ptyGeneration;
      setHasInputActivity(true);
      if (shouldRefreshTargetAfterInput(data)) {
        handlersRef.current.onInputActivity?.();
      }
      void Promise.resolve(handlersRef.current.onInput(data)).catch((error) => {
        if (isClosedPtyInputError(error)) {
          if (
            !shouldRecoverClosedPtyInput({
              currentGeneration: ptyGeneration,
              failedGeneration: inputGeneration,
              inputClosed,
            })
          ) {
            return;
          }

          inputClosed = true;
          void restartPty();
          return;
        }

        terminal.write(`\r\nPTY input failed: ${String(error)}\r\n`);
      });
    };
    const bridge = createTerminalBridge({
      terminal,
      fitAddon,
      container: terminalElement,
      onInput: sendTerminalInput,
      onResize: (size) => {
        void Promise.resolve(handlersRef.current.onResize(size)).catch((error) => {
          terminal.write(`\r\nPTY resize failed: ${String(error)}\r\n`);
        });
      },
    });

    // WebGL renderer avoids the DOM renderer's glyph clipping (Nerd Font icons get cut off) and
    // is faster. It needs the terminal's canvas to already be mounted, which createTerminalBridge
    // guarantees via terminal.open() above. WebGL can be unavailable (no GPU, disabled in
    // WebView2, etc.), so construction/loading is best-effort and never crashes the terminal.
    let webglAddon: WebglAddon | undefined;
    try {
      webglAddon = new WebglAddon();
      terminal.loadAddon(webglAddon);
      webglAddon.onContextLoss(() => {
        webglAddon?.dispose();
      });
    } catch (error) {
      console.error("WebGL renderer unavailable, falling back to the DOM renderer.", error);
      webglAddon = undefined;
    }

    // A `pty-exit` event carries the exiting session's monotonic id. Ids
    // increase monotonically, so the payload is compared against the live id:
    //   - below current  → stale exit from an already-superseded session; ignore.
    //   - equal to current → the live shell died; mark input closed and restart.
    //   - above current, or none recorded yet → an instant-exit child whose
    //     `pty-exit` raced ahead of `spawnPty` resolving. Do NOT drop it (the
    //     event is FnOnce and never re-fires); stash it so `startPty` restarts
    //     the moment it records that id.
    const handlePtyExit = (payload: unknown) => {
      if (disposed || !isPtyExitPayload(payload)) {
        return;
      }

      if (currentSessionId !== undefined && payload < currentSessionId) {
        return;
      }

      if (payload === currentSessionId) {
        inputClosed = true;
        void restartPty();
        return;
      }

      lastUnmatchedExitId = payload;
    };

    const handlePaste = (event: ClipboardEvent) => {
      event.preventDefault();
      const text = event.clipboardData?.getData("text/plain");
      if (text) {
        void Promise.resolve(
          handlersRef.current.onTextPaste?.(text) ?? handlersRef.current.onInput(text),
        ).catch((error) => {
          terminal.write(`\r\nPTY paste failed: ${String(error)}\r\n`);
        });
        return;
      }

      void Promise.resolve(handlersRef.current.onClipboardImagePaste?.()).catch((error) => {
        terminal.write(`\r\nImage paste failed: ${String(error)}\r\n`);
      });
    };

    const handleTerminalKeyDown = (event: KeyboardEvent) => {
      const action = resolveTerminalKeyAction(event, terminal.hasSelection());

      if (action === "copy") {
        const selection = terminal.getSelection();
        // An empty selection (e.g. hasSelection reporting a zero-width range)
        // must NOT eat the key: leave the event untouched so its default
        // behavior proceeds instead of swallowing a no-op copy.
        if (!selection) {
          return;
        }

        event.preventDefault();
        event.stopPropagation();
        void writeClipboardText(selection).catch((error) => {
          terminal.write(`\r\nCopy to clipboard failed: ${String(error)}\r\n`);
        });
        terminal.clearSelection();
        return;
      }

      if (action === "interrupt") {
        event.preventDefault();
        event.stopPropagation();
        sendTerminalInput("\x03");
      }
    };

    terminalElement.addEventListener("paste", handlePaste, { capture: true });
    terminalElement.addEventListener("keydown", handleTerminalKeyDown, { capture: true });

    // Register BOTH the output and exit listeners before the first spawn, so
    // no `pty-output` or `pty-exit` event can be missed in the window between
    // spawning and subscribing.
    void Promise.all([
      listen(PTY_OUTPUT_EVENT, (event) => {
        if (!disposed && isPtyOutputPayload(event.payload)) {
          writePtyOutput(event.payload);
        }
      }),
      listen(PTY_EXIT_EVENT, (event) => {
        handlePtyExit(event.payload);
      }),
    ])
      .then(([outputUnlisten, exitUnlisten]) => {
        if (disposed) {
          outputUnlisten();
          exitUnlisten();
          return;
        }

        unlistenPtyOutput = outputUnlisten;
        unlistenPtyExit = exitUnlisten;
        void startPty().catch((error) => {
          terminal.write(`\r\nFailed to start ConPTY session: ${String(error)}\r\n`);
        });
      })
      .catch((error) => {
        terminal.write(`\r\nFailed to subscribe to PTY events: ${String(error)}\r\n`);
      });

    return () => {
      disposed = true;
      const filteredTail = outputFilter.flush();
      void writeTerminalActions(filteredTail);
      outputScheduler.flush();
      outputScheduler.dispose();
      terminalElement.removeEventListener("paste", handlePaste, { capture: true });
      terminalElement.removeEventListener("keydown", handleTerminalKeyDown, { capture: true });
      unlistenPtyOutput?.();
      unlistenPtyExit?.();
      void killPty();
      fileLinkProvider.dispose();
      webglAddon?.dispose();
      bridge.dispose();
      terminalRef.current = null;
    };
    // The terminal owns external resources. Callback changes must flow through handlersRef
    // so parent UI/HUD refreshes cannot remount xterm or restart the PTY.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <section className="terminal-frame" aria-label="Terminal">
      <div className="terminal-host" ref={terminalElementRef} />
      <footer className="terminal-statusbar">
        <span>
          ConPTY · input {hasInputActivity ? "yes" : "waiting"} · output{" "}
          {hasPtyOutput ? "yes" : "waiting"}
        </span>
        <ActivePasteTargetPanel activePasteTargetState={activePasteTargetState} />
        <PastePreviewPanel pasteState={pasteState} />
        <button
          className="terminal-settings-toggle"
          type="button"
          aria-expanded={settingsOpen}
          onClick={() => setSettingsOpen((current) => !current)}
        >
          Settings
        </button>
      </footer>
      {settingsOpen ? (
        <TerminalSettingsPanel
          settings={terminalSettings}
          onChange={setTerminalSettings}
          onClose={() => setSettingsOpen(false)}
        />
      ) : null}
    </section>
  );
}

function isClosedPtyInputError(error: unknown) {
  const message = error instanceof Error ? error.message : String(error);
  return (
    message.includes("PTY session closed") ||
    message.includes("PTY session is not running") ||
    message.includes("pipe is being closed") ||
    message.includes("pipe has been ended")
  );
}

function ActivePasteTargetPanel({
  activePasteTargetState,
}: Pick<TerminalViewProps, "activePasteTargetState">) {
  if (activePasteTargetState.kind === "ready") {
    return (
      <p className="paste-preview paste-target muted">
        Active paste target: {activePasteTargetState.adapterName} /{" "}
        {activePasteTargetState.processName}
      </p>
    );
  }

  if (activePasteTargetState.kind === "unsupported") {
    return (
      <p className="paste-preview paste-target warning">
        Active paste target unsupported: {activePasteTargetState.processName}
      </p>
    );
  }

  return <p className="paste-preview paste-target muted">{activePasteTargetState.message}</p>;
}

function PastePreviewPanel({ pasteState }: Pick<TerminalViewProps, "pasteState">) {
  if (pasteState.kind === "idle") {
    return <p className="paste-preview paste-route muted">{pasteState.message}</p>;
  }

  if (pasteState.kind === "ready") {
    return (
      <div className="paste-preview paste-route success">
        <span>
          Adapter {pasteState.adapterName} selected for {pasteState.processName}:
        </span>
        <code>{pasteState.text}</code>
      </div>
    );
  }

  if (pasteState.kind === "unsupported") {
    return (
      <p className="paste-preview paste-route warning">
        Image was extracted, but the active process is unsupported: {pasteState.processName}.{" "}
        {pasteState.path}
      </p>
    );
  }

  return <p className="paste-preview paste-route warning">{pasteState.message}</p>;
}

function TerminalSettingsPanel({
  onChange,
  onClose,
  settings,
}: {
  onChange: (settings: TerminalSettings) => void;
  onClose: () => void;
  settings: TerminalSettings;
}) {
  return (
    <aside className="terminal-settings-panel" aria-label="Terminal settings">
      <div className="settings-panel-header">
        <strong>Terminal settings</strong>
        <button type="button" onClick={onClose}>
          Close
        </button>
      </div>
      <label>
        Text
        <input
          type="color"
          value={settings.foreground}
          onChange={(event) => onChange({ ...settings, foreground: event.currentTarget.value })}
        />
      </label>
      <label>
        Background
        <input
          type="color"
          value={settings.background}
          onChange={(event) => onChange({ ...settings, background: event.currentTarget.value })}
        />
      </label>
      <label>
        Font size
        <input
          max="22"
          min="10"
          type="number"
          value={settings.fontSize}
          onChange={(event) =>
            onChange({
              ...settings,
              fontSize: Number(event.currentTarget.value),
            })
          }
        />
      </label>
    </aside>
  );
}
