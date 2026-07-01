use serde::Serialize;
use splice_core::{AdapterRegistry, PastePayload, PasteRoute};
use splice_pty::{PtySession, TerminalSize};
use std::{path::PathBuf, process::Command, sync::Mutex};
use tauri::{Emitter, State};

const PTY_OUTPUT_EVENT: &str = "pty-output";

struct PtyState {
    session: Mutex<Option<PtySession>>,
}

#[tauri::command]
fn app_status() -> String {
    "Splice Shell scaffold ready".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "camelCase")]
enum PastePreview {
    Ready {
        text: String,
        process_name: String,
        adapter_name: String,
    },
    UnsupportedImage {
        path: String,
        process_name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActivePasteTarget {
    process_name: String,
    adapter_name: Option<String>,
    supported: bool,
}

#[tauri::command]
fn preview_clipboard_image_paste(process_name: String) -> Result<PastePreview, String> {
    let payload = read_clipboard_image_paste_payload()?;

    Ok(preview_paste_payload(&process_name, &payload))
}

#[tauri::command]
fn active_paste_target(state: State<'_, PtyState>) -> Result<ActivePasteTarget, String> {
    let process_name = active_pty_process_name(&state)?;
    Ok(active_paste_target_for_process(&process_name))
}

#[tauri::command]
fn preview_active_clipboard_image_paste(
    state: State<'_, PtyState>,
) -> Result<PastePreview, String> {
    let process_name = active_pty_process_name(&state)?;
    let payload = read_clipboard_image_paste_payload()?;

    Ok(preview_paste_payload(&process_name, &payload))
}

#[tauri::command]
fn pty_spawn(
    app: tauri::AppHandle,
    state: State<'_, PtyState>,
    cols: u16,
    rows: u16,
    program: Option<String>,
    args: Option<Vec<String>>,
) -> Result<(), String> {
    let size = TerminalSize::new(cols, rows).map_err(|error| format!("{error:?}"))?;
    let command = resolve_pty_command(program, args);
    let command_args = command.args.iter().map(String::as_str).collect::<Vec<_>>();

    if let Some(previous_session) = state
        .session
        .lock()
        .map_err(|_| "PTY state lock poisoned".to_owned())?
        .take()
    {
        previous_session.close();
    }

    let session = PtySession::spawn(&command.program, &command_args, size, move |output| {
        let _ = app.emit(PTY_OUTPUT_EVENT, output);
    })
    .map_err(|error| error.to_string())?;

    let mut guard = state
        .session
        .lock()
        .map_err(|_| "PTY state lock poisoned".to_owned())?;
    *guard = Some(session);

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PtyCommand {
    program: String,
    args: Vec<String>,
}

fn resolve_pty_command(program: Option<String>, args: Option<Vec<String>>) -> PtyCommand {
    match program {
        Some(program) if !program.trim().is_empty() => PtyCommand {
            program,
            args: args.unwrap_or_default(),
        },
        _ => PtyCommand {
            program: "cmd.exe".to_owned(),
            args: default_shell_args(),
        },
    }
}

fn default_shell_args() -> Vec<String> {
    vec![
        "/D".to_owned(),
        "/K".to_owned(),
        format!("set PATH={};%PATH%", common_cli_path_prefix()),
    ]
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

#[tauri::command]
fn pty_write(state: State<'_, PtyState>, data: String) -> Result<(), String> {
    let mut guard = state
        .session
        .lock()
        .map_err(|_| "PTY state lock poisoned".to_owned())?;
    let Some(session) = guard.as_ref() else {
        return Err("PTY session is not running".to_owned());
    };

    match session.write(&data) {
        Ok(()) => Ok(()),
        Err(error) if error.is_terminal_closed() => {
            if let Some(closed_session) = guard.take() {
                closed_session.close();
            }
            Err("PTY session closed; start a new terminal session".to_owned())
        }
        Err(error) => Err(error.to_string()),
    }
}

#[tauri::command]
fn pty_interrupt(state: State<'_, PtyState>) -> Result<(), String> {
    with_pty_session(&state, |session| session.interrupt())
}

#[tauri::command]
fn pty_resize(state: State<'_, PtyState>, cols: u16, rows: u16) -> Result<(), String> {
    let size = TerminalSize::new(cols, rows).map_err(|error| format!("{error:?}"))?;
    with_pty_session(&state, |session| session.resize(size))
}

#[tauri::command]
fn pty_read(state: State<'_, PtyState>) -> Result<Vec<String>, String> {
    {
        let mut guard = state
            .session
            .lock()
            .map_err(|_| "PTY state lock poisoned".to_owned())?;
        if guard
            .as_ref()
            .map(PtySession::is_running)
            .transpose()
            .map_err(|error| error.to_string())?
            == Some(false)
        {
            if let Some(session) = guard.take() {
                session.close();
            }
            return Err("PTY session closed; start a new terminal session".to_owned());
        }
    }

    Ok(Vec::new())
}

#[tauri::command]
fn pty_kill(state: State<'_, PtyState>) -> Result<(), String> {
    let mut guard = state
        .session
        .lock()
        .map_err(|_| "PTY state lock poisoned".to_owned())?;
    if let Some(session) = guard.take() {
        session.close();
    }

    Ok(())
}

#[tauri::command]
fn open_path(path: String) -> Result<(), String> {
    let path = PathBuf::from(path);
    if !path.exists() {
        return Err(format!("Path does not exist: {}", path.display()));
    }

    Command::new("explorer.exe")
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Failed to open path: {error}"))
}

fn with_pty_session<F>(state: &State<'_, PtyState>, operation: F) -> Result<(), String>
where
    F: FnOnce(&PtySession) -> Result<(), splice_pty::PtyError>,
{
    let guard = state
        .session
        .lock()
        .map_err(|_| "PTY state lock poisoned".to_owned())?;
    let session = guard
        .as_ref()
        .ok_or_else(|| "PTY session is not running".to_owned())?;

    operation(session).map_err(|error| error.to_string())
}

fn active_pty_process_name(state: &State<'_, PtyState>) -> Result<String, String> {
    let guard = state
        .session
        .lock()
        .map_err(|_| "PTY state lock poisoned".to_owned())?;

    let registry = AdapterRegistry::with_builtin_adapters();
    let candidates = guard
        .as_ref()
        .map(PtySession::active_process_candidates)
        .transpose()
        .map_err(|error| error.to_string())?
        .unwrap_or_else(|| vec!["cmd.exe".to_owned()]);

    Ok(select_process_for_adapter(&registry, &candidates)
        .unwrap_or("cmd.exe")
        .to_owned())
}

fn read_clipboard_image_paste_payload() -> Result<PastePayload, String> {
    let temp_dir = std::env::temp_dir().join("splice-shell").join("clipboard");
    splice_clipboard::read_clipboard_image_paste_payload(&temp_dir)
        .map_err(|error| error.to_string())
}

fn preview_paste_payload(process_name: &str, payload: &PastePayload) -> PastePreview {
    let registry = AdapterRegistry::with_builtin_adapters();
    let adapter_name = registry.adapter_name_for_process(process_name);

    match registry.route_paste(process_name, payload) {
        PasteRoute::Text(text) => PastePreview::Ready {
            text,
            process_name: process_name.to_owned(),
            adapter_name: adapter_name.unwrap_or("text-passthrough").to_owned(),
        },
        PasteRoute::UnsupportedImage { path } => PastePreview::UnsupportedImage {
            path,
            process_name: process_name.to_owned(),
        },
    }
}

fn active_paste_target_for_process(process_name: &str) -> ActivePasteTarget {
    let registry = AdapterRegistry::with_builtin_adapters();
    let adapter_name = registry
        .adapter_name_for_process(process_name)
        .map(str::to_owned);

    ActivePasteTarget {
        process_name: process_name.to_owned(),
        supported: adapter_name.is_some(),
        adapter_name,
    }
}

fn select_process_for_adapter<'a>(
    registry: &AdapterRegistry,
    process_candidates: &'a [String],
) -> Option<&'a str> {
    process_candidates
        .iter()
        .find(|process_name| registry.adapter_name_for_process(process_name).is_some())
        .or_else(|| process_candidates.first())
        .map(String::as_str)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(PtyState {
            session: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![
            app_status,
            active_paste_target,
            preview_clipboard_image_paste,
            preview_active_clipboard_image_paste,
            pty_spawn,
            pty_write,
            pty_interrupt,
            pty_resize,
            pty_read,
            pty_kill,
            open_path
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Splice Shell desktop app");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_status_describes_scaffold_state() {
        assert_eq!(app_status(), "Splice Shell scaffold ready");
    }

    #[test]
    fn preview_paste_payload_returns_text_for_supported_cli() {
        let payload = PastePayload::Image(splice_core::ImagePaste {
            path: "C:/Temp/splice/image.bmp".to_owned(),
            mime_type: "image/bmp".to_owned(),
        });

        assert_eq!(
            preview_paste_payload("codex.exe", &payload),
            PastePreview::Ready {
                text: "Image file: C:/Temp/splice/image.bmp\r".to_owned(),
                process_name: "codex.exe".to_owned(),
                adapter_name: "codex-cli".to_owned()
            }
        );
    }

    #[test]
    fn preview_paste_payload_refuses_unknown_image_process() {
        let payload = PastePayload::Image(splice_core::ImagePaste {
            path: "C:/Temp/splice/image.bmp".to_owned(),
            mime_type: "image/bmp".to_owned(),
        });

        assert_eq!(
            preview_paste_payload("unknown.exe", &payload),
            PastePreview::UnsupportedImage {
                path: "C:/Temp/splice/image.bmp".to_owned(),
                process_name: "unknown.exe".to_owned()
            }
        );
    }

    #[test]
    fn pty_state_starts_empty() {
        let state = PtyState {
            session: Mutex::new(None),
        };

        assert!(state
            .session
            .lock()
            .expect("state lock should work")
            .is_none());
    }

    #[test]
    fn resolve_pty_command_uses_safe_default_shell() {
        assert_eq!(
            resolve_pty_command(None, None),
            PtyCommand {
                program: "cmd.exe".to_owned(),
                args: default_shell_args(),
            }
        );
    }

    #[test]
    fn default_shell_path_includes_common_cli_locations() {
        let path_prefix = common_cli_path_prefix();

        assert!(path_prefix.contains(".local\\bin"));
        assert!(path_prefix.contains("scoop\\shims"));
        assert!(path_prefix.contains("agy\\bin"));
    }

    #[test]
    fn resolve_pty_command_accepts_configured_program() {
        assert_eq!(
            resolve_pty_command(
                Some("codex.exe".to_owned()),
                Some(vec!["--help".to_owned()])
            ),
            PtyCommand {
                program: "codex.exe".to_owned(),
                args: vec!["--help".to_owned()],
            }
        );
    }

    #[test]
    fn select_process_for_adapter_prefers_supported_parent_over_unsupported_leaf() {
        let registry = AdapterRegistry::with_builtin_adapters();
        let candidates = vec![
            "node.exe".to_owned(),
            "codex.exe".to_owned(),
            "cmd.exe".to_owned(),
        ];

        assert_eq!(
            select_process_for_adapter(&registry, &candidates),
            Some("codex.exe")
        );
    }

    #[test]
    fn active_paste_target_reports_adapter_support() {
        assert_eq!(
            active_paste_target_for_process("codex.exe"),
            ActivePasteTarget {
                process_name: "codex.exe".to_owned(),
                adapter_name: Some("codex-cli".to_owned()),
                supported: true,
            }
        );

        assert_eq!(
            active_paste_target_for_process("unknown.exe"),
            ActivePasteTarget {
                process_name: "unknown.exe".to_owned(),
                adapter_name: None,
                supported: false,
            }
        );
    }

    #[test]
    fn open_path_rejects_missing_paths() {
        let missing_path = std::env::temp_dir().join("splice-shell-missing-open-path-file.png");
        let _ = std::fs::remove_file(&missing_path);

        let error = open_path(missing_path.display().to_string())
            .expect_err("missing paths should not be opened");

        assert!(error.contains("Path does not exist"));
    }
}
