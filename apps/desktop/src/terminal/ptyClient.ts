import { invoke } from "@tauri-apps/api/core";

export const PTY_OUTPUT_EVENT = "pty-output";
export const PTY_EXIT_EVENT = "pty-exit";

export type PtyLaunchCommand = {
  program: string;
  args?: string[];
};

// The backend `pty-output` event carries the emitting session's monotonic id
// alongside the decoded bytes, so a `TerminalView` can demultiplex output
// across concurrent sessions (mirroring the `pty-exit` id payload).
export type PtyOutputPayload = {
  sessionId: number;
  data: string;
};

export function isPtyOutputPayload(payload: unknown): payload is PtyOutputPayload {
  return (
    typeof payload === "object" &&
    payload !== null &&
    typeof (payload as { sessionId?: unknown }).sessionId === "number" &&
    typeof (payload as { data?: unknown }).data === "string"
  );
}

// The backend `pty-exit` event carries the exiting session's monotonic id
// (a number). The frontend compares it against the current session id to
// ignore stale exits from already-superseded sessions.
export function isPtyExitPayload(payload: unknown): payload is number {
  return typeof payload === "number";
}

// Resolves to the newly spawned session's monotonic id, which the caller
// records so it can match later `pty-exit` events against the live session.
export function spawnPty({
  cols,
  rows,
  command,
}: {
  cols: number;
  rows: number;
  command?: PtyLaunchCommand;
}) {
  return invoke<number>("pty_spawn", {
    cols,
    rows,
    program: command?.program,
    args: command?.args,
  });
}

export function writePty(data: string, sessionId: number) {
  return invoke<void>("pty_write", {
    data,
    sessionId,
  });
}

export function interruptPty(sessionId: number) {
  return invoke<void>("pty_interrupt", {
    sessionId,
  });
}

export function resizePty(size: { cols: number; rows: number }, sessionId: number) {
  return invoke<void>("pty_resize", {
    cols: size.cols,
    rows: size.rows,
    sessionId,
  });
}

export function killPty(sessionId: number) {
  return invoke<void>("pty_kill", {
    sessionId,
  });
}
