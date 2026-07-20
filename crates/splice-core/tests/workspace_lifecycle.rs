use splice_core::{
    AgentDescriptor, EnvironmentMetadata, SessionId, SessionLifecycleError, SessionLifecyclePort,
    TabId, WorkspaceBinding, WorkspaceController, WorkspaceId, WorkspaceProfile, WorkspaceStore,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct FakeState {
    next_session: u64,
    runtime_id: String,
    running: BTreeMap<SessionId, WorkspaceId>,
    closed: Vec<SessionId>,
    start_failure: Option<SessionLifecycleError>,
    close_failure: Option<SessionLifecycleError>,
    remove_directory_on_start: Option<PathBuf>,
    remove_directory_on_close: Option<PathBuf>,
    non_idempotent_close: bool,
}

impl Default for FakeState {
    fn default() -> Self {
        Self {
            next_session: 0,
            runtime_id: "test-runtime".to_owned(),
            running: BTreeMap::new(),
            closed: vec![],
            start_failure: None,
            close_failure: None,
            remove_directory_on_start: None,
            remove_directory_on_close: None,
            non_idempotent_close: false,
        }
    }
}

#[derive(Clone, Default)]
struct FakeSessions(Arc<Mutex<FakeState>>);

impl FakeSessions {
    fn with_runtime_id(runtime_id: &str) -> Self {
        Self(Arc::new(Mutex::new(FakeState {
            runtime_id: runtime_id.to_owned(),
            ..FakeState::default()
        })))
    }
}

impl SessionLifecyclePort for FakeSessions {
    fn start(&mut self, profile: &WorkspaceProfile) -> Result<SessionId, SessionLifecycleError> {
        let mut state = self.0.lock().expect("fake state remains available");
        if let Some(error) = state.start_failure.clone() {
            return Err(error);
        }
        if let Some(directory) = state.remove_directory_on_start.take() {
            std::fs::remove_dir_all(directory).expect("test directory can be removed");
        }
        state.next_session += 1;
        let session = SessionId::new(state.next_session).expect("fake session id is non-zero");
        state.running.insert(session, profile.id.clone());
        Ok(session)
    }

    fn close(&mut self, session: SessionId) -> Result<(), SessionLifecycleError> {
        let mut state = self.0.lock().expect("fake state remains available");
        if let Some(error) = state.close_failure.clone() {
            return Err(error);
        }
        if state.running.remove(&session).is_none() && state.non_idempotent_close {
            return Err(retryable_error("session-not-found"));
        }
        state.closed.push(session);
        if let Some(directory) = state.remove_directory_on_close.take() {
            std::fs::remove_dir_all(directory).unwrap();
        }
        Ok(())
    }

    fn runtime_id(&self) -> String {
        self.0
            .lock()
            .expect("fake state remains available")
            .runtime_id
            .clone()
    }
}

fn root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "splice-core-lifecycle-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("workspace root exists");
    root
}

fn profile(id: &str, directory: PathBuf, sessions: Vec<u64>) -> WorkspaceProfile {
    WorkspaceProfile::new(
        WorkspaceId::new(id).expect("workspace id is valid"),
        id,
        directory,
        EnvironmentMetadata::new("development", ["PATH"]).expect("metadata is valid"),
        AgentDescriptor::new("codex", "codex").expect("agent is valid"),
        sessions,
    )
    .expect("profile is valid")
}

fn open_profile(
    id: &str,
    directory: PathBuf,
    session_id: SessionId,
    tab_id: &str,
    runtime_id: &str,
) -> WorkspaceProfile {
    WorkspaceProfile {
        session_ids: vec![session_id.get()],
        lifecycle_desired_open: true,
        lifecycle_tab_id: Some(tab_id.to_owned()),
        lifecycle_runtime_id: Some(runtime_id.to_owned()),
        ..profile(id, directory, vec![])
    }
}

fn session_error(
    code: &str,
    message: &str,
    platform: Option<&str>,
    retryable: bool,
) -> SessionLifecycleError {
    SessionLifecycleError {
        code: code.to_owned(),
        message: message.to_owned(),
        platform: platform.map(str::to_owned),
        retryable,
    }
}

fn retryable_error(code: &str) -> SessionLifecycleError {
    session_error(code, "Injected lifecycle failure.", None, true)
}

#[test]
fn lifecycle_errors_serialize_stably_for_invalid_and_injected_failures() {
    let invalid = SessionId::new(0)
        .expect_err("zero session IDs are rejected")
        .contract();
    assert_eq!(
        serde_json::to_value(invalid).unwrap(),
        serde_json::json!({
            "code": "invalid-session-id",
            "message": "Session ID must be non-zero.",
            "platform": null,
            "retryable": false,
        })
    );
    let invalid_tab = TabId::new("")
        .expect_err("empty tab IDs are rejected")
        .contract();
    assert_eq!(
        serde_json::to_value(invalid_tab).unwrap(),
        serde_json::json!({
            "code": "invalid-tab-id",
            "message": "Tab ID is invalid.",
            "platform": null,
            "retryable": false,
        })
    );

    let root = root("structured-session-error");
    let fake = FakeSessions::default();
    let expected = session_error(
        "session-start-failed",
        "Session service is unavailable.",
        Some("linux"),
        true,
    );
    fake.0.lock().unwrap().start_failure = Some(expected.clone());
    let mut controller = WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake);
    let error = controller
        .create(
            profile("alpha", root, vec![]),
            TabId::new("tab-alpha").unwrap(),
        )
        .expect_err("injected start failure is returned");

    assert_eq!(
        serde_json::to_value(error.contract()).unwrap(),
        serde_json::json!({
            "code": "session-start-failed",
            "message": "Session service is unavailable.",
            "platform": "linux",
            "retryable": true,
        })
    );
}

#[test]
fn failed_create_is_retryable_after_controller_reconstruction() {
    let root = root("create-start-retry");
    let fake = FakeSessions::default();
    let state = fake.0.clone();
    state.lock().unwrap().start_failure = Some(retryable_error("session-start-failed"));
    let store = WorkspaceStore::new(&root).unwrap();
    WorkspaceController::new(store, fake.clone())
        .create(
            profile("alpha", root.clone(), vec![]),
            TabId::new("tab").unwrap(),
        )
        .unwrap_err();

    state.lock().unwrap().start_failure = None;
    let mut controller = WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake);
    controller
        .create(profile("alpha", root, vec![]), TabId::new("tab").unwrap())
        .expect("the same create request resumes");
}

#[test]
fn failed_restart_start_recovers_tab_identity_after_reconstruction() {
    let root = root("restart-start-recovery");
    let fake = FakeSessions::default();
    let state = fake.0.clone();
    let mut controller =
        WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake.clone());
    let original = controller
        .create(
            profile("alpha", root.clone(), vec![]),
            TabId::new("stable-tab").unwrap(),
        )
        .unwrap();
    state.lock().unwrap().start_failure = Some(retryable_error("session-start-failed"));
    controller
        .restart(&original.workspace_id)
        .expect_err("replacement start fails");

    state.lock().unwrap().start_failure = None;
    let mut recovered = WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake);
    let bindings = recovered.recover().unwrap();
    assert_eq!(bindings[0].tab_id, TabId::new("stable-tab").unwrap());
    let state = state.lock().unwrap();
    assert_eq!(state.closed, vec![original.session_id]);
    assert_eq!(
        state.running,
        BTreeMap::from([(bindings[0].session_id, original.workspace_id)])
    );
}

#[test]
fn failed_restart_start_retries_with_the_durable_tab_intent() {
    let root = root("restart-start-direct-retry");
    let fake = FakeSessions::default();
    let state = fake.0.clone();
    let mut controller =
        WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake.clone());
    let original = controller
        .create(
            profile("alpha", root, vec![]),
            TabId::new("stable-tab").unwrap(),
        )
        .unwrap();
    state.lock().unwrap().start_failure = Some(retryable_error("session-start-failed"));
    controller
        .restart(&original.workspace_id)
        .expect_err("replacement start fails");

    state.lock().unwrap().start_failure = None;
    let replacement = controller
        .restart(&original.workspace_id)
        .expect("direct restart uses the durable tab intent");
    let state = state.lock().unwrap();
    assert_eq!(replacement.tab_id, TabId::new("stable-tab").unwrap());
    assert_eq!(state.closed, vec![original.session_id]);
    assert_eq!(
        state.running,
        BTreeMap::from([(replacement.session_id, original.workspace_id)])
    );
}

#[test]
fn failed_start_save_and_rollback_keeps_live_session_addressable() {
    let root = root("start-save-rollback-failure");
    let working = root.join("working");
    std::fs::create_dir_all(&working).unwrap();
    let store = WorkspaceStore::new(&root).unwrap();
    store
        .save(&profile("alpha", working.clone(), vec![9]))
        .unwrap();
    let fake = FakeSessions::default();
    let state = fake.0.clone();
    {
        let mut state = state.lock().unwrap();
        state.remove_directory_on_start = Some(working.clone());
        state.close_failure = Some(retryable_error("session-close-failed"));
    }
    let mut controller = WorkspaceController::new(store, fake.clone());
    let failure = controller
        .recover()
        .expect_err("save and rollback close fail");
    assert_eq!(failure.contract().code, "workspace-start-rollback-failed");
    assert!(failure.contract().retryable);
    let binding = controller.bindings().pop().unwrap();

    std::fs::create_dir_all(&working).unwrap();
    state.lock().unwrap().close_failure = None;
    let recovered = controller
        .create(
            profile("alpha", working.clone(), vec![]),
            TabId::new("alpha").unwrap(),
        )
        .unwrap();
    assert_eq!(recovered, binding);
    assert_eq!(
        controller.list().unwrap()[0].lifecycle_closing_session_id,
        Some(binding.session_id.get())
    );
    drop(controller);
    let mut reconstructed = WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake);
    let replacement = reconstructed.recover().unwrap();
    assert!(replacement.is_empty());
    assert!(state.lock().unwrap().running.is_empty());
    assert_eq!(state.lock().unwrap().closed, vec![binding.session_id]);
}

#[test]
fn update_preserves_pending_start_and_close_intents() {
    let root = root("update-intents");
    let store = WorkspaceStore::new(&root).unwrap();
    let mut pending_start = profile("alpha", root.clone(), vec![]);
    pending_start.lifecycle_tab_id = Some("stable-tab".into());
    store.save(&pending_start).unwrap();
    let mut pending_close = profile("beta", root.clone(), vec![7]);
    pending_close.lifecycle_closing_session_id = Some(7);
    pending_close.lifecycle_closing_runtime_id = Some("runtime-pending-close".to_owned());
    store.save(&pending_close).unwrap();
    let mut controller = WorkspaceController::new(store, FakeSessions::default());

    controller
        .update(profile("alpha", root.clone(), vec![]))
        .unwrap();
    controller.update(profile("beta", root, vec![])).unwrap();
    let profiles = controller.list().unwrap();
    assert_eq!(profiles[0].lifecycle_tab_id.as_deref(), Some("stable-tab"));
    assert_eq!(profiles[1].session_ids, vec![7]);
    assert_eq!(profiles[1].lifecycle_closing_session_id, Some(7));
    assert_eq!(
        profiles[1].lifecycle_closing_runtime_id.as_deref(),
        Some("runtime-pending-close")
    );
}

#[test]
fn reconstructed_close_converges_after_clear_save_failure() {
    let root = root("close-clear-reconstruction");
    let working = root.join("working");
    std::fs::create_dir_all(&working).unwrap();
    let fake = FakeSessions::default();
    let state = fake.0.clone();
    state.lock().unwrap().non_idempotent_close = true;
    let mut controller =
        WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake.clone());
    let binding = controller
        .create(
            profile("alpha", working.clone(), vec![]),
            TabId::new("tab").unwrap(),
        )
        .unwrap();
    state.lock().unwrap().remove_directory_on_close = Some(working.clone());
    controller.close(&binding.workspace_id).unwrap_err();

    std::fs::create_dir_all(&working).unwrap();
    let mut reconstructed = WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake);
    assert!(reconstructed.recover().unwrap().is_empty());
    assert_eq!(
        reconstructed.list().unwrap(),
        vec![profile("alpha", working, vec![])]
    );
    assert_eq!(state.lock().unwrap().closed, vec![binding.session_id]);
}

#[test]
fn recovery_continues_after_an_earlier_profile_fails() {
    let root = root("recovery-aggregate");
    let store = WorkspaceStore::new(&root).unwrap();
    let mut alpha = profile("alpha", root.clone(), vec![7]);
    alpha.lifecycle_closing_session_id = Some(7);
    alpha.lifecycle_closing_runtime_id = Some("test-runtime".to_owned());
    store.save(&alpha).unwrap();
    store.save(&profile("beta", root, vec![9])).unwrap();
    let fake = FakeSessions::default();
    let state = fake.0.clone();
    state.lock().unwrap().close_failure = Some(retryable_error("session-close-failed"));
    let mut controller = WorkspaceController::new(store, fake);

    let failure = controller.recover().unwrap_err();
    assert_eq!(failure.contract().code, "session-close-failed");
    assert_eq!(
        controller.bindings()[0].workspace_id,
        WorkspaceId::new("beta").unwrap()
    );
}

#[test]
fn close_failure_persists_cleanup_intent_for_reconstructed_recovery() {
    let root = root("close-reconstruction");
    let fake = FakeSessions::default();
    let state = fake.0.clone();
    let mut controller =
        WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake.clone());
    let alpha = controller
        .create(
            profile("alpha", root.clone(), vec![]),
            TabId::new("tab-alpha").unwrap(),
        )
        .unwrap();
    state.lock().unwrap().close_failure = Some(retryable_error("session-close-failed"));
    controller.close(&alpha.workspace_id).unwrap_err();

    assert_eq!(
        controller.list().unwrap(),
        vec![WorkspaceProfile {
            session_ids: vec![alpha.session_id.get()],
            lifecycle_runtime_id: Some("test-runtime".to_owned()),
            lifecycle_closing_session_id: Some(alpha.session_id.get()),
            lifecycle_closing_runtime_id: Some("test-runtime".to_owned()),
            ..profile("alpha", root.clone(), vec![])
        }]
    );

    let mut reconstructed = WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake);
    reconstructed
        .recover()
        .expect_err("reconstructed recovery retries cleanup before any replacement start");
    assert_eq!(
        state.lock().unwrap().running,
        BTreeMap::from([(alpha.session_id, alpha.workspace_id.clone())])
    );
    assert!(state.lock().unwrap().closed.is_empty());

    state.lock().unwrap().close_failure = None;
    assert!(reconstructed.recover().unwrap().is_empty());
    assert_eq!(state.lock().unwrap().closed, vec![alpha.session_id]);
    assert!(state.lock().unwrap().running.is_empty());
    assert_eq!(
        reconstructed.list().unwrap(),
        vec![profile("alpha", root, vec![])]
    );
}

#[test]
fn duplicate_workspace_conflict_uses_the_controller_error_contract() {
    let root = root("workspace-conflict");
    let mut controller =
        WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), FakeSessions::default());
    controller
        .create(
            profile("alpha", root.clone(), vec![]),
            TabId::new("tab-alpha").unwrap(),
        )
        .unwrap();

    let conflict = controller
        .create(
            profile("alpha", root, vec![]),
            TabId::new("tab-retry").unwrap(),
        )
        .expect_err("duplicate profile conflicts");
    assert_eq!(
        serde_json::to_value(conflict.contract()).unwrap(),
        serde_json::json!({
            "code": "workspace-conflict",
            "message": "Workspace already exists.",
            "platform": null,
            "retryable": false,
        })
    );
}

#[test]
fn close_failure_preserves_the_binding_until_the_port_succeeds() {
    let root = root("close-failure");
    let fake = FakeSessions::default();
    let state = fake.0.clone();
    let mut controller = WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake);
    let alpha = controller
        .create(
            profile("alpha", root, vec![]),
            TabId::new("tab-alpha").unwrap(),
        )
        .unwrap();
    state.lock().unwrap().close_failure = Some(session_error(
        "session-close-failed",
        "Session service is busy.",
        Some("windows"),
        true,
    ));

    let failure = controller
        .close(&alpha.workspace_id)
        .expect_err("close failure propagates");
    assert_eq!(failure.contract().code, "session-close-failed");
    assert_eq!(controller.bindings(), vec![alpha.clone()]);
    assert_eq!(
        controller.list().unwrap()[0].session_ids,
        vec![alpha.session_id.get()]
    );
    assert_eq!(
        controller.list().unwrap()[0].lifecycle_closing_session_id,
        Some(alpha.session_id.get())
    );

    state.lock().unwrap().close_failure = None;
    controller
        .close(&alpha.workspace_id)
        .expect("retry closes once");
    let state = state.lock().unwrap();
    assert_eq!(state.closed, vec![alpha.session_id]);
    assert!(state.running.is_empty());
}

#[test]
fn close_persistence_failure_does_not_close_and_keeps_other_workspaces_isolated() {
    let root = root("close-persistence-retry");
    let working_directory = root.join("working");
    std::fs::create_dir_all(&working_directory).unwrap();
    let fake = FakeSessions::default();
    let state = fake.0.clone();
    let mut controller = WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake);
    let alpha = controller
        .create(
            profile("alpha", working_directory.clone(), vec![]),
            TabId::new("tab-alpha").unwrap(),
        )
        .unwrap();
    let beta = controller
        .create(
            profile("beta", working_directory.clone(), vec![]),
            TabId::new("tab-beta").unwrap(),
        )
        .unwrap();

    std::fs::remove_dir_all(&working_directory).unwrap();
    let failure = controller
        .close(&alpha.workspace_id)
        .expect_err("persistence fails before port close");
    assert_eq!(failure.contract().code, "workspace-store-failure");
    assert!(state.lock().unwrap().closed.is_empty());
    assert_eq!(controller.bindings(), vec![alpha.clone(), beta.clone()]);

    std::fs::create_dir_all(&working_directory).unwrap();
    controller
        .close(&alpha.workspace_id)
        .expect("close retries safely");
    assert_eq!(state.lock().unwrap().closed, vec![alpha.session_id]);
    assert_eq!(
        controller.list().unwrap(),
        vec![
            profile("alpha", working_directory.clone(), vec![]),
            open_profile(
                "beta",
                working_directory,
                beta.session_id,
                "tab-beta",
                "test-runtime",
            ),
        ]
    );
}

#[test]
fn restart_reconciles_a_pending_close_without_double_closing_or_leaking() {
    let root = root("restart-persistence-retry");
    let working_directory = root.join("working");
    std::fs::create_dir_all(&working_directory).unwrap();
    let fake = FakeSessions::default();
    let state = fake.0.clone();
    let mut controller = WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake);
    let alpha = controller
        .create(
            profile("alpha", working_directory.clone(), vec![]),
            TabId::new("tab-alpha").unwrap(),
        )
        .unwrap();
    let beta = controller
        .create(
            profile("beta", working_directory.clone(), vec![]),
            TabId::new("tab-beta").unwrap(),
        )
        .unwrap();

    std::fs::remove_dir_all(&working_directory).unwrap();
    controller
        .restart(&alpha.workspace_id)
        .expect_err("restart stops at failed close persistence");
    std::fs::create_dir_all(&working_directory).unwrap();
    let restarted = controller
        .restart(&alpha.workspace_id)
        .expect("retry reconciles then starts a replacement");

    let state = state.lock().unwrap();
    assert_eq!(state.closed, vec![alpha.session_id]);
    assert_eq!(
        state.running,
        BTreeMap::from([
            (restarted.session_id, alpha.workspace_id),
            (beta.session_id, beta.workspace_id.clone()),
        ])
    );
    assert_eq!(controller.bindings(), vec![restarted, beta]);
}

#[test]
fn recovery_cleans_up_a_failed_start_and_retries_after_persistence_recovers() {
    let root = root("start-persistence-retry");
    let working_directory = root.join("working");
    std::fs::create_dir_all(&working_directory).unwrap();
    let store = WorkspaceStore::new(&root).unwrap();
    store
        .save(&profile("alpha", working_directory.clone(), vec![9]))
        .unwrap();
    let fake = FakeSessions::default();
    let state = fake.0.clone();
    state.lock().unwrap().remove_directory_on_start = Some(working_directory.clone());
    let mut controller = WorkspaceController::new(store, fake);

    controller
        .recover()
        .expect_err("failed save cleans up the started session");
    assert_eq!(
        state.lock().unwrap().closed,
        vec![SessionId::new(1).unwrap()]
    );
    assert!(controller.bindings().is_empty());

    std::fs::create_dir_all(&working_directory).unwrap();
    let recovered = controller
        .recover()
        .expect("recovery retries after storage heals");
    assert_eq!(recovered.len(), 1);
    let state = state.lock().unwrap();
    assert_eq!(state.closed, vec![SessionId::new(1).unwrap()]);
    assert_eq!(
        state.running,
        BTreeMap::from([(recovered[0].session_id, WorkspaceId::new("alpha").unwrap())])
    );
}

#[test]
fn lifecycle_keeps_workspace_tab_and_session_identities_isolated() {
    let root = root("identity-isolation");
    let fake = FakeSessions::default();
    let state = fake.0.clone();
    let store = WorkspaceStore::new(&root).expect("store root is absolute");
    let mut controller = WorkspaceController::new(store, fake);

    let alpha = controller
        .create(
            profile("alpha", root.clone(), vec![]),
            TabId::new("tab-alpha").unwrap(),
        )
        .expect("alpha starts");
    let beta = controller
        .create(
            profile("beta", root.clone(), vec![]),
            TabId::new("tab-beta").unwrap(),
        )
        .expect("beta starts");
    controller.select(&beta.workspace_id).expect("beta selects");
    let mut updated_beta = profile("beta", root.clone(), vec![]);
    updated_beta.name = "Beta renamed".to_owned();
    controller
        .update(updated_beta.clone())
        .expect("profile update keeps its session");
    controller.close(&alpha.workspace_id).expect("alpha closes");
    controller
        .close(&alpha.workspace_id)
        .expect("repeat close is idempotent");
    let restarted_beta = controller
        .restart(&beta.workspace_id)
        .expect("restart replaces only beta's session");
    let missing = WorkspaceId::new("missing").unwrap();

    assert_ne!(alpha.workspace_id, beta.workspace_id);
    assert_ne!(alpha.tab_id, beta.tab_id);
    assert_ne!(alpha.session_id, beta.session_id);
    assert_eq!(beta.workspace_id, restarted_beta.workspace_id);
    assert_eq!(beta.tab_id, restarted_beta.tab_id);
    assert_ne!(beta.session_id, restarted_beta.session_id);
    assert_eq!(controller.selected(), Some(&beta.workspace_id));
    assert_eq!(controller.bindings(), vec![restarted_beta.clone()]);
    assert!(
        matches!(controller.select(&missing), Err(splice_core::WorkspaceLifecycleError::NotFound(id)) if id == missing)
    );
    assert_eq!(
        controller.list().expect("profiles list"),
        vec![
            profile("alpha", root.clone(), vec![]),
            WorkspaceProfile {
                session_ids: vec![restarted_beta.session_id.get()],
                lifecycle_desired_open: true,
                lifecycle_tab_id: Some("tab-beta".to_owned()),
                lifecycle_runtime_id: Some("test-runtime".to_owned()),
                ..updated_beta
            },
        ]
    );
    let state = state.lock().expect("fake state remains available");
    assert_eq!(state.closed, vec![alpha.session_id, beta.session_id]);
    assert_eq!(
        state.running,
        BTreeMap::from([(restarted_beta.session_id, beta.workspace_id)])
    );
}

#[test]
fn recovery_replaces_stale_sessions_and_is_idempotent() {
    let root = root("recovery");
    let store = WorkspaceStore::new(&root).expect("store root is absolute");
    store
        .save(&profile("alpha", root.clone(), vec![9]))
        .unwrap();
    store
        .save(&profile("beta", root.clone(), vec![11]))
        .unwrap();
    let fake = FakeSessions::default();
    let state = fake.0.clone();
    let mut controller = WorkspaceController::new(store, fake);

    let recovered = controller.recover().expect("stale sessions recover");
    assert_eq!(recovered.len(), 2);
    assert_ne!(recovered[0].session_id.get(), 9);
    assert_ne!(recovered[1].session_id.get(), 11);
    assert_eq!(
        controller.recover().expect("recovery is idempotent"),
        recovered
    );
    assert_eq!(
        controller.list().expect("profiles list"),
        vec![
            open_profile(
                "alpha",
                root.clone(),
                recovered[0].session_id,
                "alpha",
                "test-runtime",
            ),
            open_profile(
                "beta",
                root,
                recovered[1].session_id,
                "beta",
                "test-runtime",
            ),
        ]
    );
    assert_eq!(state.lock().unwrap().running.len(), 2);
}

#[test]
fn recovery_normalizes_all_stale_runtime_associations_before_reusing_session_ids() {
    let root = root("recovery-atomic-normalization");
    let store = WorkspaceStore::new(&root).expect("store root is absolute");
    let mut alpha = profile("alpha", root.clone(), vec![2]);
    alpha.lifecycle_tab_id = Some("tab-alpha".to_owned());
    let mut beta = profile("beta", root.clone(), vec![1]);
    beta.lifecycle_tab_id = Some("tab-beta".to_owned());
    store
        .save(&alpha)
        .expect("alpha persisted with an old runtime ID");
    store
        .save(&beta)
        .expect("beta persisted with an old runtime ID");
    let mut controller = WorkspaceController::new(store, FakeSessions::default());

    let recovered = controller
        .recover()
        .expect("recovery clears every stale association before allocating IDs");

    assert_eq!(
        recovered,
        vec![
            WorkspaceBinding {
                workspace_id: WorkspaceId::new("alpha").unwrap(),
                tab_id: TabId::new("tab-alpha").unwrap(),
                session_id: SessionId::new(1).unwrap(),
            },
            WorkspaceBinding {
                workspace_id: WorkspaceId::new("beta").unwrap(),
                tab_id: TabId::new("tab-beta").unwrap(),
                session_id: SessionId::new(2).unwrap(),
            },
        ]
    );
}

#[test]
fn explicit_close_after_natural_exit_durably_clears_recovery_intent() {
    let root = root("explicit-close-after-natural-exit");
    let fake = FakeSessions::default();
    let mut controller =
        WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake.clone());
    let alpha = controller
        .create(
            profile("alpha", root.clone(), vec![]),
            TabId::new("tab-alpha").unwrap(),
        )
        .expect("alpha starts");
    controller
        .reconcile_terminated_session(alpha.session_id)
        .expect("natural exit preserves recoverable intent");

    controller
        .close(&alpha.workspace_id)
        .expect("explicit close clears the durable intent even without a binding");
    drop(controller);

    let mut reconstructed = WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake);
    assert!(reconstructed
        .recover()
        .expect("closed workspaces are never restarted")
        .is_empty());
    assert_eq!(
        reconstructed.list().unwrap(),
        vec![profile("alpha", root, vec![])]
    );
}

#[test]
fn successful_start_preserves_the_durable_tab_id() {
    let root = root("start-preserves-tab-id");
    let mut controller =
        WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), FakeSessions::default());

    controller
        .create(
            profile("alpha", root.clone(), vec![]),
            TabId::new("stable-tab").unwrap(),
        )
        .expect("start succeeds");

    assert_eq!(
        controller.list().unwrap()[0].lifecycle_tab_id.as_deref(),
        Some("stable-tab"),
        "a successful start must not discard the durable tab identity"
    );
}

#[test]
fn recovery_drops_a_stale_close_intent_from_another_runtime_without_closing_a_reused_id() {
    let root = root("recovery-runtime-identity-mismatch");
    let old_sessions = FakeSessions::with_runtime_id("epoch-old");
    let old_state = old_sessions.0.clone();
    let mut old_controller = WorkspaceController::new(
        WorkspaceStore::new(&root).expect("store root is absolute"),
        old_sessions,
    );
    let beta = old_controller
        .create(
            profile("beta", root.clone(), vec![]),
            TabId::new("tab-beta").unwrap(),
        )
        .expect("old beta starts as session one");
    old_state.lock().unwrap().close_failure = Some(retryable_error("session-close-failed"));
    old_controller
        .close(&beta.workspace_id)
        .expect_err("old beta persists a close intent before its port failure");
    drop(old_controller);

    let current_sessions = FakeSessions::with_runtime_id("epoch-new");
    let current_state = current_sessions.0.clone();
    let mut controller = WorkspaceController::new(
        WorkspaceStore::new(&root).expect("store root is absolute"),
        current_sessions,
    );
    let alpha = controller
        .create(
            profile("alpha", root.clone(), vec![]),
            TabId::new("tab-alpha").unwrap(),
        )
        .expect("new alpha reuses session one in its new runtime");
    assert_eq!(alpha.session_id, beta.session_id);

    assert_eq!(
        controller
            .recover()
            .expect("stale beta cleanup is discarded"),
        vec![alpha.clone()]
    );
    assert_eq!(controller.bindings(), vec![alpha.clone()]);
    let profiles = controller.list().expect("profiles remain persisted");
    assert_eq!(
        profiles[0],
        open_profile(
            "alpha",
            root.clone(),
            alpha.session_id,
            "tab-alpha",
            "epoch-new",
        )
    );
    assert_eq!(profiles[1], profile("beta", root, vec![]));
    let state = current_state.lock().unwrap();
    assert!(
        state.closed.is_empty(),
        "beta must not close alpha's reused ID"
    );
    assert_eq!(
        state.running,
        BTreeMap::from([(alpha.session_id, alpha.workspace_id)])
    );
}

#[test]
fn recovery_reconciles_a_close_intent_from_the_same_runtime() {
    let root = root("recovery-runtime-identity-match");
    let sessions = FakeSessions::with_runtime_id("epoch-current");
    let state = sessions.0.clone();
    let mut controller = WorkspaceController::new(
        WorkspaceStore::new(&root).expect("store root is absolute"),
        sessions,
    );
    let beta = controller
        .create(
            profile("beta", root.clone(), vec![]),
            TabId::new("tab-beta").unwrap(),
        )
        .expect("beta starts");
    state.lock().unwrap().close_failure = Some(retryable_error("session-close-failed"));
    controller
        .close(&beta.workspace_id)
        .expect_err("close intent remains durable while the port is unavailable");
    state.lock().unwrap().close_failure = None;
    drop(controller);

    let mut recovered = WorkspaceController::new(
        WorkspaceStore::new(&root).expect("store root is absolute"),
        FakeSessions(state.clone()),
    );
    assert!(recovered
        .recover()
        .expect("same-runtime close intent is reconciled")
        .is_empty());
    assert_eq!(
        recovered.list().unwrap(),
        vec![profile("beta", root, vec![])]
    );
    let state = state.lock().unwrap();
    assert_eq!(state.closed, vec![beta.session_id]);
    assert!(state.running.is_empty());
}

#[test]
fn recovery_discards_a_legacy_close_intent_without_a_runtime_id() {
    let root = root("recovery-legacy-runtime-identity");
    let store = WorkspaceStore::new(&root).expect("store root is absolute");
    let mut beta = profile("beta", root.clone(), vec![7]);
    beta.lifecycle_tab_id = Some("tab-beta".to_owned());
    beta.lifecycle_closing_session_id = Some(7);
    store
        .save(&beta)
        .expect("profiles written before runtime identity remain readable");
    let fake = FakeSessions::with_runtime_id("epoch-new");
    let state = fake.0.clone();
    let mut controller = WorkspaceController::new(store, fake);

    assert!(controller
        .recover()
        .expect("legacy cleanup intent is safely discarded")
        .is_empty());
    assert_eq!(
        controller.list().unwrap(),
        vec![profile("beta", root, vec![])]
    );
    assert!(state.lock().unwrap().closed.is_empty());
}

#[test]
fn terminated_session_reconciles_only_its_binding_and_allows_create_or_restart_to_replace_it() {
    let root = root("terminated-session");
    let store = WorkspaceStore::new(&root).expect("store root is absolute");
    let fake = FakeSessions::default();
    let mut controller = WorkspaceController::new(store, fake);
    let alpha_id = WorkspaceId::new("alpha").expect("workspace ID is valid");
    let beta_id = WorkspaceId::new("beta").expect("workspace ID is valid");

    let alpha = controller
        .create(
            profile("alpha", root.clone(), vec![]),
            TabId::new("tab-alpha").expect("tab ID is valid"),
        )
        .expect("alpha starts");
    let beta = controller
        .create(
            profile("beta", root.clone(), vec![]),
            TabId::new("tab-beta").expect("tab ID is valid"),
        )
        .expect("beta starts");

    controller
        .reconcile_terminated_session(alpha.session_id)
        .expect("natural exit reconciles its workspace");
    controller
        .reconcile_terminated_session(alpha.session_id)
        .expect("a repeated exit callback is idempotent");

    assert_eq!(
        controller.bindings(),
        vec![beta.clone()],
        "an exit never clears another workspace binding"
    );
    let profiles = controller.list().expect("profiles remain readable");
    assert_eq!(profiles[0].id, alpha_id);
    assert!(profiles[0].session_ids.is_empty());
    assert_eq!(profiles[0].lifecycle_tab_id.as_deref(), Some("tab-alpha"));
    assert_eq!(profiles[1].id, beta_id);
    assert_eq!(profiles[1].session_ids, vec![beta.session_id.get()]);

    let replacement = controller
        .create(
            profile("alpha", root.clone(), vec![]),
            TabId::new("tab-alpha").expect("tab ID is valid"),
        )
        .expect("create replaces an exited session instead of returning its stale ID");
    assert_ne!(replacement.session_id, alpha.session_id);

    controller
        .reconcile_terminated_session(replacement.session_id)
        .expect("replacement exit reconciles");
    let restarted = controller
        .restart(&alpha_id)
        .expect("restart replaces the reconciled session");
    assert_ne!(restarted.session_id, replacement.session_id);
}

#[test]
fn terminated_session_save_failure_drops_dead_binding_and_retries_after_store_recovers() {
    let root = root("terminated-session-save-retry");
    let fake = FakeSessions::default();
    let state = fake.0.clone();
    let mut controller = WorkspaceController::new(WorkspaceStore::new(&root).unwrap(), fake);
    let original = controller
        .create(
            profile("alpha", root.clone(), vec![]),
            TabId::new("tab-alpha").unwrap(),
        )
        .unwrap();

    state.lock().unwrap().running.remove(&original.session_id);
    let primary = root.join("workspace-profiles.v1.json");
    let backup = root.join("workspace-profiles.v1.json.bak");
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(&primary, &backup).unwrap();
    std::fs::create_dir(&primary).unwrap();

    let failure = controller
        .reconcile_terminated_session(original.session_id)
        .expect_err("a failed reconciliation save is returned");
    assert_eq!(failure.contract().code, "workspace-store-failure");
    assert!(failure.contract().retryable);
    assert!(controller.bindings().is_empty());

    let retry = controller
        .create(
            profile("alpha", root.clone(), vec![]),
            TabId::new("tab-alpha").unwrap(),
        )
        .expect_err("a pending reconciliation prevents a stale session from returning");
    assert_eq!(retry.contract().code, "workspace-store-failure");
    assert!(retry.contract().retryable);

    std::fs::remove_dir(&primary).unwrap();
    let replacement = controller
        .create(
            profile("alpha", root.clone(), vec![]),
            TabId::new("tab-alpha").unwrap(),
        )
        .expect("store recovery persists the termination before starting a replacement");

    assert_ne!(replacement.session_id, original.session_id);
    let state = state.lock().unwrap();
    assert_eq!(state.next_session, replacement.session_id.get());
    assert_eq!(
        state.running,
        BTreeMap::from([(replacement.session_id, replacement.workspace_id.clone())])
    );
}
