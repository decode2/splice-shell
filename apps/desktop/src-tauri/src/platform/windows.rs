use super::ShellCommand;
use std::path::Path;
pub(super) fn shell() -> ShellCommand {
    ShellCommand::new("cmd.exe", ["/D", "/K"])
}
pub(super) fn reveal(path: &Path) -> ShellCommand {
    ShellCommand::new("explorer.exe", [format!("/select,{}", path.display())])
}
