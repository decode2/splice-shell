import { invoke } from "@tauri-apps/api/core";

export const PTY_OUTPUT_EVENT = "pty-output";
export const PTY_EXIT_EVENT = "pty-exit";
// Carries a session's flow-control stall state (output backed up, waiting on the
// renderer). Distinct from `pty-exit`: the session is alive and correctly
// throttled, not dead — the frontend surfaces it as a "stalled" tab health.
export const PTY_STALL_EVENT = "pty-stall";

export type PtyLaunchCommand = {
  program: string;
  args?: string[];
};

// The backend `pty-output` event carries the emitting session's monotonic id
// alongside the decoded bytes, so a `TerminalView` can demultiplex output
// across concurrent sessions (mirroring the `pty-exit` id payload).
export type PtyOutputPayload = {
  sessionId: number;
  // UTF-8 byte cost the backend charged against this session's credit window
  // for this payload. Echo it back through `ackPty` once xterm has actually
  // consumed the data, or the window drains and the session stalls.
  //
  // Sent by the backend rather than recomputed here on purpose: `data.length`
  // counts UTF-16 code units, which diverges from Rust's UTF-8 byte accounting
  // on any non-ASCII output and would silently desynchronise the credit ledger.
  bytes: number;
  data: string;
};

export function isPtyOutputPayload(payload: unknown): payload is PtyOutputPayload {
  return (
    typeof payload === "object" &&
    payload !== null &&
    typeof (payload as { sessionId?: unknown }).sessionId === "number" &&
    typeof (payload as { bytes?: unknown }).bytes === "number" &&
    typeof (payload as { data?: unknown }).data === "string"
  );
}

// The backend `pty-exit` event carries the exiting session's monotonic id
// (a number). The frontend compares it against the current session id to
// ignore stale exits from already-superseded sessions.
export function isPtyExitPayload(payload: unknown): payload is number {
  return typeof payload === "number";
}

// The backend `pty-stall` event carries the session id whose output has backed
// up plus whether it is currently stalled (true) or has recovered (false). The
// frontend demultiplexes by session id, like `pty-output`/`pty-exit`.
export type PtyStallPayload = {
  sessionId: number;
  stalled: boolean;
};

export function isPtyStallPayload(payload: unknown): payload is PtyStallPayload {
  return (
    typeof payload === "object" &&
    payload !== null &&
    typeof (payload as { sessionId?: unknown }).sessionId === "number" &&
    typeof (payload as { stalled?: unknown }).stalled === "boolean"
  );
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

// Return `bytes` of consumed output to the session's credit window, letting its
// flusher emit again. This is the ACK half of the credit-based flow control:
// without it the backend can never learn whether the webview actually consumed
// what it emitted (Tauri's `emit` is fire-and-forget), and the real backlog just
// piles up invisibly in the webview's message queue.
//
// Safe to call for a session that has already been torn down: the backend treats
// an unknown id as a no-op, because an xterm write callback legitimately races
// `pty_kill`.
export function ackPty(sessionId: number, bytes: number) {
  return invoke<void>("pty_ack", {
    sessionId,
    bytes,
  });
}
