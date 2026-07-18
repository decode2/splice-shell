use super::ShellCommand;
use std::path::Path;
pub(super) fn shell() -> ShellCommand {
    ShellCommand::new("/bin/sh", std::iter::empty::<String>())
}
pub(super) fn reveal(path: &Path) -> ShellCommand {
    ShellCommand::new(
        "xdg-open",
        [path.parent().unwrap_or(path).to_string_lossy().into_owned()],
    )
}
