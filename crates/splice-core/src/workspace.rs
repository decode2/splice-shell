use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
const SCHEMA_VERSION: u32 = 1;
const STORE_FILE: &str = "workspace-profiles.v1.json";
const LOCK_FILE: &str = "workspace-profiles.v1.lock";
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(String);
impl WorkspaceId {
    pub fn new(value: impl Into<String>) -> Result<Self, WorkspaceError> {
        let value = value.into();
        valid_label(&value)
            .then_some(Self(value))
            .ok_or(WorkspaceError::InvalidWorkspaceId)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDescriptor {
    pub id: String,
    pub command: String,
}
impl AgentDescriptor {
    pub fn new(id: impl Into<String>, command: impl Into<String>) -> Result<Self, WorkspaceError> {
        let (id, command) = (id.into(), command.into());
        if valid_label(&id) && valid_command(&command) {
            Ok(Self { id, command })
        } else {
            Err(WorkspaceError::InvalidProfile)
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentMetadata {
    pub profile: String,
    pub variable_names: Vec<String>,
}
impl EnvironmentMetadata {
    pub fn new(
        profile: impl Into<String>,
        names: impl IntoIterator<Item = impl Into<String>>,
    ) -> Result<Self, WorkspaceError> {
        let metadata = Self {
            profile: profile.into(),
            variable_names: names.into_iter().map(Into::into).collect(),
        };
        (valid_label(&metadata.profile)
            && metadata
                .variable_names
                .iter()
                .all(|name| valid_env_name(name)))
        .then_some(metadata)
        .ok_or(WorkspaceError::InvalidProfile)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceProfile {
    pub id: WorkspaceId,
    pub name: String,
    pub working_directory: PathBuf,
    pub environment: EnvironmentMetadata,
    pub agent: AgentDescriptor,
    pub session_ids: Vec<u64>,
}
impl WorkspaceProfile {
    pub fn new(
        id: WorkspaceId,
        name: impl Into<String>,
        directory: PathBuf,
        environment: EnvironmentMetadata,
        agent: AgentDescriptor,
        sessions: Vec<u64>,
    ) -> Result<Self, WorkspaceError> {
        let working_directory =
            fs::canonicalize(directory).map_err(|_| WorkspaceError::InvalidProfile)?;
        if !working_directory.is_dir() {
            return Err(WorkspaceError::InvalidProfile);
        }
        let session_ids = sessions.into_iter().collect::<BTreeSet<_>>();
        if session_ids.contains(&0) {
            return Err(WorkspaceError::InvalidProfile);
        }
        Ok(Self {
            id,
            name: name.into(),
            working_directory,
            environment,
            agent,
            session_ids: session_ids.into_iter().collect(),
        })
    }
}
#[derive(Debug)]
pub enum WorkspaceError {
    InvalidWorkspaceId,
    InvalidProfile,
    UnsupportedSchema(u32),
    Quarantined(PathBuf),
    Io,
    Serialization,
}
impl From<std::io::Error> for WorkspaceError {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}
#[derive(Serialize, Deserialize)]
struct Database {
    schema_version: u32,
    profiles: BTreeMap<WorkspaceId, WorkspaceProfile>,
}
pub struct WorkspaceStore {
    root: PathBuf,
}
impl WorkspaceStore {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let root = root.as_ref();
        root.is_absolute()
            .then_some(Self {
                root: root.to_owned(),
            })
            .ok_or(WorkspaceError::InvalidProfile)
    }

    pub fn save(&self, profile: &WorkspaceProfile) -> Result<(), WorkspaceError> {
        if !valid_profile(profile) || !profile.working_directory.is_dir() {
            return Err(WorkspaceError::InvalidProfile);
        }
        fs::create_dir_all(&self.root)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join(LOCK_FILE))?;
        lock.lock_exclusive()?;
        let mut database = self.read()?.unwrap_or(Database {
            schema_version: SCHEMA_VERSION,
            profiles: BTreeMap::new(),
        });
        database
            .profiles
            .insert(profile.id.clone(), profile.clone());
        if !valid_database(&database) {
            return Err(WorkspaceError::InvalidProfile);
        }
        self.write(&database)
    }

    pub fn load(&self, id: &WorkspaceId) -> Result<Option<WorkspaceProfile>, WorkspaceError> {
        Ok(self
            .read()?
            .and_then(|mut database| database.profiles.remove(id)))
    }

    fn path(&self) -> PathBuf {
        self.root.join(STORE_FILE)
    }

    fn backup_path(&self) -> PathBuf {
        self.root.join(format!("{STORE_FILE}.bak"))
    }

    fn read(&self) -> Result<Option<Database>, WorkspaceError> {
        let path = self.path();
        if !path.exists() {
            let backup = self.backup_path();
            return backup
                .exists()
                .then_some(backup)
                .map_or(Ok(None), |path| self.read_file(&path));
        }
        self.read_file(&path)
    }

    fn read_file(&self, path: &Path) -> Result<Option<Database>, WorkspaceError> {
        let bytes = fs::read(path)?;
        let database: Database = match serde_json::from_slice(&bytes) {
            Ok(database) => database,
            Err(_) => return Err(WorkspaceError::Quarantined(self.quarantine(path)?)),
        };
        if database.schema_version != SCHEMA_VERSION {
            return Err(WorkspaceError::UnsupportedSchema(database.schema_version));
        }
        if valid_database(&database) {
            Ok(Some(database))
        } else {
            Err(WorkspaceError::Quarantined(self.quarantine(path)?))
        }
    }

    fn write(&self, database: &Database) -> Result<(), WorkspaceError> {
        fs::create_dir_all(&self.root)?;
        let bytes = serde_json::to_vec(database).map_err(|_| WorkspaceError::Serialization)?;
        let temp = self.root.join(format!(
            ".{STORE_FILE}.{}-{}.tmp",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| WorkspaceError::Serialization)?
                .as_nanos()
        ));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        let path = self.path();
        if path.exists() {
            let backup = self.backup_path();
            if backup.exists() {
                fs::remove_file(&backup)?;
            }
            fs::rename(&path, backup)?;
        }
        fs::rename(&temp, path)?;
        let _ = File::open(&self.root).and_then(|directory| directory.sync_all());
        Ok(())
    }

    fn quarantine(&self, path: &Path) -> Result<PathBuf, WorkspaceError> {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| WorkspaceError::Serialization)?
            .as_nanos();
        let quarantine = self
            .root
            .join(format!("workspace-profiles.corrupt-{suffix}.json"));
        fs::rename(path, &quarantine)?;
        Ok(quarantine)
    }
}
fn valid_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}
fn valid_command(value: &str) -> bool {
    !value.is_empty() && !value.chars().any(char::is_whitespace)
}
fn valid_env_name(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}
fn valid_profile(profile: &WorkspaceProfile) -> bool {
    valid_label(&profile.id.0)
        && !profile.name.trim().is_empty()
        && profile.working_directory.is_absolute()
        && valid_label(&profile.environment.profile)
        && profile
            .environment
            .variable_names
            .iter()
            .all(|name| valid_env_name(name))
        && valid_label(&profile.agent.id)
        && valid_command(&profile.agent.command)
        && profile.session_ids.iter().all(|id| *id != 0)
        && profile.session_ids.iter().collect::<BTreeSet<_>>().len() == profile.session_ids.len()
}
fn valid_database(database: &Database) -> bool {
    let mut session_ids = BTreeSet::new();
    database.profiles.iter().all(|(id, profile)| {
        id == &profile.id
            && valid_profile(profile)
            && profile
                .session_ids
                .iter()
                .all(|session| session_ids.insert(session))
    })
}
