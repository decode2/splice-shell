mod linux;
mod windows;
mod wsl;

use serde::Serialize;
use std::{path::Path, process::Command};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PlatformTarget {
    Windows,
    NativeUbuntu,
    Wsl2Wslg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformErrorCode {
    InvalidPath,
    MissingPath,
    MissingEnvironment,
    NativeMechanismFailed,
    UnsupportedTarget,
    WslgUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformError {
    pub code: PlatformErrorCode,
    pub message: String,
    pub platform: Option<PlatformTarget>,
    pub retryable: bool,
}

impl PlatformError {
    fn new(code: PlatformErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            platform: None,
            retryable,
        }
    }
    fn target(
        target: PlatformTarget,
        code: PlatformErrorCode,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            platform: Some(target),
            retryable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl ShellCommand {
    fn new(program: impl Into<String>, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }
    }
}

pub struct PlatformFacts {
    pub os: String,
    pub ubuntu: Option<String>,
    pub wsl: Option<String>,
    pub wslg: bool,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformServices {
    target: PlatformTarget,
    path: String,
}

impl PlatformServices {
    pub fn detect() -> Result<Self, PlatformError> {
        Self::from_facts(PlatformFacts {
            os: std::env::consts::OS.into(),
            ubuntu: ubuntu_version(),
            wsl: std::env::var("WSL_DISTRO_NAME").ok(),
            wslg: std::env::var_os("WAYLAND_DISPLAY").is_some()
                || std::env::var_os("DISPLAY").is_some(),
            path: std::env::var("PATH").ok(),
        })
    }
    pub fn from_facts(facts: PlatformFacts) -> Result<Self, PlatformError> {
        let target = if facts.os == "windows" {
            PlatformTarget::Windows
        } else if facts.os != "linux" {
            return Err(PlatformError::new(
                PlatformErrorCode::UnsupportedTarget,
                format!("{} is not a supported desktop target", facts.os),
                false,
            ));
        } else if facts.wsl.is_some() {
            if !facts.wslg {
                return Err(PlatformError::new(
                    PlatformErrorCode::WslgUnavailable,
                    "WSL requires a usable WSLg display or Wayland socket",
                    true,
                ));
            }
            PlatformTarget::Wsl2Wslg
        } else if matches!(facts.ubuntu.as_deref(), Some("22.04" | "24.04")) {
            PlatformTarget::NativeUbuntu
        } else {
            return Err(PlatformError::new(
                PlatformErrorCode::UnsupportedTarget,
                "only Ubuntu 22.04 and 24.04 are supported on Linux",
                false,
            ));
        };
        let path = facts.path.filter(|path| !path.is_empty()).ok_or_else(|| {
            PlatformError::target(
                target,
                PlatformErrorCode::MissingEnvironment,
                "PATH is required for this platform target",
                true,
            )
        })?;
        Ok(Self { target, path })
    }
    pub fn target(&self) -> PlatformTarget {
        self.target
    }
    pub fn shell(&self) -> Result<ShellCommand, PlatformError> {
        Ok(match self.target {
            PlatformTarget::Windows => windows::shell(),
            PlatformTarget::NativeUbuntu => linux::shell(),
            PlatformTarget::Wsl2Wslg => wsl::shell(),
        })
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn reveal_command(&self, path: impl AsRef<Path>) -> Result<ShellCommand, PlatformError> {
        let path = path.as_ref();
        if !path.is_absolute() {
            return Err(PlatformError::target(
                self.target,
                PlatformErrorCode::InvalidPath,
                "only absolute paths can be revealed",
                false,
            ));
        }
        if !path.exists() {
            return Err(PlatformError::target(
                self.target,
                PlatformErrorCode::MissingPath,
                format!("path does not exist: {}", path.display()),
                false,
            ));
        }
        Ok(match self.target {
            PlatformTarget::Windows => windows::reveal(path),
            PlatformTarget::NativeUbuntu => linux::reveal(path),
            PlatformTarget::Wsl2Wslg => wsl::reveal(path),
        })
    }
    pub fn reveal(&self, path: impl AsRef<Path>) -> Result<(), PlatformError> {
        let command = self.reveal_command(path)?;
        Command::new(command.program)
            .args(command.args)
            .spawn()
            .map(|_| ())
            .map_err(|error| {
                PlatformError::target(
                    self.target,
                    PlatformErrorCode::NativeMechanismFailed,
                    format!("failed to reveal path: {error}"),
                    true,
                )
            })
    }
}

fn ubuntu_version() -> Option<String> {
    let release = std::fs::read_to_string("/etc/os-release").ok()?;
    let id = release.lines().find(|line| line.starts_with("ID="))?;
    let version = release
        .lines()
        .find(|line| line.starts_with("VERSION_ID="))?;
    (id.trim_start_matches("ID=").trim_matches('"') == "ubuntu").then(|| {
        version
            .trim_start_matches("VERSION_ID=")
            .trim_matches('"')
            .into()
    })
}
