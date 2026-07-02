import { invoke } from "@tauri-apps/api/core";

export const PTY_OUTPUT_EVENT = "pty-output";
export const PTY_EXIT_EVENT = "pty-exit";

export type PtyLaunchCommand = {
  program: string;
  args?: string[];
};

export function isPtyOutputPayload(payload: unknown): payload is string {
  return typeof payload === "string";
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

export function writePty(data: string) {
  return invoke<void>("pty_write", {
    data,
  });
}

export function interruptPty() {
  return invoke<void>("pty_interrupt");
}

export function resizePty(size: { cols: number; rows: number }) {
  return invoke<void>("pty_resize", {
    cols: size.cols,
    rows: size.rows,
  });
}

export function killPty() {
  return invoke<void>("pty_kill");
}
