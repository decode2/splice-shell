import { invoke } from "@tauri-apps/api/core";

export const PTY_OUTPUT_EVENT = "pty-output";

export type PtyLaunchCommand = {
  program: string;
  args?: string[];
};

export function isPtyOutputPayload(payload: unknown): payload is string {
  return typeof payload === "string";
}

export function spawnPty({
  cols,
  rows,
  command,
}: {
  cols: number;
  rows: number;
  command?: PtyLaunchCommand;
}) {
  return invoke<void>("pty_spawn", {
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

export function readPty() {
  return invoke<string[]>("pty_read");
}

export function killPty() {
  return invoke<void>("pty_kill");
}
