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
    // A prefixed PATH must be honored by the ConPTY shell so that CLIs the user
    // has installed (Claude, Codex, ...) resolve. Keep the test hermetic: drop a
    // stand-in executable in a temp directory, prefix PATH with that directory,
    // and assert the shell resolves it. This proves PATH prefixing works through
    // the ConPTY boundary without depending on any real CLI being installed.
    let stub_dir = std::env::temp_dir().join(format!("splice-pty-path-{}", std::process::id()));
    std::fs::create_dir_all(&stub_dir).expect("stub PATH directory should be creatable");
    std::fs::write(
        stub_dir.join("splice-fake-cli.cmd"),
        "@echo splice-fake-cli\r\n",
    )
    .expect("stub CLI should be writable");

    let path_prefix = stub_dir.display().to_string();
    let output = splice_pty::run_conpty_command_with_input(
        "cmd.exe",
        &["/D", "/K", &format!("set PATH={path_prefix};%PATH%")],
        "where splice-fake-cli\r\nexit\r\n",
        splice_pty::TerminalSize::new(100, 30).expect("valid terminal size"),
    )
    .expect("ConPTY shell should run with prefixed PATH");

    let _ = std::fs::remove_dir_all(&stub_dir);

    assert!(
        output.contains("splice-fake-cli.cmd"),
        "expected prefixed PATH to resolve the stub CLI, got: {output:?}"
    );
}
