import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import {
  asWorkspaceId,
  asWorkspaceTabId,
  createInitialWorkspaceUiState,
  createWorkspaceClient,
  workspaceUiReducer,
  type WorkspaceBinding,
  type WorkspaceClient,
  type WorkspaceLifecycleError,
  type WorkspaceProfile,
} from "./workspaceClient";

type WorkspaceSwitcherProps = {
  client?: WorkspaceClient;
};

type CreateFields = {
  name: string;
  workingDirectory: string;
  agentCommand: string;
};

const INITIAL_CREATE_FIELDS: CreateFields = {
  name: "",
  workingDirectory: "",
  agentCommand: "",
};

function workspaceError(error: unknown): WorkspaceLifecycleError {
  if (typeof error === "object" && error !== null && "code" in error && "message" in error) {
    const value = error as Partial<WorkspaceLifecycleError>;
    return {
      code: String(value.code),
      message: String(value.message),
      platform: value.platform,
      retryable: value.retryable === true,
    };
  }
  return { code: "workspace-request-failed", message: String(error), retryable: true };
}

function workspaceIdFromName(name: string) {
  return asWorkspaceId(name.trim().toLowerCase().replace(/[^a-z0-9_-]+/g, "-"));
}

function agentIdFromCommand(command: string) {
  return `agent-${command.trim().toLowerCase().replace(/[^a-z0-9_-]+/g, "-").slice(0, 58)}`;
}

export function WorkspaceSwitcher({ client: providedClient }: WorkspaceSwitcherProps) {
  const defaultClient = useMemo(() => createWorkspaceClient(), []);
  const client = providedClient ?? defaultClient;
  const [state, dispatch] = useReducer(workspaceUiReducer, undefined, createInitialWorkspaceUiState);
  const [profiles, setProfiles] = useState<WorkspaceProfile[]>([]);
  const [selectedWorkspaceId, setSelectedWorkspaceId] = useState<string>();
  const [fields, setFields] = useState(INITIAL_CREATE_FIELDS);
  const workspaceTabSequence = useRef(0);
  const bindingsRef = useRef<WorkspaceBinding[]>([]);

  const setBindings = useCallback((bindings: WorkspaceBinding[]) => {
    bindingsRef.current = bindings;
    dispatch({ type: "ready", bindings });
  }, []);

  const load = useCallback(async () => {
    dispatch({ type: "loading" });
    try {
      const listedProfiles = await client.list();
      setProfiles(Array.isArray(listedProfiles) ? listedProfiles : []);
      setBindings(bindingsRef.current);
    } catch (error) {
      const lifecycleError = workspaceError(error);
      dispatch({
        type: lifecycleError.code === "permission-denied" ? "unavailable" : "failed",
        error: lifecycleError,
      });
    }
  }, [client, setBindings]);

  useEffect(() => {
    void load();
  }, [load]);

  const select = async (workspaceId: WorkspaceProfile["id"]) => {
    try {
      await client.select(workspaceId);
      setSelectedWorkspaceId(workspaceId);
    } catch (error) {
      dispatch({ type: "failed", error: workspaceError(error) });
    }
  };

  const create = async () => {
    const workspaceId = workspaceIdFromName(fields.name);
    const profile: WorkspaceProfile = {
      id: workspaceId,
      name: fields.name.trim(),
      working_directory: fields.workingDirectory.trim(),
      environment: { profile: "default", variable_names: [] },
      agent: { id: agentIdFromCommand(fields.agentCommand), command: fields.agentCommand.trim() },
      session_ids: [],
    };
    const tabId = asWorkspaceTabId(`workspace-tab-${workspaceTabSequence.current++}`);
    try {
      const binding = await client.create(profile, tabId);
      setProfiles((current) => [...current, profile]);
      setSelectedWorkspaceId(profile.id);
      setBindings([...bindingsRef.current, binding]);
      setFields(INITIAL_CREATE_FIELDS);
    } catch (error) {
      dispatch({ type: "failed", error: workspaceError(error) });
    }
  };

  const restart = async (workspaceId: WorkspaceProfile["id"]) => {
    try {
      const binding = await client.restart(workspaceId);
      setBindings([
        ...bindingsRef.current.filter((current) => current.workspaceId !== workspaceId),
        binding,
      ]);
    } catch (error) {
      dispatch({ type: "failed", error: workspaceError(error) });
    }
  };

  const close = async (workspaceId: WorkspaceProfile["id"]) => {
    try {
      await client.close(workspaceId);
      setProfiles((current) => current.filter((profile) => profile.id !== workspaceId));
      setSelectedWorkspaceId((current) => (current === workspaceId ? undefined : current));
      setBindings(bindingsRef.current.filter((binding) => binding.workspaceId !== workspaceId));
    } catch (error) {
      dispatch({ type: "failed", error: workspaceError(error) });
    }
  };

  const recover = async () => {
    try {
      setBindings(await client.recover());
    } catch (error) {
      dispatch({ type: "failed", error: workspaceError(error) });
    }
  };

  const activeBinding = state.bindings.find((binding) => binding.workspaceId === selectedWorkspaceId);
  const canCreate = Object.values(fields).every((value) => value.trim().length > 0);

  return (
    <section aria-label="Workspaces">
      <h2>Workspaces</h2>
      {state.status === "loading" ? <p>Loading workspaces…</p> : null}
      {state.error ? (
        <p role="alert">
          {state.error.message}
          {state.capability === "unavailable" ? (
            <button type="button" onClick={() => void load()}>
              Retry workspace loading
            </button>
          ) : null}
        </p>
      ) : null}
      <ul>
        {profiles.map((profile) => (
          <li key={profile.id}>
            <button
              type="button"
              aria-current={selectedWorkspaceId === profile.id || undefined}
              onClick={() => void select(profile.id)}
            >
              {profile.name}
            </button>
            <button type="button" onClick={() => void restart(profile.id)}>
              Restart {profile.name}
            </button>
            <button type="button" onClick={() => void close(profile.id)}>
              Close {profile.name}
            </button>
          </li>
        ))}
      </ul>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void create();
        }}
      >
        <label>
          Workspace name
          <input
            value={fields.name}
            onChange={(event) => setFields((current) => ({ ...current, name: event.target.value }))}
          />
        </label>
        <label>
          Working directory
          <input
            value={fields.workingDirectory}
            onChange={(event) =>
              setFields((current) => ({ ...current, workingDirectory: event.target.value }))
            }
          />
        </label>
        <label>
          Agent command
          <input
            value={fields.agentCommand}
            onChange={(event) =>
              setFields((current) => ({ ...current, agentCommand: event.target.value }))
            }
          />
        </label>
        <button type="submit" disabled={!canCreate}>
          Create workspace
        </button>
      </form>
      <button type="button" onClick={() => void recover()}>
        Recover workspaces
      </button>
      {activeBinding ? <p>Session {activeBinding.sessionId} is ready to attach.</p> : null}
    </section>
  );
}
