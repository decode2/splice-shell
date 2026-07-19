use super::ShellCommand;
use std::path::Path;
pub(super) fn shell() -> ShellCommand {
    ShellCommand::new(
        "cmd.exe",
        [
            "/D".to_owned(),
            "/K".to_owned(),
            format!("set PATH={};%PATH%", common_cli_path_prefix()),
        ],
    )
}
pub(super) fn reveal(path: &Path) -> ShellCommand {
    ShellCommand::new("explorer.exe", [format!("/select,{}", path.display())])
}

fn common_cli_path_prefix() -> String {
    let user_profile = std::env::var("USERPROFILE").unwrap_or_default();
    let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_default();

    [
        format!("{user_profile}\\.local\\bin"),
        format!("{user_profile}\\scoop\\shims"),
        format!("{user_profile}\\scoop\\apps\\nodejs\\current\\bin"),
        format!("{user_profile}\\scoop\\apps\\nodejs\\current"),
        format!("{local_app_data}\\agy\\bin"),
        format!("{local_app_data}\\Programs\\OpenCode\\bin"),
        format!("{local_app_data}\\Programs\\opencode\\bin"),
        format!("{local_app_data}\\OpenAI\\Codex\\bin"),
    ]
    .into_iter()
    .filter(|path| !path.starts_with('\\') && !path.is_empty())
    .collect::<Vec<_>>()
    .join(";")
}
