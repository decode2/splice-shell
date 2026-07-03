#[cfg(windows)]
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

#[cfg(windows)]
fn main() {
    let command = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "codex".to_owned());
    let output_path = std::env::args()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("target/{command}-pty-capture.raw")));

    let (sender, receiver) = mpsc::channel::<String>();
    let session = splice_pty::PtySession::spawn(
        "cmd.exe",
        &[
            "/D",
            "/Q",
            "/K",
            &format!("set PATH={};%PATH%", common_cli_path_prefix()),
        ],
        splice_pty::TerminalSize::new(120, 40).expect("valid terminal size"),
        move |_id, output| {
            let _ = sender.send(output);
        },
        |_id| {},
    )
    .expect("shell should start");

    session
        .write(&format!("{command}\r\n"))
        .expect("command should be accepted");

    let raw = collect_for(&receiver, Duration::from_secs(8));
    session.close();

    std::fs::write(&output_path, raw.as_bytes()).expect("capture should be written");

    println!("wrote {}", output_path.display());
    println!("bytes: {}", raw.len());
    println!("alt screen enter: {}", raw.matches("\x1b[?1049h").count());
    println!("alt screen exit: {}", raw.matches("\x1b[?1049l").count());
    println!("sync output enter: {}", raw.matches("\x1b[?2026h").count());
    println!("sync output exit: {}", raw.matches("\x1b[?2026l").count());
    println!("clear screen: {}", raw.matches("\x1b[2J").count());
    println!("cursor home: {}", raw.matches("\x1b[H").count());
    println!("visible preview:\n{}", visible_preview(&raw));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("capture_ai_cli is only supported on Windows");
}

#[cfg(windows)]
fn collect_for(receiver: &mpsc::Receiver<String>, duration: Duration) -> String {
    let deadline = Instant::now() + duration;
    let mut output = String::new();

    while Instant::now() < deadline {
        if let Ok(chunk) = receiver.recv_timeout(Duration::from_millis(100)) {
            output.push_str(&chunk);
        }
    }

    output
}

#[cfg(windows)]
fn visible_preview(output: &str) -> String {
    output
        .replace('\x1b', "\\x1b")
        .replace('\r', "\\r")
        .replace('\n', "\\n\n")
        .chars()
        .take(6000)
        .collect()
}

#[cfg(windows)]
fn common_cli_path_prefix() -> String {
    let user_profile = std::env::var("USERPROFILE").unwrap_or_default();
    let local_app_data = std::env::var("LOCALAPPDATA").unwrap_or_default();
    [
        path_if_present(format!("{user_profile}\\.local\\bin")),
        path_if_present(format!("{user_profile}\\scoop\\shims")),
        path_if_present(format!("{user_profile}\\scoop\\apps\\nodejs\\current\\bin")),
        path_if_present(format!("{local_app_data}\\agy\\bin")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(";")
}

#[cfg(windows)]
fn path_if_present(path: String) -> Option<String> {
    Path::new(&path).exists().then_some(path)
}
