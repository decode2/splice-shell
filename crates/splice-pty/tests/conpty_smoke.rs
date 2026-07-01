#[cfg(windows)]
#[test]
fn conpty_runs_command_and_captures_output() {
    let output = splice_pty::run_conpty_command_with_input(
        "cmd.exe",
        &["/Q", "/K"],
        "echo splice-conpty-proof\r\nexit\r\n",
        splice_pty::TerminalSize::new(80, 24).expect("valid terminal size"),
    )
    .expect("ConPTY smoke command should run");

    assert!(
        output.contains("splice-conpty-proof"),
        "expected ConPTY output buffer to contain proof marker, got: {output:?}"
    );
}

#[cfg(windows)]
#[test]
fn conpty_resizes_before_command_input() {
    let output = splice_pty::run_conpty_command_with_resize(
        "cmd.exe",
        &["/Q", "/K"],
        "echo splice-conpty-resize-proof\r\nexit\r\n",
        splice_pty::TerminalSize::new(80, 24).expect("valid initial terminal size"),
        splice_pty::TerminalSize::new(100, 30).expect("valid resized terminal size"),
    )
    .expect("ConPTY resize smoke command should run");

    assert!(
        output.contains("splice-conpty-resize-proof"),
        "expected resized ConPTY output buffer to contain proof marker, got: {output:?}"
    );
}

#[cfg(windows)]
#[test]
fn live_pty_session_writes_input_and_streams_output() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let (sender, receiver) = mpsc::channel::<String>();
    let session = splice_pty::PtySession::spawn(
        "cmd.exe",
        &["/D", "/K"],
        splice_pty::TerminalSize::new(80, 24).expect("valid terminal size"),
        move |output| {
            let _ = sender.send(output);
        },
    )
    .expect("live ConPTY session should start");

    session
        .write("echo splice-live-proof\r\nexit\r\n")
        .expect("live ConPTY session should accept input");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut output = String::new();
    while Instant::now() < deadline && !output.contains("splice-live-proof") {
        if let Ok(chunk) = receiver.recv_timeout(Duration::from_millis(250)) {
            output.push_str(&chunk);
        }
    }

    session.close();

    assert!(
        output.contains("splice-live-proof"),
        "expected live PTY output callback to contain proof marker, got: {output:?}"
    );
}

#[cfg(windows)]
#[test]
fn live_pty_session_forwards_arrow_up_to_shell_history() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let (sender, receiver) = mpsc::channel::<String>();
    let session = splice_pty::PtySession::spawn(
        "cmd.exe",
        &["/D", "/K"],
        splice_pty::TerminalSize::new(80, 24).expect("valid terminal size"),
        move |output| {
            let _ = sender.send(output);
        },
    )
    .expect("live ConPTY session should start");

    session
        .write("echo splice-history-first\r\n")
        .expect("first command should be accepted");
    session
        .write("echo splice-history-second\r\n")
        .expect("second command should be accepted");
    session
        .write("\x1b[A\r\nexit\r\n")
        .expect("arrow-up history command should be accepted");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut output = String::new();
    while Instant::now() < deadline && output.matches("splice-history-second").count() < 2 {
        if let Ok(chunk) = receiver.recv_timeout(Duration::from_millis(250)) {
            output.push_str(&chunk);
        }
    }

    session.close();

    assert!(
        output.matches("splice-history-second").count() >= 2,
        "expected ArrowUp+Enter to re-run the second command, got: {output:?}"
    );
}

#[cfg(windows)]
#[test]
fn live_pty_ctrl_c_interrupts_command_without_closing_shell() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let (sender, receiver) = mpsc::channel::<String>();
    let session = splice_pty::PtySession::spawn(
        "cmd.exe",
        &["/D", "/K"],
        splice_pty::TerminalSize::new(80, 24).expect("valid terminal size"),
        move |output| {
            let _ = sender.send(output);
        },
    )
    .expect("live ConPTY session should start");

    session
        .write("ping -t 127.0.0.1\r\n")
        .expect("long-running command should start");
    std::thread::sleep(Duration::from_millis(500));
    session
        .write("\x03")
        .expect("Ctrl+C should be accepted by the PTY");
    std::thread::sleep(Duration::from_millis(500));
    session
        .write("echo splice-after-interrupt\r\nexit\r\n")
        .expect("shell should remain writable after Ctrl+C");

    let deadline = Instant::now() + Duration::from_secs(8);
    let mut output = String::new();
    while Instant::now() < deadline && !output.contains("splice-after-interrupt") {
        if let Ok(chunk) = receiver.recv_timeout(Duration::from_millis(250)) {
            output.push_str(&chunk);
        }
    }

    session.close();

    assert!(
        output.contains("splice-after-interrupt"),
        "expected Ctrl+C to interrupt only the running command and keep shell alive, got: {output:?}"
    );
}

#[cfg(windows)]
#[test]
fn conpty_default_shell_can_find_user_cli_paths_when_prefixed() {
    let user_profile = std::env::var("USERPROFILE").expect("USERPROFILE should be set");
    let local_app_data = std::env::var("LOCALAPPDATA").expect("LOCALAPPDATA should be set");
    let path_prefix = [
        format!("{user_profile}\\.local\\bin"),
        format!("{user_profile}\\scoop\\shims"),
        format!("{user_profile}\\scoop\\apps\\nodejs\\current\\bin"),
        format!("{local_app_data}\\agy\\bin"),
    ]
    .join(";");
    let output = splice_pty::run_conpty_command_with_input(
        "cmd.exe",
        &["/D", "/K", &format!("set PATH={path_prefix};%PATH%")],
        "where claude\r\nwhere codex\r\nwhere agy\r\nexit\r\n",
        splice_pty::TerminalSize::new(100, 30).expect("valid terminal size"),
    )
    .expect("ConPTY shell should run with prefixed PATH");

    assert!(
        output.contains("claude.exe"),
        "expected prefixed PATH to find claude, got: {output:?}"
    );
    assert!(
        output.contains("codex"),
        "expected prefixed PATH to find codex, got: {output:?}"
    );
    assert!(
        output.contains("agy.exe"),
        "expected prefixed PATH to find agy, got: {output:?}"
    );
}
