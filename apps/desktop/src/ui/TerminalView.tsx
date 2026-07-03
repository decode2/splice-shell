import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { createLocalFileLinkProvider } from "../terminal/fileLinks";
import { shouldRefreshTargetAfterInput } from "../terminal/inputActivity";
import { resolveTerminalKeyAction } from "../terminal/keyboardShortcuts";
import { readClipboardText, writeClipboardText } from "../terminal/clipboardClient";
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
import type { SessionHealth } from "./TitleBar";

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
  settingsOpen?: boolean;
  onCloseSettings?: () => void;
  onClipboardImagePaste?: () => void | Promise<void>;
  onInput?: (data: string) => void | Promise<void>;
  onInputActivity?: () => void;
  onSessionHealth?: (status: SessionHealth) => void;
  onTextPaste?: (text: string) => void | Promise<void>;
  onResize?: (size: { cols: number; rows: number }) => void;
  onPtyReady?: (sessionId: number) => void;
};

export function TerminalView({
  settingsOpen = false,
  onCloseSettings,
  onClipboardImagePaste,
  onInput,
  onInputActivity,
  onSessionHealth,
  onTextPaste,
  onPtyReady,
  onResize,
}: TerminalViewProps) {
  const terminalElementRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const [terminalSettings, setTerminalSettings] = useState(DEFAULT_TERMINAL_SETTINGS);
  const handlersRef = useRef({
    onClipboardImagePaste,
    onInput,
    onInputActivity,
    onSessionHealth,
    onTextPaste,
    onPtyReady,
    onResize,
  });

  handlersRef.current = {
    onClipboardImagePaste,
    onInput,
    onInputActivity,
    onSessionHealth,
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
    let inputClosed = false;
    let restartInFlight = false;
    let ptyGeneration = 0;
    let currentSessionId: number | undefined;
    // Ordered FIFO queue of session-attributed output chunks that arrived
    // BEFORE their session id was recorded. The reader thread can emit
    // `pty-output` before `spawnPty` resolves over IPC (an instant-exit shell's
    // banner is SEVERAL chunks), so a single last-wins slot would drop all but
    // the last chunk. Chunks are enqueued in arrival order and drained
    // synchronously the moment `startPty` records `currentSessionId` (see
    // `flushEarlyOutputQueue`). Inherently bounded: it only grows during one
    // spawn IPC round-trip, is cleared on record, and is dropped on unmount.
    const earlyOutputQueue: { sessionId: number; data: string }[] = [];
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

    // Session health drives the title bar's health dot. Emit only on real
    // transitions: a healthy session produces output on every frame, and the
    // storm guard can loop, so deduping here keeps the parent from being spammed
    // with identical statuses (App also no-ops an unchanged status defensively).
    let lastReportedHealth: SessionHealth | undefined;
    const reportSessionHealth = (status: SessionHealth) => {
      if (lastReportedHealth === status) {
        return;
      }
      lastReportedHealth = status;
      handlersRef.current.onSessionHealth?.(status);
    };

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
      // A session that has stayed up past the min-uptime threshold and is now
      // producing output is genuinely healthy, so clear the restart-storm
      // counter: a later, legitimate one-off restart should not be counted
      // against a burst of instant-exit failures from long ago. Output that
      // arrives within the first `MIN_HEALTHY_UPTIME_MS` is NOT trusted — a
      // shell that prints then instantly dies must keep accumulating restarts
      // toward the cap, otherwise the storm guard is defeated by any output.
      // The same proof-of-health drives the title bar dot back to "healthy".
      if (Date.now() - spawnTime > MIN_HEALTHY_UPTIME_MS) {
        consecutiveRestarts = 0;
        reportSessionHealth("healthy");
      }

      writeTerminalActions(outputFilter.write(chunk));
    };
    // Default input/resize writers thread the live `currentSessionId` into the
    // id-scoped Tauri commands. When the parent supplies its own handler (e.g.
    // paste routing), that wins; otherwise a `0` sentinel is used until a
    // session is recorded (the counter starts at 1, so `0` is a guaranteed
    // miss that the backend maps to today's "not running" error).
    const writeInput = (data: string) => {
      const handler = handlersRef.current.onInput;
      if (handler) {
        return handler(data);
      }
      return writePty(data, currentSessionId ?? 0);
    };
    const resizeTerminal = (size: { cols: number; rows: number }) => {
      const handler = handlersRef.current.onResize;
      if (handler) {
        return handler(size);
      }
      return resizePty(size, currentSessionId ?? 0);
    };
    // Drain the early-output queue the moment a session id is recorded. Write
    // the chunks that belong to `sessionId` in arrival order, drop chunks for
    // any other id (a superseded or never-recorded session), and clear the
    // queue. MUST be called synchronously (no `await`) right after recording
    // the id so no queued listener callback can interleave.
    const flushEarlyOutputQueue = (sessionId: number) => {
      for (const chunk of earlyOutputQueue) {
        if (chunk.sessionId === sessionId) {
          writePtyOutput(chunk.data);
        }
      }
      earlyOutputQueue.length = 0;
    };
    const startPty = async () => {
      const spawnedSessionId = await spawnPty({ cols: terminal.cols, rows: terminal.rows });
      // The view was torn down while this spawn was in flight. Removing
      // `pty_spawn`'s predecessor reaping means nothing else will reap this
      // now-orphaned session, so kill it explicitly by id and record no state.
      if (disposed) {
        void killPty(spawnedSessionId);
        return;
      }
      currentSessionId = spawnedSessionId;
      spawnTime = Date.now();
      ptyGeneration += 1;
      inputClosed = false;
      handlersRef.current.onPtyReady?.(spawnedSessionId);

      // Drain any output that raced ahead of this spawn resolving. This runs
      // BEFORE the stashed-exit early-return below so an instant-exit shell's
      // banner (queued here) still prints, matching today's single-session
      // behavior where the first bytes always render.
      flushEarlyOutputQueue(spawnedSessionId);

      // A `pty-exit` for this exact session may have raced ahead of `spawnPty`
      // resolving (instant-exit child) and been stashed by `handlePtyExit`.
      // Now that the id is recorded, treat it as an immediate exit and restart.
      // This shares the restart path, so the storm cap still bounds an
      // instant-exit shell instead of letting it loop forever.
      if (lastUnmatchedExitId === spawnedSessionId) {
        lastUnmatchedExitId = undefined;
        inputClosed = true;
        void restartPty();
        return;
      }

      // The session spawned and was not pre-empted by a stashed instant-exit:
      // treat the settle as proof it booted so the title bar dot returns to
      // quiet. Genuinely unhealthy sessions die and re-enter reconnecting via
      // `restartPty` before this matters.
      reportSessionHealth("healthy");
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
          // The storm guard gives up: surface a terminal "failed" state so the
          // title bar dot goes red instead of silently pretending to reconnect.
          reportSessionHealth("failed");
          terminal.write(
            "\r\nPTY session keeps exiting immediately; automatic restart stopped.\r\n",
          );
          return;
        }

        // A restart is under way: the dot pulses amber until the new session
        // proves healthy (output past the min uptime) or the cap gives up.
        reportSessionHealth("reconnecting");
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
      if (shouldRefreshTargetAfterInput(data)) {
        handlersRef.current.onInputActivity?.();
      }
      void Promise.resolve(writeInput(data)).catch((error) => {
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
        void Promise.resolve(resizeTerminal(size)).catch((error) => {
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
          handlersRef.current.onTextPaste?.(text) ?? writeInput(text),
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

      if (action === "paste") {
        // xterm maps Ctrl+V to the C0 byte \x16 and cancels the keydown, so the
        // DOM paste event never fires. Intercept here, read the OS clipboard
        // natively, and prefer text: terminal.paste() applies bracketed-paste
        // (DECSET 2004) wrapping + CRLF→CR normalization so a multi-line paste
        // into codex stays one block instead of auto-executing, then flows
        // through the existing onData → writePty + paste-target refresh path.
        // When there is no text, fall back to the existing image paste route.
        // Neither present → a clean no-op. Fire-and-forget with .catch so the
        // keydown handler can never throw.
        event.preventDefault();
        event.stopPropagation();
        void readClipboardText()
          .then((text) => {
            // The read is async: guard against the terminal being torn down
            // (StrictMode remount, app shutdown) before it resolves, so we
            // never inject input into a disposed xterm.
            if (disposed) {
              return undefined;
            }

            if (text) {
              terminal.paste(text);
              return undefined;
            }

            return handlersRef.current.onClipboardImagePaste?.();
          })
          .catch((error) => {
            if (disposed) {
              return;
            }

            terminal.write(`\r\nPaste failed: ${String(error)}\r\n`);
          });
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
        if (disposed || !isPtyOutputPayload(event.payload)) {
          return;
        }

        const { sessionId, data } = event.payload;
        // Demultiplex by the live session id, mirroring `pty-exit` filtering:
        //   - equal to current            → this view's output; write it.
        //   - above current, or none yet  → raced ahead of `spawnPty`
        //     resolving; queue it FIFO for `startPty` to flush on record.
        //   - below current               → stale, already-superseded; drop.
        if (sessionId === currentSessionId) {
          writePtyOutput(data);
          return;
        }

        if (currentSessionId === undefined || sessionId > currentSessionId) {
          earlyOutputQueue.push({ sessionId, data });
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
      // Only kill a session that was actually recorded. A spawn still in flight
      // at teardown is handled by `startPty`'s post-await disposed check, which
      // kills the orphan by its resolved id.
      if (currentSessionId !== undefined) {
        void killPty(currentSessionId);
      }
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
      {settingsOpen ? (
        <TerminalSettingsPanel
          settings={terminalSettings}
          onChange={setTerminalSettings}
          onClose={() => onCloseSettings?.()}
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
