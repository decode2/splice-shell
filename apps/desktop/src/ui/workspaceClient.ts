import { invoke } from "@tauri-apps/api/core";

declare const workspaceIdBrand: unique symbol;
declare const sessionIdBrand: unique symbol;
declare const workspaceTabIdBrand: unique symbol;

export type WorkspaceId = string & { readonly [workspaceIdBrand]: "WorkspaceId" };
export type SessionId = number & { readonly [sessionIdBrand]: "SessionId" };
export type WorkspaceTabId = string & { readonly [workspaceTabIdBrand]: "WorkspaceTabId" };

export type WorkspaceProfile = {
  id: WorkspaceId;
  name: string;
  working_directory: string;
  environment: { profile: string; variable_names: string[] };
  agent: { id: string; command: string };
  session_ids: SessionId[];
};

export type WorkspaceBinding = {
  workspaceId: WorkspaceId;
  tabId: WorkspaceTabId;
  sessionId: SessionId;
};

export type WorkspaceLifecycleError = {
  code: string;
  message: string;
  platform?: string;
  retryable: boolean;
};

export type WorkspaceCapability = "unknown" | "available" | "unavailable";
export type WorkspaceUiState = {
  status: "idle" | "loading" | "ready" | "error";
  capability: WorkspaceCapability;
  bindings: WorkspaceBinding[];
  error?: WorkspaceLifecycleError;
};

export type WorkspaceUiAction =
  | { type: "loading" }
  | { type: "ready"; bindings: WorkspaceBinding[] }
  | { type: "failed"; error: WorkspaceLifecycleError }
  | { type: "unavailable"; error: WorkspaceLifecycleError };

type WorkspaceInvoke = (command: string, args?: Record<string, unknown>) => Promise<unknown>;

const invokeWorkspace: WorkspaceInvoke = (command, args) => invoke(command, args);

export function asWorkspaceId(value: string): WorkspaceId {
  if (!/^[a-zA-Z0-9_-]{1,64}$/.test(value)) {
    throw new Error("Workspace ID must contain only letters, numbers, underscores, or hyphens.");
  }
  return value as WorkspaceId;
}

export function asSessionId(value: number): SessionId {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error("Session ID must be a positive integer.");
  }
  return value as SessionId;
}

export function asWorkspaceTabId(value: string): WorkspaceTabId {
  if (value.length === 0 || value.length > 64) {
    throw new Error("Tab ID must be between 1 and 64 characters.");
  }
  return value as WorkspaceTabId;
}

export function createInitialWorkspaceUiState(): WorkspaceUiState {
  return { status: "idle", capability: "unknown", bindings: [] };
}

export function workspaceUiReducer(
  state: WorkspaceUiState,
  action: WorkspaceUiAction,
): WorkspaceUiState {
  switch (action.type) {
    case "loading":
      return { ...state, status: "loading", error: undefined };
    case "ready":
      return { status: "ready", capability: "available", bindings: action.bindings };
    case "failed":
      return { ...state, status: "error", error: action.error };
    case "unavailable":
      return { ...state, status: "error", capability: "unavailable", error: action.error };
  }
}

export function createWorkspaceClient(invokeCommand: WorkspaceInvoke = invokeWorkspace) {
  const request = <Result>(command: string, args?: Record<string, unknown>) =>
    (args === undefined ? invokeCommand(command) : invokeCommand(command, args)) as Promise<Result>;

  return {
    list: () => request<WorkspaceProfile[]>("workspace_list"),
    create: (profile: WorkspaceProfile, tabId: WorkspaceTabId) =>
      request<WorkspaceBinding>("workspace_create", { profile, tabId }),
    select: (workspaceId: WorkspaceId) => request<void>("workspace_select", { workspaceId }),
    close: (workspaceId: WorkspaceId) => request<void>("workspace_close", { workspaceId }),
    restart: (workspaceId: WorkspaceId) =>
      request<WorkspaceBinding>("workspace_restart", { workspaceId }),
    recover: () => request<WorkspaceBinding[]>("workspace_recover"),
  };
}
