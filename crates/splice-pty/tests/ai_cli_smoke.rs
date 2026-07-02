#[cfg(windows)]
use std::{
    path::Path,
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

#[cfg(windows)]
#[derive(Debug)]
struct AiCliCase {
    command: &'static str,
    startup_markers: &'static [&'static str],
    allow_session_close_on_ctrl_c: bool,
}

#[cfg(windows)]
const AI_CLI_CASES: &[AiCliCase] = &[
    AiCliCase {
        command: "codex",
        startup_markers: &["gpt-", "Improve documentation", "Starting MCP servers"],
        allow_session_close_on_ctrl_c: false,
    },
    AiCliCase {
        command: "claude",
        startup_markers: &["Enter to confirm", "Esc to cancel"],
        allow_session_close_on_ctrl_c: false,
    },
    AiCliCase {
        command: "agy",
        startup_markers: &["Antigravity CLI", "Claude Sonnet", "? for shortcuts"],
        allow_session_close_on_ctrl_c: true,
    },
    AiCliCase {
        command: "opencode",
        startup_markers: &["1.17.12", "Build", "ctrl+p"],
        allow_session_close_on_ctrl_c: true,
    },
];

#[cfg(windows)]
#[test]
#[ignore = "requires locally installed/authenticated AI CLIs; run with RUN_AI_CLI_SMOKE=1"]
fn ai_cli_tuis_render_and_accept_control_input() {
    if std::env::var("RUN_AI_CLI_SMOKE").as_deref() != Ok("1") {
        eprintln!("set RUN_AI_CLI_SMOKE=1 to run local AI CLI smoke tests");
        return;
    }

    for case in AI_CLI_CASES {
        if !command_exists(case.command) {
            eprintln!(
                "skipping {} because it is not available on PATH",
                case.command
            );
            continue;
        }

        run_ai_cli_case(case);
    }
}

#[cfg(windows)]
fn run_ai_cli_case(case: &AiCliCase) {
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
        move |output| {
            let _ = sender.send(output);
        },
        |_id| {},
    )
    .unwrap_or_else(|error| panic!("{}: shell should start: {error}", case.command));

    session
        .write(&format!("{}\r\n", case.command))
        .unwrap_or_else(|error| panic!("{}: command should be accepted: {error}", case.command));

    let startup = collect_until(&receiver, Duration::from_secs(15), |output| {
        contains_any(output, case.startup_markers)
    });
    assert!(
        contains_any(&startup, case.startup_markers),
        "{}: expected startup output to contain one of {:?}, got:\n{}",
        case.command,
        case.startup_markers,
        visible(&startup)
    );

    session.write("\x1b[B").unwrap_or_else(|error| {
        panic!(
            "{}: normal ArrowDown should be accepted: {error}",
            case.command
        )
    });
    let mut after_arrows = collect_for(&receiver, Duration::from_secs(2));
    if after_arrows.is_empty() {
        session.write("\x1bOB").unwrap_or_else(|error| {
            panic!(
                "{}: application ArrowDown should be accepted: {error}",
                case.command
            )
        });
        after_arrows = collect_for(&receiver, Duration::from_secs(2));
    }
    assert!(
        !after_arrows.is_empty(),
        "{}: expected TUI to respond after normal/application ArrowDown; startup was:\n{}",
        case.command,
        visible(&startup)
    );

    session
        .write("\x03")
        .unwrap_or_else(|error| panic!("{}: Ctrl+C should be accepted: {error}", case.command));
    let mut after_ctrl_c = collect_for(&receiver, Duration::from_secs(3));
    if after_ctrl_c.contains("Ctrl-C again") || after_ctrl_c.contains("Ctrl+C again") {
        session.write("\x03").unwrap_or_else(|error| {
            panic!(
                "{}: second Ctrl+C should be accepted when requested by the CLI: {error}",
                case.command
            )
        });
        after_ctrl_c.push_str(&collect_for(&receiver, Duration::from_secs(3)));
    }
    after_ctrl_c.push_str(&collect_until(
        &receiver,
        Duration::from_secs(3),
        looks_like_cmd_prompt,
    ));

    if let Err(error) = session.write("echo splice-ai-cli-still-alive\r\n") {
        session.close();
        assert!(
            case.allow_session_close_on_ctrl_c,
            "{}: PTY should remain writable after Ctrl+C: {error}",
            case.command
        );
        return;
    }
    let after_probe = collect_until(&receiver, Duration::from_secs(5), |output| {
        output.contains("splice-ai-cli-still-alive")
    });
    let after_probe = if after_probe.contains("splice-ai-cli-still-alive") {
        after_probe
    } else if looks_like_cmd_prompt(&after_probe) || looks_like_cmd_prompt(&after_ctrl_c) {
        session
            .write("echo splice-ai-cli-still-alive\r\n")
            .unwrap_or_else(|error| {
                panic!(
                    "{}: second shell liveness probe should be accepted: {error}",
                    case.command
                )
            });
        collect_until(&receiver, Duration::from_secs(5), |output| {
            output.contains("splice-ai-cli-still-alive")
        })
    } else {
        after_probe
    };

    session.close();

    assert!(
        after_probe.contains("splice-ai-cli-still-alive"),
        "{}: expected shell to remain usable after Ctrl+C. Ctrl+C output:\n{}\nProbe output:\n{}",
        case.command,
        visible(&after_ctrl_c),
        visible(&after_probe)
    );
}

#[cfg(windows)]
fn collect_until(
    receiver: &Receiver<String>,
    timeout: Duration,
    done: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + timeout;
    let mut output = String::new();

    while Instant::now() < deadline && !done(&output) {
        if let Ok(chunk) = receiver.recv_timeout(Duration::from_millis(100)) {
            output.push_str(&chunk);
        }
    }

    output
}

#[cfg(windows)]
fn collect_for(receiver: &Receiver<String>, duration: Duration) -> String {
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
fn contains_any(output: &str, needles: &[&str]) -> bool {
    let text = terminal_text(output);
    let compact_output = compact_text(&text);
    needles
        .iter()
        .any(|needle| text.contains(needle) || compact_output.contains(&compact_text(needle)))
}

#[cfg(windows)]
fn looks_like_cmd_prompt(output: &str) -> bool {
    output.contains("\r\nC:\\") || output.contains("\nC:\\")
}

#[cfg(windows)]
fn visible(output: &str) -> String {
    output
        .replace('\x1b', "\\x1b")
        .replace('\r', "\\r")
        .replace('\n', "\\n\n")
}

#[cfg(windows)]
fn terminal_text(output: &str) -> String {
    let mut text = String::with_capacity(output.len());
    let mut chars = output.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            if !ch.is_control() || ch == '\n' || ch == '\r' {
                text.push(ch);
            }
            continue;
        }

        match chars.peek().copied() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\x1b' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }

    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(windows)]
fn compact_text(text: &str) -> String {
    text.chars().filter(|ch| !ch.is_whitespace()).collect()
}

#[cfg(windows)]
fn command_exists(command: &str) -> bool {
    std::process::Command::new("where.exe")
        .arg(command)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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
