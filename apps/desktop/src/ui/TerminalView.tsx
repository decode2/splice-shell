import { useEffect, useRef, useState } from "react";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import type { ActivePasteTargetState } from "../paste/activePasteTarget";
import type { PastePreviewState } from "../paste/pastePreview";
import { createLocalFileLinkProvider } from "../terminal/fileLinks";
import { shouldRefreshTargetAfterInput } from "../terminal/inputActivity";
import { isTerminalInterruptShortcut } from "../terminal/keyboardShortcuts";
import {
  killPty,
  isPtyOutputPayload,
  PTY_OUTPUT_EVENT,
  readPty,
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
  }, [terminalSettings]);

  useEffect(() => {
    const terminalElement = terminalElementRef.current;
    if (!terminalElement) {
      return undefined;
    }

    const terminal = new Terminal({
      allowProposedApi: true,
      cursorBlink: true,
      fontFamily: '"Cascadia Code", "Fira Code", Consolas, monospace',
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
    terminal.loadAddon(fitAddon);
    const fileLinkProvider = terminal.registerLinkProvider(createLocalFileLinkProvider(terminal));
    let disposed = false;
    let outputSeen = false;
    let inputClosed = false;
    let restartInFlight = false;
    let ptyGeneration = 0;
    let unlistenPtyOutput: UnlistenFn | undefined;
    const outputFilter = createTerminalOutputFilter();

    const sleep = (delayMs: number) =>
      new Promise<void>((resolve) => {
        window.setTimeout(resolve, delayMs);
      });
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
    const writePtyOutput = (chunk: string) => {
      if (!outputSeen) {
        outputSeen = true;
        setHasPtyOutput(true);
      }

      writeTerminalActions(outputFilter.write(chunk));
    };
    const startPty = async () => {
      await spawnPty({ cols: terminal.cols, rows: terminal.rows });
      ptyGeneration += 1;
      inputClosed = false;
      handlersRef.current.onPtyReady?.();
    };
    const restartPty = async () => {
      if (restartInFlight || disposed) {
        return;
      }

      restartInFlight = true;
      try {
        terminal.write("\r\nPTY session ended. Starting a new shell...\r\n");
        await startPty();
        terminal.write("\r\nNew shell session started.\r\n");
      } catch (error) {
        terminal.write(`\r\nFailed to restart ConPTY session: ${String(error)}\r\n`);
      } finally {
        restartInFlight = false;
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
        void handlersRef.current.onResize(size);
      },
    });

    const monitorPtySession = async () => {
      while (!disposed) {
        try {
          await readPty();
          if (disposed) {
            break;
          }

          await sleep(500);
        } catch (error) {
          if (!disposed) {
            if (isClosedPtyInputError(error)) {
              inputClosed = true;
              await restartPty();
              continue;
            }

            terminal.write(`\r\nPTY output read failed: ${String(error)}\r\n`);
            await sleep(100);
          }
        }
      }
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
      if (!isTerminalInterruptShortcut(event)) {
        return;
      }

      event.preventDefault();
      event.stopPropagation();
      sendTerminalInput("\x03");
    };

    terminalElement.addEventListener("paste", handlePaste, { capture: true });
    terminalElement.addEventListener("keydown", handleTerminalKeyDown, { capture: true });

    void listen(PTY_OUTPUT_EVENT, (event) => {
      if (!disposed && isPtyOutputPayload(event.payload)) {
        writePtyOutput(event.payload);
      }
    })
      .then((unlisten) => {
        if (disposed) {
          unlisten();
          return;
        }

        unlistenPtyOutput = unlisten;
        void startPty()
          .then(() => {
            void monitorPtySession();
          })
          .catch((error) => {
            terminal.write(`\r\nFailed to start ConPTY session: ${String(error)}\r\n`);
          });
      })
      .catch((error) => {
        terminal.write(`\r\nFailed to subscribe to PTY output: ${String(error)}\r\n`);
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
      void killPty();
      fileLinkProvider.dispose();
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
