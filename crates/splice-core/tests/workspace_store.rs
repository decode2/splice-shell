use fs2::FileExt;
use splice_core::{
    AgentDescriptor, EnvironmentMetadata, WorkspaceId, WorkspaceProfile, WorkspaceStore,
};
use std::{
    fs::OpenOptions,
    path::PathBuf,
    sync::{mpsc, Arc, Barrier},
    thread,
    time::Duration,
};
fn temp_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("splice-core-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp root should exist");
    root
}
fn profile(id: &str, directory: PathBuf, session_ids: Vec<u64>) -> WorkspaceProfile {
    WorkspaceProfile::new(
        WorkspaceId::new(id).expect("valid workspace id"),
        id,
        directory,
        EnvironmentMetadata::new("development", ["PATH"]).expect("safe metadata"),
        AgentDescriptor::new("codex", "codex").expect("safe agent"),
        session_ids,
    )
    .expect("valid workspace profile")
}
fn duplicate_session_profiles(root: &PathBuf, session_id: u64) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "profiles": {
            "alpha": {
                "id": "alpha",
                "name": "alpha",
                "working_directory": root,
                "environment": { "profile": "development", "variable_names": ["PATH"] },
                "agent": { "id": "codex", "command": "codex" },
                "session_ids": [session_id]
            },
            "beta": {
                "id": "beta",
                "name": "beta",
                "working_directory": root,
                "environment": { "profile": "development", "variable_names": ["PATH"] },
                "agent": { "id": "codex", "command": "codex" },
                "session_ids": [session_id]
            }
        }
    })
}
#[test]
fn isolates_workspaces_and_rejects_shared_sessions() {
    let root = temp_root("isolation");
    let workspace_a = profile("alpha", root.clone(), vec![11]);
    let workspace_b = profile("beta", root.clone(), vec![22]);
    let store = WorkspaceStore::new(&root).expect("absolute store root");
    store.save(&workspace_a).expect("first workspace saves");
    assert!(store
        .save(&profile("beta", root.clone(), vec![11]))
        .is_err());
    store.save(&workspace_b).expect("second workspace saves");
    assert_eq!(
        store.load(&workspace_a.id).expect("workspace loads"),
        Some(workspace_a)
    );
    assert_eq!(
        store.load(&workspace_b.id).expect("workspace loads"),
        Some(workspace_b)
    );
}
#[test]
fn loads_all_metadata_when_a_workspace_directory_is_unavailable() {
    let root = temp_root("offline-directory");
    let directory_a = root.join("alpha-directory");
    let directory_b = root.join("beta-directory");
    std::fs::create_dir_all(&directory_a).expect("alpha directory exists");
    std::fs::create_dir_all(&directory_b).expect("beta directory exists");
    let workspace_a = profile("alpha", directory_a.clone(), vec![41]);
    let workspace_b = profile("beta", directory_b, vec![43]);
    let store = WorkspaceStore::new(&root).expect("absolute store root");
    store.save(&workspace_a).expect("alpha saves");
    store.save(&workspace_b).expect("beta saves");

    std::fs::remove_dir_all(directory_a).expect("alpha becomes unavailable");

    assert_eq!(
        store.load(&workspace_b.id).expect("beta metadata loads"),
        Some(workspace_b)
    );
    assert_eq!(
        store.load(&workspace_a.id).expect("alpha metadata loads"),
        Some(workspace_a)
    );
    assert!(root.join("workspace-profiles.v1.json").exists());
    assert!(!std::fs::read_dir(&root)
        .expect("store root reads")
        .any(|entry| entry
            .expect("valid entry")
            .file_name()
            .to_string_lossy()
            .contains("corrupt")));
}
#[test]
fn serializes_competing_stores_and_preserves_unique_sessions() {
    let root = temp_root("concurrent-save");
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join("workspace-profiles.v1.lock"))
        .expect("test lock opens");
    lock.lock_exclusive().expect("test lock acquired");
    let candidates = [("alpha", 51), ("beta", 53), ("gamma", 59), ("delta", 59)];
    let barrier = Arc::new(Barrier::new(candidates.len()));
    let (sender, receiver) = mpsc::channel();
    let handles = candidates
        .into_iter()
        .map(|(id, session)| {
            let root = root.clone();
            let barrier = Arc::clone(&barrier);
            let sender = sender.clone();
            thread::spawn(move || {
                let workspace = profile(id, root.clone(), vec![session]);
                barrier.wait();
                let saved = WorkspaceStore::new(&root)
                    .expect("store opens")
                    .save(&workspace)
                    .is_ok();
                sender.send((workspace, saved)).expect("result sends");
            })
        })
        .collect::<Vec<_>>();
    drop(sender);
    assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
    FileExt::unlock(&lock).expect("test lock released");
    let results = receiver.into_iter().collect::<Vec<_>>();
    for handle in handles {
        handle.join().expect("save thread completes");
    }

    assert_eq!(results.iter().filter(|(_, saved)| *saved).count(), 3);
    let store = WorkspaceStore::new(&root).expect("store opens");
    for (workspace, saved) in results {
        assert_eq!(
            store.load(&workspace.id).expect("metadata loads").is_some(),
            saved
        );
    }
}
#[test]
fn rejects_relative_directories_secret_values_and_zero_session_ids() {
    let root = temp_root("validation");
    let id = WorkspaceId::new("safe").expect("valid id");
    let environment = EnvironmentMetadata::new("development", ["PATH"]).expect("safe metadata");
    let agent = AgentDescriptor::new("codex", "codex").expect("safe agent");
    assert!(WorkspaceProfile::new(
        id.clone(),
        "safe",
        PathBuf::from("relative"),
        environment.clone(),
        agent.clone(),
        vec![1]
    )
    .is_err());
    assert!(EnvironmentMetadata::new("development", ["API_TOKEN=top-secret"]).is_err());
    assert!(WorkspaceProfile::new(id, "safe", root, environment, agent, vec![0]).is_err());
}
#[test]
fn replaces_an_existing_store_and_recovers_its_backup() {
    let root = temp_root("atomic");
    let store = WorkspaceStore::new(&root).expect("absolute store root");
    let first = profile("alpha", root.clone(), vec![3]);
    let second = profile("beta", root.clone(), vec![7]);
    store.save(&first).expect("first workspace saves");
    store.save(&second).expect("replacement workspace saves");
    assert_eq!(
        store.load(&second.id).expect("replacement loads"),
        Some(second)
    );
    let path = root.join("workspace-profiles.v1.json");
    let backup = root.join("workspace-profiles.v1.json.bak");
    assert!(std::fs::read_to_string(&backup)
        .expect("backup exists")
        .contains("alpha"));
    std::fs::remove_file(path).expect("simulate interruption after backup");
    assert_eq!(store.load(&first.id).expect("backup recovers"), Some(first));
}
#[test]
fn quarantines_partial_data_then_recovers_with_a_new_workspace() {
    let root = temp_root("corrupt");
    let path = root.join("workspace-profiles.v1.json");
    std::fs::write(&path, r#"{"schema_version":1,"profiles":"partial""#).expect("partial store");
    let store = WorkspaceStore::new(&root).expect("absolute store root");
    let error = store
        .load(&WorkspaceId::new("alpha").expect("valid id"))
        .expect_err("partial data must not be loaded");
    assert!(matches!(error, splice_core::WorkspaceError::Quarantined(ref path) if path.exists()));
    let workspace = profile("alpha", root.clone(), vec![5]);
    store
        .save(&workspace)
        .expect("new store recovers after quarantine");
    assert_eq!(
        store.load(&workspace.id).expect("recovered load"),
        Some(workspace)
    );
}
#[test]
fn quarantines_persisted_duplicate_sessions_then_recovers_cleanly() {
    let root = temp_root("duplicate-session");
    let path = root.join("workspace-profiles.v1.json");
    let duplicate_profiles = duplicate_session_profiles(&root, 17);
    std::fs::write(
        &path,
        serde_json::to_vec(&duplicate_profiles).expect("valid forged store"),
    )
    .expect("forged store writes");
    let store = WorkspaceStore::new(&root).expect("absolute store root");
    let error = store
        .load(&WorkspaceId::new("alpha").expect("valid id"))
        .expect_err("duplicate sessions must not load");
    assert!(matches!(error, splice_core::WorkspaceError::Quarantined(ref path) if path.exists()));
    assert!(!path.exists(), "duplicate store is moved out of service");

    let recovered = profile("gamma", root.clone(), vec![23]);
    store.save(&recovered).expect("clean store recovers");
    assert_eq!(
        store
            .load(&recovered.id)
            .expect("recovered workspace loads"),
        Some(recovered)
    );
}
#[test]
fn quarantines_duplicate_primary_then_recovers_the_valid_backup() {
    let root = temp_root("duplicate-session-backup");
    let store = WorkspaceStore::new(&root).expect("absolute store root");
    let backup_profile = profile("alpha", root.clone(), vec![29]);
    store.save(&backup_profile).expect("backup source saves");
    store
        .save(&profile("beta", root.clone(), vec![31]))
        .expect("replacement creates backup");

    let path = root.join("workspace-profiles.v1.json");
    let duplicate_profiles = duplicate_session_profiles(&root, 37);
    std::fs::write(
        &path,
        serde_json::to_vec(&duplicate_profiles).expect("valid forged store"),
    )
    .expect("forged primary writes");

    let error = store
        .load(&backup_profile.id)
        .expect_err("duplicate primary must not load");
    assert!(matches!(error, splice_core::WorkspaceError::Quarantined(ref path) if path.exists()));
    assert_eq!(
        store
            .load(&backup_profile.id)
            .expect("validated backup recovers"),
        Some(backup_profile)
    );
}
#[test]
fn rejects_unknown_schema_without_replacing_the_existing_store() {
    let root = temp_root("migration");
    let path = root.join("workspace-profiles.v1.json");
    std::fs::write(&path, r#"{"schema_version":2,"profiles":{}}"#).expect("future schema store");
    let store = WorkspaceStore::new(&root).expect("absolute store root");
    let error = store
        .load(&WorkspaceId::new("alpha").expect("valid id"))
        .expect_err("future schema must be rejected");
    assert!(matches!(
        error,
        splice_core::WorkspaceError::UnsupportedSchema(2)
    ));
    assert!(
        path.exists(),
        "a future schema must remain available to a compatible build"
    );
}

#[test]
fn loads_legacy_profiles_without_runtime_or_durable_intent_fields() {
    let root = temp_root("legacy-lifecycle-defaults");
    let canonical_root = std::fs::canonicalize(&root).unwrap();
    let path = root.join("workspace-profiles.v1.json");
    let legacy = serde_json::json!({
        "schema_version": 1,
        "profiles": {
            "alpha": {
                "id": "alpha",
                "name": "alpha",
                "working_directory": canonical_root,
                "environment": { "profile": "development", "variable_names": ["PATH"] },
                "agent": { "id": "codex", "command": "codex" },
                "session_ids": []
            }
        }
    });
    std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();

    assert_eq!(
        WorkspaceStore::new(&root)
            .unwrap()
            .load(&WorkspaceId::new("alpha").unwrap())
            .unwrap(),
        Some(profile("alpha", canonical_root, vec![]))
    );
}
