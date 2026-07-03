import { invoke } from "@tauri-apps/api/core";

export const ACTIVE_PASTE_TARGET_COMMAND = "active_paste_target";

export type ActivePasteTarget = {
  processName: string;
  adapterName?: string | null;
  supported: boolean;
};

export type ActivePasteTargetState =
  | {
      kind: "loading";
      message: string;
    }
  | {
      kind: "ready";
      processName: string;
      adapterName: string;
    }
  | {
      kind: "unsupported";
      processName: string;
    }
  | {
      kind: "error";
      message: string;
    };

export function activePasteTargetToState(target: ActivePasteTarget): ActivePasteTargetState {
  if (target.supported && target.adapterName) {
    return {
      kind: "ready",
      processName: target.processName,
      adapterName: target.adapterName,
    };
  }

  return {
    kind: "unsupported",
    processName: target.processName,
  };
}

// `sessionId` is optional: at mount (before any session exists) and for the
// cmd.exe fallback the frontend omits it, and the backend resolves the default
// paste target rather than erroring (Paste-Target Fallback Parity).
export async function getActivePasteTarget(sessionId?: number) {
  return invoke<ActivePasteTarget>(ACTIVE_PASTE_TARGET_COMMAND, { sessionId });
}
