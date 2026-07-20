const TERMINAL_COMMANDS: &[&str] = &[
    "app_status",
    "active_paste_target",
    "preview_clipboard_image_paste",
    "preview_active_clipboard_image_paste",
    "pty_spawn",
    "pty_write",
    "pty_interrupt",
    "pty_resize",
    "pty_kill",
    "pty_ack",
    "clipboard_write_text",
    "clipboard_read_text",
    "open_path",
    "close_paste_session",
    "workspace_list",
    "workspace_create",
    "workspace_select",
    "workspace_update",
    "workspace_close",
    "workspace_restart",
    "workspace_recover",
];

fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(TERMINAL_COMMANDS)),
    )
    .expect("failed to build the finite terminal command authority");
}
