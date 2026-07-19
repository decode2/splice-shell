use crate::{WorkspaceError, WorkspaceId, WorkspaceProfile, WorkspaceStore};
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabId(String);

impl TabId {
    pub fn new(value: impl Into<String>) -> Result<Self, WorkspaceLifecycleError> {
        let value = value.into();
        (!value.is_empty() && value.len() <= 64)
            .then_some(Self(value))
            .ok_or(WorkspaceLifecycleError::InvalidTabId)
    }

    fn as_str(&self) -> &str {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceBinding {
    pub workspace_id: WorkspaceId,
    pub tab_id: TabId,
    pub session_id: SessionId,
}

pub struct WorkspaceController<P> {
    store: WorkspaceStore,
    sessions: P,
    bindings: BTreeMap<WorkspaceId, WorkspaceBinding>,
    selected: Option<WorkspaceId>,
}

impl<P: SessionLifecyclePort> WorkspaceController<P> {
    pub fn new(store: WorkspaceStore, sessions: P) -> Self {
        Self {
            store,
            sessions,
            bindings: BTreeMap::new(),
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
        if let Some(existing) = self.store.load(&profile.id)? {
            let retrying_live_binding = self
                .bindings
                .get(&existing.id)
                .is_some_and(|binding| binding.tab_id == tab_id);
            if existing.lifecycle_tab_id.as_deref() != Some(tab_id.as_str())
                && !retrying_live_binding
            {
                return Err(WorkspaceLifecycleError::Conflict(profile.id));
            }
            return self.start(&existing.id, tab_id);
        }
        profile.session_ids.clear();
        profile.lifecycle_tab_id = Some(tab_id.as_str().to_owned());
        self.store.save(&profile)?;
        let binding = self.start(&profile.id, tab_id)?;
        self.selected = Some(binding.workspace_id.clone());
        Ok(binding)
    }

    pub fn select(&mut self, id: &WorkspaceId) -> Result<(), WorkspaceLifecycleError> {
        self.require_profile(id)?;
        self.selected = Some(id.clone());
        Ok(())
    }

    pub fn update(&mut self, mut profile: WorkspaceProfile) -> Result<(), WorkspaceLifecycleError> {
        let current = self.require_profile(&profile.id)?;
        profile.session_ids = current.session_ids;
        profile.lifecycle_tab_id = current.lifecycle_tab_id;
        profile.lifecycle_closing_session_id = current.lifecycle_closing_session_id;
        self.store.save(&profile)?;
        Ok(())
    }

    pub fn close(&mut self, id: &WorkspaceId) -> Result<(), WorkspaceLifecycleError> {
        let mut profile = self.require_profile(id)?;
        let closing_session = profile
            .lifecycle_closing_session_id
            .map(SessionId::new)
            .transpose()?
            .or_else(|| self.bindings.get(id).map(|binding| binding.session_id));
        if let Some(session_id) = closing_session {
            if profile.lifecycle_closing_session_id.is_none() {
                let binding = self.bindings.get(id).expect("a live session has a binding");
                profile.session_ids = vec![session_id.get()];
                profile.lifecycle_tab_id = Some(binding.tab_id.as_str().to_owned());
                profile.lifecycle_closing_session_id = Some(session_id.get());
                self.store.save(&profile)?;
            }
            if let Err(error) = self.sessions.close(session_id) {
                if !error.closes_convergently() {
                    return Err(WorkspaceLifecycleError::Session(error));
                }
            }
            profile.session_ids.clear();
            profile.lifecycle_tab_id = None;
            profile.lifecycle_closing_session_id = None;
            self.store.save(&profile)?;
            self.bindings.remove(id);
        }
        self.reconcile_close(id)
    }

    fn reconcile_close(&mut self, id: &WorkspaceId) -> Result<(), WorkspaceLifecycleError> {
        let mut profile = self.require_profile(id)?;
        if !profile.session_ids.is_empty()
            && profile.lifecycle_closing_session_id.is_none()
            && !self.bindings.contains_key(id)
        {
            profile.session_ids.clear();
            profile.lifecycle_tab_id = None;
            self.store.save(&profile)?;
        }
        if self.selected.as_ref() == Some(id) {
            self.selected = None;
        }
        Ok(())
    }

    pub fn restart(
        &mut self,
        id: &WorkspaceId,
    ) -> Result<WorkspaceBinding, WorkspaceLifecycleError> {
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
        for profile in self.store.list()? {
            let result = if profile.lifecycle_closing_session_id.is_some() {
                self.close(&profile.id)
            } else if profile.session_ids.is_empty() && profile.lifecycle_tab_id.is_none() {
                Ok(())
            } else {
                let tab_id = profile
                    .lifecycle_tab_id
                    .as_deref()
                    .unwrap_or(profile.id.as_str());
                self.start(&profile.id, TabId::new(tab_id)?).map(drop)
            };
            if let Err(error) = result {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or_else(|| Ok(self.bindings()), Err)
    }

    fn start(
        &mut self,
        id: &WorkspaceId,
        tab_id: TabId,
    ) -> Result<WorkspaceBinding, WorkspaceLifecycleError> {
        if let Some(binding) = self.bindings.get(id) {
            let mut profile = self.require_profile(id)?;
            if !profile.session_ids.contains(&binding.session_id.get()) {
                profile.session_ids = vec![binding.session_id.get()];
                profile.lifecycle_tab_id = Some(binding.tab_id.as_str().to_owned());
                profile.lifecycle_closing_session_id = Some(binding.session_id.get());
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
        profile.lifecycle_tab_id = None;
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
