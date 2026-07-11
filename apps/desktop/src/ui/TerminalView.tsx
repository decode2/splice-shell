import { useEffect, useRef } from "react";
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
  ackPty,
  interruptPty,
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
  const terminalElementRef = useRef<HTMLDivElement | null>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  // Settings are global: they arrive as a prop from App (defaulting to the
  // shared module constant). No local state means no per-tab divergence.
  const effectiveSettings = settings;
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

    terminal.options.fontSize = effectiveSettings.fontSize;
    terminal.options.theme = {
      background: effectiveSettings.background,
      foreground: effectiveSettings.foreground,
      cursor: "#38bdf8",
      selectionBackground: "#1e3a8a",
    };

    // Changing the font size changes the cell dimensions, so the grid must be
    // refit or the row/col count goes stale — leaving a growing unpainted band
    // and a ConPTY size mismatch. fit() flows through terminal.onResize ->
    // the bridge's onResize -> resizePty, keeping the backend in sync. With
    // global settings this now fires in EVERY mounted instance, so one
    // font-size change refits all tabs (hidden ones included).
    fitAddonRef.current?.fit();
  }, [effectiveSettings]);

  // Activation insurance. When this instance becomes the foreground tab, refit
  // (it may have been hidden while the window resized) and route focus to it.
  // fit() is a cheap no-op when cols/rows are unchanged; focus is required for
  // input routing regardless.
  useEffect(() => {
    if (!active) {
      return;
    }
    fitAddonRef.current?.fit();
    terminalRef.current?.focus();
  }, [active]);

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
      fontSize: effectiveSettings.fontSize,
      theme: {
        background: effectiveSettings.background,
        foreground: effectiveSettings.foreground,
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
    // Tauri's `pty-output`/`pty-exit` events are GLOBAL: with N mounted
    // TerminalViews, every instance's listeners receive EVERY session's events.
    // `spawnInFlight` scopes this instance's willingness to retain
    // non-matching events to its own spawn round-trip ONLY. While false, any
    // event whose id !== currentSessionId is dropped outright, so a sibling
    // tab's stream can never accumulate here. Set true synchronously right
    // before `spawnPty` is awaited and cleared on EVERY settle path.
    let spawnInFlight = false;
    // Ordered FIFO queue of session-attributed output chunks that arrived
    // BEFORE their session id was recorded. The reader thread can emit
    // `pty-output` before `spawnPty` resolves over IPC (an instant-exit shell's
    // banner is SEVERAL chunks), so a single last-wins slot would drop all but
    // the last chunk. Chunks are enqueued in arrival order and drained
    // synchronously the moment `startPty` records `currentSessionId` (see
    // `flushEarlyOutputQueue`). Inherently bounded: it only grows during one
    // spawn IPC round-trip, is cleared on record, and is dropped on unmount.
    const earlyOutputQueue: { sessionId: number; bytes: number; data: string }[] = [];
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
    // leaving a dead terminal. Stash such ids here so `startPty` can act on the
    // one matching its own resolved id the moment it records it.
    //
    // A SET, not a single slot: under N>1, a foreign tab's insta-exit can land
    // between this instance's own stash and its `spawnPty` resolving. A single
    // slot would be clobbered by that foreign id, so this instance's own
    // insta-exit would be missed and its dead terminal reported healthy. The
    // set is clobber-proof and stays bounded — it only fills while
    // `spawnInFlight` is true and is cleared on every settle.
    const unmatchedExitIds = new Set<number>();
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

    // `terminal.write`'s callback fires once xterm's parser has actually
    // consumed the chunk. That — not the moment we hand it over — is when the
    // bytes are genuinely gone from the pipeline, so that is when the scheduler
    // is allowed to ack them back to Rust.
    const writeTerminalChunk = (chunk: string, consumed: () => void) => {
      terminal.write(chunk, consumed);
    };
    // The session that produced the bytes currently in the scheduler. Tracked
    // separately from `currentSessionId` because xterm's write callback is
    // asynchronous: it can land AFTER a restart has already advanced
    // `currentSessionId`, and crediting a fresh session for a dead one's bytes
    // would be wrong. Acking the dead id instead is a harmless backend no-op.
    let ackSessionId: number | undefined;
    const outputScheduler = createTerminalOutputScheduler({
      write: writeTerminalChunk,
      // Returns credit to the emitting session's window so its flusher can
      // resume. Fire-and-forget: an ack that races session teardown is expected
      // and is a no-op on an unknown id in the backend.
      ack: (bytes) => {
        const sessionId = ackSessionId;
        if (sessionId === undefined) {
          return;
        }

        void ackPty(sessionId, bytes).catch(() => {
          // A failed ack must never surface as terminal output: the session is
          // gone, and its credit window went with it.
        });
      },
    });
    // `bytes` is the credit cost of the PTY payload these actions were derived
    // from. It is charged ONCE per payload (to the first emitted action), never
    // per action: the filter can split, merge or swallow actions, but flow
    // control accounts for bytes RECEIVED, not bytes rendered.
    const writeTerminalActions = (actions: TerminalOutputAction[], bytes = 0) => {
      const coalesced = coalesceTerminalOutputActions(actions);
      if (coalesced.length === 0) {
        // The filter held the whole payload back (a partial escape sequence).
        // Those bytes were still received and still charged, so they must still
        // be acked — otherwise the window bleeds away and the session stalls.
        outputScheduler.write("", bytes);
        return;
      }

      coalesced.forEach((action, index) => {
        outputScheduler.write(action.data, index === 0 ? bytes : 0);
      });
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
    const writePtyOutput = (chunk: string, bytes: number) => {
      // Bind the credit these bytes owe to the session that actually produced
      // them, before they enter the scheduler.
      ackSessionId = currentSessionId;

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

      writeTerminalActions(outputFilter.write(chunk), bytes);
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
          writePtyOutput(chunk.data, chunk.bytes);
        }
      }
      earlyOutputQueue.length = 0;
    };
    // Empty this instance's spawn-window scratch state. Invariant: whenever
    // `spawnInFlight` is false, the early-output queue and the unmatched-exit
    // set are both empty. Called on EVERY settle path (resolve-live,
    // resolve-disposed, reject) so no cross-session event is ever retained
    // outside an active spawn window.
    const closeSpawnWindow = () => {
      spawnInFlight = false;
      earlyOutputQueue.length = 0;
      unmatchedExitIds.clear();
    };
    const startPty = async () => {
      // Open the spawn window synchronously BEFORE the await, so any
      // early-output / instant-exit for this pending session is retained (and
      // any foreign event during the window is bounded by its duration).
      spawnInFlight = true;
      let spawnedSessionId: number;
      try {
        spawnedSessionId = await spawnPty({ cols: terminal.cols, rows: terminal.rows });
      } catch (error) {
        // Reject settle: close the window (flag false, queue + set emptied) so a
        // doomed spawn leaks nothing into the next session, then rethrow for the
        // caller's catch (restartPty / the mount-effect starter) to surface.
        closeSpawnWindow();
        throw error;
      }
      // The view was torn down while this spawn was in flight. Removing
      // `pty_spawn`'s predecessor reaping means nothing else will reap this
      // now-orphaned session, so kill it explicitly by id and record no state.
      if (disposed) {
        void killPty(spawnedSessionId);
        closeSpawnWindow();
        return;
      }
      currentSessionId = spawnedSessionId;
      spawnTime = Date.now();
      ptyGeneration += 1;
      inputClosed = false;
      // A throw from the ready callback or the flush write-pipeline would skip
      // the settle-path closeSpawnWindow() below, stranding spawnInFlight=true
      // so every sibling tab's output/exit would accumulate unbounded — the
      // exact leak this window guards against. Close the window and rethrow so
      // the caller's catch surfaces it, mirroring the reject-settle path above.
      try {
        handlersRef.current.onPtyReady?.(spawnedSessionId);

        // Drain any output that raced ahead of this spawn resolving. This writes
        // only chunks for `spawnedSessionId`, drops the rest, and clears the
        // queue. It runs BEFORE the stashed-exit check below so an instant-exit
        // shell's banner (queued here) still prints.
        flushEarlyOutputQueue(spawnedSessionId);
      } catch (error) {
        closeSpawnWindow();
        throw error;
      }

      // A `pty-exit` for this exact session may have raced ahead of `spawnPty`
      // resolving (instant-exit child) and been stashed by `handlePtyExit`.
      // `has(spawnedSessionId)` — not equality against a single slot — is what
      // makes this immune to a foreign tab's exit clobbering the stash. Now that
      // the id is recorded, treat it as an immediate exit and restart. This
      // shares the restart path, so the storm cap still bounds an instant-exit
      // shell instead of letting it loop forever.
      if (unmatchedExitIds.has(spawnedSessionId)) {
        closeSpawnWindow();
        inputClosed = true;
        void restartPty();
        return;
      }

      // The session spawned and was not pre-empted by a stashed instant-exit:
      // close the window and treat the settle as proof it booted so the title
      // bar dot returns to quiet. Genuinely unhealthy sessions die and re-enter
      // reconnecting via `restartPty` before this matters.
      closeSpawnWindow();
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
        // Drain the scheduler too, while `ackSessionId` still points at the
        // DYING session. Anything left pending here belongs to it, not to the
        // session about to be spawned, and flushing now keeps the credit ledger
        // attributed to the right window.
        outputScheduler.flush();
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

    // Shared failure path for every id-scoped input call (write and interrupt).
    // A closed-PTY error on a superseded generation is stale and dropped; on the
    // live generation it closes input once and triggers exactly one restart.
    const handleInputFailure = (error: unknown, inputGeneration: number) => {
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
        handleInputFailure(error, inputGeneration);
      });
    };

    // Ctrl+C must go through `pty_interrupt`, not a raw \x03 write: the backend
    // both writes the C0 byte AND raises a native CTRL_C_EVENT on the console
    // group, which is what actually interrupts a Windows console child. Errors
    // follow the same generation guard / closed-input recovery as `writeInput`.
    const sendTerminalInterrupt = () => {
      if (inputClosed) {
        return;
      }

      const inputGeneration = ptyGeneration;
      handlersRef.current.onInputActivity?.();
      void Promise.resolve(interruptPty(currentSessionId ?? 0)).catch((error) => {
        handleInputFailure(error, inputGeneration);
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

      // Above current, or none recorded yet. Only stash it while THIS instance
      // is spawning — an unrecorded id could be this instance's own instant-exit
      // racing its spawn. While not spawning, such an exit belongs to a foreign
      // tab and is DROPPED (never stashed), so a sibling's exit can never mark
      // this instance dead nor clobber its stash.
      if (spawnInFlight) {
        unmatchedExitIds.add(payload);
      }
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
        sendTerminalInterrupt();
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

        const { sessionId, bytes, data } = event.payload;
        // Demultiplex by the live session id:
        //   - equal to current   → this view's output; write it.
        //   - otherwise, and this instance is SPAWNING → it may be this
        //     instance's own session racing ahead of `spawnPty` resolving;
        //     queue it FIFO for `startPty` to flush (matching) or drop
        //     (non-matching) on record. Bounded by the spawn window.
        //   - otherwise, and NOT spawning → a foreign tab's stream (or a stale
        //     superseded session); DROP it. Never enqueue, so a sibling's output
        //     can never accumulate here unbounded.
        if (sessionId === currentSessionId) {
          writePtyOutput(data, bytes);
          return;
        }

        if (spawnInFlight) {
          earlyOutputQueue.push({ sessionId, bytes, data });
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

function isClosedPtyInputError(error: unknown) {
  const message = error instanceof Error ? error.message : String(error);
  return (
    message.includes("PTY session closed") ||
    message.includes("PTY session is not running") ||
    message.includes("pipe is being closed") ||
    message.includes("pipe has been ended")
  );
}
