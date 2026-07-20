// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  asSessionId,
  asWorkspaceId,
  asWorkspaceTabId,
  type WorkspaceBinding,
  type WorkspaceClient,
  type WorkspaceProfile,
} from "./workspaceClient";
import { WorkspaceSwitcher } from "./WorkspaceSwitcher";

const projectAlpha: WorkspaceProfile = {
  id: asWorkspaceId("project-alpha"),
  name: "Project Alpha",
  working_directory: "/projects/alpha",
  environment: { profile: "default", variable_names: [] },
  agent: { id: "generic-tui", command: "bash" },
  session_ids: [asSessionId(7)],
};

const recoveredBinding: WorkspaceBinding = {
  workspaceId: projectAlpha.id,
  tabId: asWorkspaceTabId("workspace-tab-4"),
  sessionId: asSessionId(9),
};

function createClient(overrides: Partial<WorkspaceClient> = {}): WorkspaceClient {
  return {
    list: vi.fn(async () => [projectAlpha]),
    create: vi.fn(async (profile: WorkspaceProfile, tabId) => ({
      workspaceId: profile.id,
      tabId,
      sessionId: asSessionId(9),
    })),
    select: vi.fn(async () => undefined),
    close: vi.fn(async () => undefined),
    restart: vi.fn(async () => recoveredBinding),
    recover: vi.fn(async () => [recoveredBinding]),
    ...overrides,
  };
}

afterEach(cleanup);

describe("WorkspaceSwitcher", () => {
  it("renders listed workspaces and selects the requested workspace without adopting its session", async () => {
    const client = createClient();
    render(<WorkspaceSwitcher client={client} />);

    const workspace = await screen.findByRole("button", { name: "Project Alpha" });
    fireEvent.click(workspace);

    await waitFor(() => expect(client.select).toHaveBeenCalledWith(projectAlpha.id));
    expect(workspace.getAttribute("aria-current")).toBe("true");
    expect(screen.queryByLabelText("Terminal")).toBeNull();
  });

  it("maps the entered command to a non-generic agent profile with a distinct UI tab id", async () => {
    const client = createClient();
    render(<WorkspaceSwitcher client={client} />);

    await screen.findByRole("button", { name: "Project Alpha" });
    fireEvent.change(screen.getByLabelText("Workspace name"), { target: { value: "New project" } });
    fireEvent.change(screen.getByLabelText("Working directory"), { target: { value: "/projects/new" } });
    fireEvent.change(screen.getByLabelText("Agent command"), { target: { value: "codex" } });
    fireEvent.click(screen.getByRole("button", { name: "Create workspace" }));

    await waitFor(() => expect(client.create).toHaveBeenCalledTimes(1));
    const [profile, tabId] = (client.create as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(profile).toMatchObject({
      id: "new-project",
      name: "New project",
      working_directory: "/projects/new",
      agent: { id: "agent-codex", command: "codex" },
    });
    expect(tabId).toMatch(/^workspace-tab-/);
    expect(tabId).not.toBe(profile.id);
    expect(await screen.findByText("Session 9 is ready to attach.")).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Restart Project Alpha" }));
    fireEvent.click(screen.getByRole("button", { name: "Close Project Alpha" }));
    fireEvent.click(screen.getByRole("button", { name: "Recover workspaces" }));

    await waitFor(() => {
      expect(client.restart).toHaveBeenCalledWith(projectAlpha.id);
      expect(client.close).toHaveBeenCalledWith(projectAlpha.id);
      expect(client.recover).toHaveBeenCalledTimes(1);
    });
  });

  it("shows an actionable unavailable state when workspace commands are denied", async () => {
    const client = createClient({
      list: vi.fn(async () => {
        throw { code: "permission-denied", message: "Workspace commands are unavailable", retryable: false };
      }),
    });
    render(<WorkspaceSwitcher client={client} />);

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Workspace commands are unavailable");
    expect(screen.getByRole("button", { name: "Retry workspace loading" })).toBeTruthy();
  });

  it("requires complete create fields before asking the backend to start a workspace", async () => {
    const client = createClient();
    render(<WorkspaceSwitcher client={client} />);

    const create = await screen.findByRole("button", { name: "Create workspace" });
    expect((create as HTMLButtonElement).disabled).toBe(true);

    fireEvent.change(screen.getByLabelText("Workspace name"), { target: { value: "New project" } });
    fireEvent.change(screen.getByLabelText("Working directory"), { target: { value: "/projects/new" } });
    fireEvent.change(screen.getByLabelText("Agent command"), { target: { value: "codex" } });

    expect((create as HTMLButtonElement).disabled).toBe(false);
  });
});
