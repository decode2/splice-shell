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
  it("negotiates one v1 activation and propagates its token before create and recover", async () => {
    const invoke = vi.fn(async (command: string) => {
      if (command === "workspace_protocol_negotiate") return { bootId: "boot-1", selected: 1, limits: { perRouteBytes: 1048576, routeCount: 32, totalBytes: 33554432 }, commands: ["workspace_create", "workspace_recover"] };
      if (command === "workspace_protocol_activate") return { activationId: "activation-1" };
      if (command === "workspace_create") return binding;
      if (command === "workspace_recover") return [binding];
      return undefined;
    });
    const client = createWorkspaceClient(invoke);
    const recoveringClient = createWorkspaceClient(invoke);

    await expect(Promise.all([client.create(profile, tabId), recoveringClient.recover()])).resolves.toEqual([binding, [binding]]);

    expect(invoke.mock.calls).toEqual([
      ["workspace_protocol_negotiate", { outputAdoption: [1] }],
      ["workspace_protocol_activate", { bootId: "boot-1", version: 1, consumerInstanceId: "workspace-ui-v1" }],
      ["workspace_create", { profile, tabId, protocol: { version: 1, activationId: "activation-1" } }],
      ["workspace_recover", { protocol: { version: 1, activationId: "activation-1" } }],
    ]);
  });

  it("fails closed without creating or recovering when negotiation is unsupported", async () => {
    const invoke = vi.fn(async (command: string) => {
      if (command === "workspace_protocol_negotiate") {
        throw new Error("command not found");
      }
      return command === "workspace_create" ? binding : [binding];
    });
    const client = createWorkspaceClient(invoke);

    await expect(client.create(profile, tabId)).rejects.toMatchObject({ code: "output-adoption-unsupported", retryable: false });
    await expect(client.recover()).rejects.toMatchObject({ code: "output-adoption-unsupported" });
    expect(invoke).not.toHaveBeenCalledWith("workspace_create", expect.anything());
    expect(invoke).not.toHaveBeenCalledWith("workspace_recover", expect.anything());
  });

  it("requires exact negotiated limits and commands before activation", async () => {
    const exactLimits = { perRouteBytes: 1048576, routeCount: 32, totalBytes: 33554432 };
    const exactCommands = ["workspace_create", "workspace_recover"];
    for (const [limits, commands] of [[undefined, exactCommands], [{}, exactCommands], [{ ...exactLimits, perRouteBytes: 1 }, exactCommands], [{ ...exactLimits, routeCount: 1 }, exactCommands], [{ ...exactLimits, totalBytes: 1 }, exactCommands], [exactLimits, undefined], [exactLimits, []], [exactLimits, ["workspace_create"]], [exactLimits, ["workspace_recover"]]]) {
      const invoke = vi.fn(async (command: string) => command === "workspace_protocol_negotiate" ? { bootId: "boot-invalid", selected: 1, limits, commands } : binding);
      const client = createWorkspaceClient(invoke);

      await expect(Promise.all([client.create(profile, tabId), client.recover()])).rejects.toMatchObject({ code: "output-adoption-unsupported" });
      expect(invoke.mock.calls).toEqual([["workspace_protocol_negotiate", { outputAdoption: [1] }]]);
    }
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
