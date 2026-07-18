use super::{linux, ShellCommand};
use std::path::Path;
pub(super) fn shell() -> ShellCommand {
    linux::shell()
}
pub(super) fn reveal(path: &Path) -> ShellCommand {
    linux::reveal(path)
}
