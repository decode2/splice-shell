import { describe, expect, it, vi } from "vitest";
import {
  asSessionId,
  asWorkspaceId,
  asWorkspaceTabId,
  createInitialWorkspaceUiState,
  createWorkspaceClient,
  workspaceUiReducer,
  type WorkspaceBinding,
  type WorkspaceProfile,
} from "./workspaceClient";

const workspaceId = asWorkspaceId("project_alpha");
const tabId = asWorkspaceTabId("tab-4");

const profile: WorkspaceProfile = {
  id: workspaceId,
  name: "Project Alpha",
  working_directory: "/projects/alpha",
  environment: { profile: "default", variable_names: ["PATH"] },
  agent: { id: "codex", command: "codex" },
  session_ids: [],
};

const binding: WorkspaceBinding = {
  workspaceId,
  tabId,
  sessionId: asSessionId(42),
};

describe("workspace client", () => {
  it("maps lifecycle requests to only the workspace Tauri commands", async () => {
    const invoke = vi.fn(async (command: string) => {
      if (command === "workspace_list") return [profile];
      if (command === "workspace_recover") return [binding];
      if (command === "workspace_create" || command === "workspace_restart") return binding;
      return undefined;
    });
    const client = createWorkspaceClient(invoke);

    await expect(client.list()).resolves.toEqual([profile]);
    await expect(client.create(profile, tabId)).resolves.toEqual(binding);
    await expect(client.select(workspaceId)).resolves.toBeUndefined();
    await expect(client.close(workspaceId)).resolves.toBeUndefined();
    await expect(client.restart(workspaceId)).resolves.toEqual(binding);
    await expect(client.recover()).resolves.toEqual([binding]);

    expect(invoke.mock.calls).toEqual([
      ["workspace_list"],
      ["workspace_create", { profile, tabId }],
      ["workspace_select", { workspaceId }],
      ["workspace_close", { workspaceId }],
      ["workspace_restart", { workspaceId }],
      ["workspace_recover"],
    ]);
    expect(invoke).not.toHaveBeenCalledWith("pty_spawn", expect.anything());
  });

  it("keeps workspace, session, and UI tab identities distinct and validates each boundary", () => {
    expect(asWorkspaceId("workspace-1")).toBe("workspace-1");
    expect(asWorkspaceTabId("tab-1")).toBe("tab-1");
    expect(asSessionId(7)).toBe(7);
    expect(() => asWorkspaceId("not a workspace")).toThrow("Workspace ID");
    expect(() => asWorkspaceTabId("")).toThrow("Tab ID");
    expect(() => asSessionId(0)).toThrow("Session ID");
  });

  it("records loading, capability, and structured error state for later UI without changing bindings", () => {
    const loading = workspaceUiReducer(createInitialWorkspaceUiState(), { type: "loading" });
    expect(loading).toMatchObject({ status: "loading", capability: "unknown", bindings: [] });

    const ready = workspaceUiReducer(loading, { type: "ready", bindings: [binding] });
    expect(ready).toEqual({ status: "ready", capability: "available", bindings: [binding] });

    const failed = workspaceUiReducer(ready, {
      type: "failed",
      error: { code: "workspace-store-failure", message: "Store unavailable", retryable: true },
    });
    expect(failed).toEqual({
      status: "error",
      capability: "available",
      bindings: [binding],
      error: { code: "workspace-store-failure", message: "Store unavailable", retryable: true },
    });

    const unavailable = workspaceUiReducer(loading, {
      type: "unavailable",
      error: { code: "permission-denied", message: "Workspace commands are unavailable", retryable: false },
    });
    expect(unavailable).toMatchObject({
      status: "error",
      capability: "unavailable",
      bindings: [],
      error: { code: "permission-denied", retryable: false },
    });
  });
});
