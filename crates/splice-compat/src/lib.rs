use serde::Serialize;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{collections::BTreeMap, ffi::OsStr};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Verified,
    Qualified,
    Fallback,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceConfidence {
    Unevidenced,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CliDescriptor {
    pub id: &'static str,
    pub command: Option<&'static str>,
    pub tier: Tier,
}
const DESCRIPTORS: [CliDescriptor; 7] = [
    descriptor("codex", Some("codex")),
    descriptor("claude", Some("claude")),
    descriptor("opencode", Some("opencode")),
    descriptor("gemini", Some("gemini")),
    descriptor("aider", Some("aider")),
    descriptor("agy", Some("agy")),
    descriptor("generic-tui", None),
];
const fn descriptor(id: &'static str, command: Option<&'static str>) -> CliDescriptor {
    CliDescriptor {
        id,
        command,
        tier: Tier::Fallback,
    }
}
pub fn descriptors() -> &'static [CliDescriptor] {
    &DESCRIPTORS
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Behavior {
    Startup,
    Input,
    Interrupt,
    Liveness,
    ImagePaste,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "status")]
pub enum Probe {
    Passed,
    Skipped { reason: String },
    Unsupported { reason: String },
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    available: bool,
    version: Option<String>,
    prerequisites: Vec<String>,
    skip: Option<String>,
}
impl Observation {
    pub fn available<I, S>(version: impl Into<String>, prerequisites: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            available: true,
            version: Some(version.into()),
            prerequisites: prerequisites.into_iter().map(Into::into).collect(),
            skip: None,
        }
    }
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self {
            available: false,
            version: None,
            prerequisites: Vec::new(),
            skip: Some(reason.into()),
        }
    }
}
pub fn discover(
    command: &str,
    path: &OsStr,
    version: Option<String>,
    prerequisites: Vec<String>,
) -> Observation {
    let found = std::env::split_paths(path).any(|directory| command_exists(&directory, command));
    if found {
        Observation {
            available: true,
            version,
            prerequisites,
            skip: None,
        }
    } else {
        Observation::unavailable(format!("{command} not found on PATH"))
    }
}
fn command_exists(directory: &std::path::Path, command: &str) -> bool {
    #[cfg(windows)]
    {
        if std::path::Path::new(command).extension().is_some() {
            return directory.join(command).is_file();
        }
        let extensions =
            std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
        extensions
            .split(';')
            .any(|extension| directory.join(format!("{command}{extension}")).is_file())
    }
    #[cfg(unix)]
    {
        directory
            .join(command)
            .metadata()
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CapabilityReport {
    pub descriptor: CliDescriptor,
    pub tier: Tier,
    pub os: &'static str,
    pub shell: &'static str,
    pub version: Option<String>,
    pub prerequisites: Vec<String>,
    pub constraints: Vec<String>,
    pub evidence_confidence: EvidenceConfidence,
    pub skip: Option<String>,
    pub probes: BTreeMap<Behavior, Probe>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatrixReport {
    pub rows: Vec<CapabilityReport>,
}
impl MatrixReport {
    pub fn json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
    pub fn human(&self) -> String {
        let tier = |tier| match tier {
            Tier::Verified => "verified",
            Tier::Qualified => "qualified",
            Tier::Fallback => "fallback",
        };
        self.rows
            .iter()
            .map(|row| {
                format!(
                    "{} tier={} os={} shell={} version={} prerequisites={:?} constraints={:?} confidence={:?} skip={} probes={:?}",
                    row.descriptor.id,
                    tier(row.tier),
                    row.os,
                    row.shell,
                    row.version.as_deref().unwrap_or("unknown"),
                    row.prerequisites,
                    row.constraints,
                    row.evidence_confidence,
                    row.skip.as_deref().unwrap_or("none"),
                    row.probes
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
pub fn matrix(observations: &BTreeMap<String, Observation>) -> MatrixReport {
    MatrixReport {
        rows: descriptors()
            .iter()
            .cloned()
            .map(|descriptor| {
                let observation = observations
                    .get(descriptor.id)
                    .cloned()
                    .unwrap_or_else(|| Observation::unavailable("no runtime evidence collected"));
                CapabilityReport {
                    tier: Tier::Fallback,
                    descriptor,
                    os: "unknown",
                    shell: "unknown",
                    version: observation.version,
                    prerequisites: observation.prerequisites,
                    constraints: vec!["runtime behavior is not evidenced".to_owned()],
                    evidence_confidence: EvidenceConfidence::Unevidenced,
                    skip: observation
                        .skip
                        .or_else(|| (!observation.available).then(|| "not available".to_owned())),
                    probes: unevidenced_probes(),
                }
            })
            .collect(),
    }
}
pub fn harness(
    path: &OsStr,
    version: Option<String>,
    prerequisites: Vec<String>,
    tui: FakeTui,
) -> MatrixReport {
    let observations = descriptors()
        .iter()
        .filter_map(|descriptor| {
            descriptor.command.map(|command| {
                (
                    descriptor.id.to_owned(),
                    discover(command, path, version.clone(), prerequisites.clone()),
                )
            })
        })
        .collect();
    let probes = tui.probe();
    let mut report = matrix(&observations);
    for row in &mut report.rows {
        if observations
            .get(row.descriptor.id)
            .is_some_and(|observation| observation.available)
        {
            row.probes.extend(probes.clone());
        }
    }
    report
}
fn unevidenced_probes() -> BTreeMap<Behavior, Probe> {
    let skipped = || Probe::Skipped {
        reason: "no runtime evidence".to_owned(),
    };
    [
        (Behavior::Startup, skipped()),
        (Behavior::Input, skipped()),
        (Behavior::Interrupt, skipped()),
        (Behavior::Liveness, skipped()),
        (
            Behavior::ImagePaste,
            Probe::Unsupported {
                reason: "no evidence".to_owned(),
            },
        ),
    ]
    .into_iter()
    .collect()
}
pub struct FakeTui([bool; 4]);
impl FakeTui {
    pub fn new(startup: bool, input: bool, interrupt: bool, liveness: bool) -> Self {
        Self([startup, input, interrupt, liveness])
    }
    pub fn probe(self) -> BTreeMap<Behavior, Probe> {
        [
            (Behavior::Startup, "fake TUI did not start"),
            (Behavior::Input, "fake TUI did not accept input"),
            (
                Behavior::Interrupt,
                "fake TUI did not acknowledge interrupt",
            ),
            (Behavior::Liveness, "fake TUI was not live"),
        ]
        .into_iter()
        .zip(self.0)
        .map(|((behavior, reason), passed)| {
            (
                behavior,
                if passed {
                    Probe::Passed
                } else {
                    Probe::Skipped {
                        reason: reason.to_owned(),
                    }
                },
            )
        })
        .collect()
    }
}
