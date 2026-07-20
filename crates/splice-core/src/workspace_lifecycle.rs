use crate::{WorkspaceError, WorkspaceId, WorkspaceProfile, WorkspaceStore};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct SessionId(u64);

impl SessionId {
    pub fn new(value: u64) -> Result<Self, WorkspaceLifecycleError> {
        (value != 0)
            .then_some(Self(value))
            .ok_or(WorkspaceLifecycleError::InvalidSessionId)
    }

    pub fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TabId(String);

impl TabId {
    pub fn new(value: impl Into<String>) -> Result<Self, WorkspaceLifecycleError> {
        let value = value.into();
        (!value.is_empty() && value.len() <= 64)
            .then_some(Self(value))
            .ok_or(WorkspaceLifecycleError::InvalidTabId)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionLifecycleError {
    pub code: String,
    pub message: String,
    pub platform: Option<String>,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LifecycleError {
    pub code: String,
    pub message: String,
    pub platform: Option<String>,
    pub retryable: bool,
}

impl SessionLifecycleError {
    pub fn contract(&self) -> LifecycleError {
        LifecycleError {
            code: self.code.clone(),
            message: self.message.clone(),
            platform: self.platform.clone(),
            retryable: self.retryable,
        }
    }

    fn closes_convergently(&self) -> bool {
        matches!(self.code.as_str(), "already-closed" | "session-not-found")
    }
}

pub trait SessionLifecyclePort {
    fn start(&mut self, profile: &WorkspaceProfile) -> Result<SessionId, SessionLifecycleError>;
    fn close(&mut self, session: SessionId) -> Result<(), SessionLifecycleError>;
    fn runtime_id(&self) -> String;
}

#[derive(Debug)]
pub enum WorkspaceLifecycleError {
    InvalidSessionId,
    InvalidTabId,
    NotFound(WorkspaceId),
    Conflict(WorkspaceId),
    Store(WorkspaceError),
    Session(SessionLifecycleError),
    StartRollback {
        store: WorkspaceError,
        close: SessionLifecycleError,
    },
}

impl From<WorkspaceError> for WorkspaceLifecycleError {
    fn from(error: WorkspaceError) -> Self {
        Self::Store(error)
    }
}

impl WorkspaceLifecycleError {
    pub fn contract(&self) -> LifecycleError {
        match self {
            Self::InvalidSessionId => {
                contract("invalid-session-id", "Session ID must be non-zero.", false)
            }
            Self::InvalidTabId => contract("invalid-tab-id", "Tab ID is invalid.", false),
            Self::NotFound(_) => contract("workspace-not-found", "Workspace was not found.", false),
            Self::Conflict(_) => contract("workspace-conflict", "Workspace already exists.", false),
            Self::Store(error) => LifecycleError {
                code: "workspace-store-failure".to_owned(),
                message: "Workspace persistence failed.".to_owned(),
                platform: None,
                retryable: matches!(error, WorkspaceError::Io),
            },
            Self::Session(error) => error.contract(),
            Self::StartRollback { store, close } => LifecycleError {
                code: "workspace-start-rollback-failed".to_owned(),
                message: "Workspace persistence and session rollback failed.".to_owned(),
                platform: close.platform.clone(),
                retryable: matches!(store, WorkspaceError::Io) || close.retryable,
            },
        }
    }
}

fn contract(code: &str, message: &str, retryable: bool) -> LifecycleError {
    LifecycleError {
        code: code.to_owned(),
        message: message.to_owned(),
        platform: None,
        retryable,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceBinding {
    pub workspace_id: WorkspaceId,
    pub tab_id: TabId,
    pub session_id: SessionId,
}

#[derive(Clone)]
/// A definitively terminated PTY awaiting a successful persistence update.
///
/// This state is process-local: if the process exits before the store accepts
/// the update, reconstruction can recover only the durable lifecycle intent.
struct PendingTermination {
    session_id: SessionId,
    tab_id: TabId,
}

pub struct WorkspaceController<P> {
    store: WorkspaceStore,
    sessions: P,
    bindings: BTreeMap<WorkspaceId, WorkspaceBinding>,
    pending_terminations: BTreeMap<WorkspaceId, PendingTermination>,
    selected: Option<WorkspaceId>,
}

impl<P: SessionLifecyclePort> WorkspaceController<P> {
    pub fn new(store: WorkspaceStore, sessions: P) -> Self {
        Self {
            store,
            sessions,
            bindings: BTreeMap::new(),
            pending_terminations: BTreeMap::new(),
            selected: None,
        }
    }

    pub fn list(&self) -> Result<Vec<WorkspaceProfile>, WorkspaceLifecycleError> {
        Ok(self.store.list()?)
    }

    pub fn selected(&self) -> Option<&WorkspaceId> {
        self.selected.as_ref()
    }

    pub fn bindings(&self) -> Vec<WorkspaceBinding> {
        self.bindings.values().cloned().collect()
    }

    pub fn create(
        &mut self,
        mut profile: WorkspaceProfile,
        tab_id: TabId,
    ) -> Result<WorkspaceBinding, WorkspaceLifecycleError> {
        self.reconcile_pending_termination(&profile.id)?;
        if let Some(existing) = self.store.load(&profile.id)? {
            let existing_id = existing.id.clone();
            let retrying_live_binding = self
                .bindings
                .get(&existing.id)
                .is_some_and(|binding| binding.tab_id == tab_id);
            if existing.lifecycle_desired_open
                && existing.lifecycle_tab_id.as_deref() != Some(tab_id.as_str())
                && !retrying_live_binding
            {
                return Err(WorkspaceLifecycleError::Conflict(profile.id));
            }
            if !existing.lifecycle_desired_open {
                profile = existing;
                profile.lifecycle_desired_open = true;
                profile.lifecycle_tab_id = Some(tab_id.as_str().to_owned());
                clear_runtime_association(&mut profile);
                self.store.save(&profile)?;
            }
            return self.start(&existing_id, tab_id);
        }
        profile.session_ids.clear();
        profile.lifecycle_desired_open = true;
        profile.lifecycle_tab_id = Some(tab_id.as_str().to_owned());
        profile.lifecycle_runtime_id = None;
        profile.lifecycle_closing_session_id = None;
        profile.lifecycle_closing_runtime_id = None;
        self.store.save(&profile)?;
        let binding = self.start(&profile.id, tab_id)?;
        self.selected = Some(binding.workspace_id.clone());
        Ok(binding)
    }

    pub fn select(&mut self, id: &WorkspaceId) -> Result<(), WorkspaceLifecycleError> {
        self.reconcile_pending_termination(id)?;
        self.require_profile(id)?;
        self.selected = Some(id.clone());
        Ok(())
    }

    pub fn update(&mut self, mut profile: WorkspaceProfile) -> Result<(), WorkspaceLifecycleError> {
        self.reconcile_pending_termination(&profile.id)?;
        let current = self.require_profile(&profile.id)?;
        profile.session_ids = current.session_ids;
        profile.lifecycle_desired_open = current.lifecycle_desired_open;
        profile.lifecycle_tab_id = current.lifecycle_tab_id;
        profile.lifecycle_runtime_id = current.lifecycle_runtime_id;
        profile.lifecycle_closing_session_id = current.lifecycle_closing_session_id;
        profile.lifecycle_closing_runtime_id = current.lifecycle_closing_runtime_id;
        self.store.save(&profile)?;
        Ok(())
    }

    pub fn close(&mut self, id: &WorkspaceId) -> Result<(), WorkspaceLifecycleError> {
        self.reconcile_pending_termination(id)?;
        let mut profile = self.require_profile(id)?;
        let runtime_id = self.sessions.runtime_id();
        let binding_session = self.bindings.get(id).map(|binding| binding.session_id);
        let persisted_close = profile
            .lifecycle_closing_session_id
            .map(SessionId::new)
            .transpose()?;
        let closing_session = binding_session.or_else(|| {
            (profile.lifecycle_closing_runtime_id.as_deref() == Some(runtime_id.as_str()))
                .then_some(persisted_close)
                .flatten()
        });
        profile.lifecycle_desired_open = false;
        profile.lifecycle_tab_id = None;
        if let Some(session_id) = closing_session {
            if binding_session.is_some() {
                profile.session_ids = vec![session_id.get()];
                profile.lifecycle_runtime_id = Some(runtime_id.clone());
                profile.lifecycle_closing_session_id = Some(session_id.get());
                profile.lifecycle_closing_runtime_id = Some(runtime_id);
                self.store.save(&profile)?;
            } else if profile.lifecycle_closing_runtime_id.as_deref() != Some(runtime_id.as_str()) {
                clear_runtime_association(&mut profile);
                self.store.save(&profile)?;
                self.bindings.remove(id);
                return self.clear_selection(id);
            }
            if let Err(error) = self.sessions.close(session_id) {
                if !error.closes_convergently() {
                    return Err(WorkspaceLifecycleError::Session(error));
                }
            }
            clear_runtime_association(&mut profile);
            self.store.save(&profile)?;
            self.bindings.remove(id);
        } else {
            clear_runtime_association(&mut profile);
            self.store.save(&profile)?;
            self.bindings.remove(id);
        }
        self.clear_selection(id)
    }

    fn clear_selection(&mut self, id: &WorkspaceId) -> Result<(), WorkspaceLifecycleError> {
        if self.selected.as_ref() == Some(id) {
            self.selected = None;
        }
        Ok(())
    }

    pub fn restart(
        &mut self,
        id: &WorkspaceId,
    ) -> Result<WorkspaceBinding, WorkspaceLifecycleError> {
        self.reconcile_pending_termination(id)?;
        let profile = self.require_profile(id)?;
        let tab_id = self
            .bindings
            .get(id)
            .map(|binding| binding.tab_id.clone())
            .or_else(|| {
                profile
                    .lifecycle_tab_id
                    .as_deref()
                    .map(TabId::new)
                    .transpose()
                    .ok()
                    .flatten()
            })
            .ok_or_else(|| WorkspaceLifecycleError::NotFound(id.clone()))?;
        let selected = self.selected.as_ref() == Some(id);
        if self.bindings.contains_key(id) || profile.lifecycle_closing_session_id.is_some() {
            self.close(id)?;
        }
        let mut profile = self.require_profile(id)?;
        profile.lifecycle_desired_open = true;
        profile.lifecycle_tab_id = Some(tab_id.as_str().to_owned());
        self.store.save(&profile)?;
        let binding = self.start(id, tab_id)?;
        if selected {
            self.selected = Some(id.clone());
        }
        Ok(binding)
    }

    pub fn recover(&mut self) -> Result<Vec<WorkspaceBinding>, WorkspaceLifecycleError> {
        let mut first_error = None;
        let runtime_id = self.sessions.runtime_id();
        let profiles = self.store.list()?;
        let pending_ids = self
            .pending_terminations
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>();
        let mut cleaned_ids = BTreeSet::new();

        for profile in &profiles {
            if self.bindings.contains_key(&profile.id) || pending_ids.contains(&profile.id) {
                continue;
            }
            let actionable = profile.lifecycle_runtime_id.as_deref() == Some(runtime_id.as_str())
                || profile.lifecycle_closing_runtime_id.as_deref() == Some(runtime_id.as_str());
            if !actionable {
                continue;
            }
            let mut all_sessions_closed = true;
            for session in &profile.session_ids {
                let session = SessionId::new(*session)?;
                match self.sessions.close(session) {
                    Ok(()) => {}
                    Err(error) if error.closes_convergently() => {}
                    Err(error) => {
                        all_sessions_closed = false;
                        first_error.get_or_insert(WorkspaceLifecycleError::Session(error));
                    }
                }
            }
            if all_sessions_closed {
                cleaned_ids.insert(profile.id.clone());
            }
        }

        let blocked_ids = profiles
            .iter()
            .filter(|profile| {
                !self.bindings.contains_key(&profile.id)
                    && !pending_ids.contains(&profile.id)
                    && (profile.lifecycle_runtime_id.as_deref() == Some(runtime_id.as_str())
                        || profile.lifecycle_closing_runtime_id.as_deref()
                            == Some(runtime_id.as_str()))
                    && !cleaned_ids.contains(&profile.id)
                    && !profile.session_ids.is_empty()
            })
            .map(|profile| profile.id.clone())
            .collect::<BTreeSet<_>>();

        self.store.update_all(|profiles| {
            for profile in profiles.values_mut() {
                migrate_legacy_intent(profile);
                let stale_active = !profile.session_ids.is_empty()
                    && profile.lifecycle_runtime_id.as_deref() != Some(runtime_id.as_str());
                let stale_close = profile.lifecycle_closing_session_id.is_some()
                    && profile.lifecycle_closing_runtime_id.as_deref() != Some(runtime_id.as_str());
                if pending_ids.contains(&profile.id)
                    || cleaned_ids.contains(&profile.id)
                    || (!blocked_ids.contains(&profile.id) && (stale_active || stale_close))
                {
                    clear_runtime_association(profile);
                    if stale_close {
                        profile.lifecycle_desired_open = false;
                        profile.lifecycle_tab_id = None;
                    }
                }
            }
        })?;
        for id in pending_ids {
            self.pending_terminations.remove(&id);
        }

        for profile in self.store.list()? {
            if blocked_ids.contains(&profile.id)
                || self.bindings.contains_key(&profile.id)
                || !profile.lifecycle_desired_open
            {
                continue;
            }
            let result = profile
                .lifecycle_tab_id
                .as_deref()
                .map(TabId::new)
                .transpose()?
                .ok_or_else(|| WorkspaceLifecycleError::NotFound(profile.id.clone()))
                .and_then(|tab_id| self.start(&profile.id, tab_id).map(drop));
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or_else(|| Ok(self.bindings()), Err)
    }

    /// Converge controller state after a PTY ended outside the controller's
    /// explicit `close` path. The callback is id-scoped and idempotent: a stale
    /// exit notification cannot remove a newer binding for the same workspace.
    pub fn reconcile_terminated_session(
        &mut self,
        session_id: SessionId,
    ) -> Result<(), WorkspaceLifecycleError> {
        let Some(binding) = self
            .bindings
            .values()
            .find(|binding| binding.session_id == session_id)
            .cloned()
        else {
            let pending_workspace = self
                .pending_terminations
                .iter()
                .find(|(_, pending)| pending.session_id == session_id)
                .map(|(id, _)| id.clone());
            return pending_workspace.map_or(Ok(()), |id| self.reconcile_pending_termination(&id));
        };

        // PTY teardown is already definitive. Remove the binding before
        // persistence so no later lifecycle operation can return this dead ID.
        let workspace_id = binding.workspace_id.clone();
        self.bindings.remove(&workspace_id);
        self.pending_terminations.insert(
            workspace_id.clone(),
            PendingTermination {
                session_id,
                tab_id: binding.tab_id.clone(),
            },
        );
        self.reconcile_pending_termination(&workspace_id)
    }

    fn reconcile_pending_termination(
        &mut self,
        id: &WorkspaceId,
    ) -> Result<(), WorkspaceLifecycleError> {
        let Some(pending) = self.pending_terminations.get(id).cloned() else {
            return Ok(());
        };
        let mut profile = self.require_profile(id)?;
        clear_runtime_association(&mut profile);
        profile.lifecycle_desired_open = true;
        // Retain the tab identity as restart intent. A subsequent create or
        // restart then starts a new session rather than returning the dead id.
        profile.lifecycle_tab_id = Some(pending.tab_id.as_str().to_owned());
        self.store.save(&profile)?;
        self.pending_terminations.remove(id);
        Ok(())
    }

    fn start(
        &mut self,
        id: &WorkspaceId,
        tab_id: TabId,
    ) -> Result<WorkspaceBinding, WorkspaceLifecycleError> {
        self.reconcile_pending_termination(id)?;
        if let Some(binding) = self.bindings.get(id) {
            let mut profile = self.require_profile(id)?;
            if !profile.session_ids.contains(&binding.session_id.get()) {
                profile.session_ids = vec![binding.session_id.get()];
                profile.lifecycle_desired_open = false;
                profile.lifecycle_tab_id = None;
                profile.lifecycle_runtime_id = Some(self.sessions.runtime_id());
                profile.lifecycle_closing_session_id = Some(binding.session_id.get());
                profile.lifecycle_closing_runtime_id = profile.lifecycle_runtime_id.clone();
                self.store.save(&profile)?;
            }
            return Ok(binding.clone());
        }
        let mut profile = self.require_profile(id)?;
        let session_id = self
            .sessions
            .start(&profile)
            .map_err(WorkspaceLifecycleError::Session)?;
        profile.session_ids = vec![session_id.get()];
        profile.lifecycle_desired_open = true;
        profile.lifecycle_tab_id = Some(tab_id.as_str().to_owned());
        profile.lifecycle_runtime_id = Some(self.sessions.runtime_id());
        profile.lifecycle_closing_session_id = None;
        profile.lifecycle_closing_runtime_id = None;
        if let Err(error) = self.store.save(&profile) {
            if let Err(close) = self.sessions.close(session_id) {
                let binding = WorkspaceBinding {
                    workspace_id: id.clone(),
                    tab_id,
                    session_id,
                };
                self.bindings.insert(id.clone(), binding);
                return Err(WorkspaceLifecycleError::StartRollback {
                    store: error,
                    close,
                });
            }
            return Err(error.into());
        }
        let binding = WorkspaceBinding {
            workspace_id: id.clone(),
            tab_id,
            session_id,
        };
        self.bindings.insert(id.clone(), binding.clone());
        Ok(binding)
    }

    fn require_profile(
        &self,
        id: &WorkspaceId,
    ) -> Result<WorkspaceProfile, WorkspaceLifecycleError> {
        self.store
            .load(id)?
            .ok_or_else(|| WorkspaceLifecycleError::NotFound(id.clone()))
    }
}

fn clear_runtime_association(profile: &mut WorkspaceProfile) {
    profile.session_ids.clear();
    profile.lifecycle_runtime_id = None;
    profile.lifecycle_closing_session_id = None;
    profile.lifecycle_closing_runtime_id = None;
}

fn migrate_legacy_intent(profile: &mut WorkspaceProfile) {
    if !profile.lifecycle_desired_open
        && profile.lifecycle_closing_session_id.is_none()
        && (profile.lifecycle_tab_id.is_some() || !profile.session_ids.is_empty())
    {
        profile.lifecycle_desired_open = true;
        profile
            .lifecycle_tab_id
            .get_or_insert_with(|| profile.id.as_str().to_owned());
    }
}
