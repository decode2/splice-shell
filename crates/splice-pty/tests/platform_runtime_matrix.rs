use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, Instant},
};

use splice_pty::{PtySession, TerminalSize};

fn size() -> TerminalSize {
    TerminalSize::new(80, 24).expect("fixture terminal size is valid")
}

fn supported_ubuntu_runtime() -> bool {
    let release = fs::read_to_string("/etc/os-release").unwrap_or_default();
    release.lines().any(|line| line == "ID=ubuntu")
        && release
            .lines()
            .any(|line| matches!(line, "VERSION_ID=\"22.04\"" | "VERSION_ID=\"24.04\""))
}

fn require_supported_ubuntu_runtime() -> bool {
    if supported_ubuntu_runtime() {
        true
    } else {
        eprintln!(
            "SKIP: native Ubuntu runtime fixture requires Ubuntu 22.04 or 24.04; this host is unsupported"
        );
        false
    }
}

fn receive_until(receiver: &mpsc::Receiver<String>, marker: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut output = String::new();
    while Instant::now() < deadline && !output.contains(marker) {
        if let Ok(chunk) = receiver.recv_timeout(Duration::from_millis(250)) {
            output.push_str(&chunk);
        }
    }
    output
}

#[test]
fn native_ubuntu_runtime_fixture_exercises_shell_utf8_input_resize_and_liveness() {
    if !require_supported_ubuntu_runtime() {
        return;
    }

    let (sender, receiver) = mpsc::channel();
    let session = PtySession::spawn(
        "/bin/sh",
        &[
            "-c",
            "printf 'ready utf8=café\\n'; stty size; read value; printf 'input=%s\\n' \"$value\"; stty size",
        ],
        size(),
        move |_, output| {
            let _ = sender.send(output);
        },
        |_| {},
    )
    .expect("native Ubuntu default shell starts");

    assert!(session.is_running().expect("liveness is available"));
    assert_eq!(session.active_process_name().expect("process name"), "sh");
    assert!(receive_until(&receiver, "24 80").contains("ready utf8=café"));

    session
        .resize(TerminalSize::new(132, 43).expect("resized fixture terminal size"))
        .expect("resize succeeds");
    session.write("fixture-input\n").expect("input succeeds");
    let output = receive_until(&receiver, "43 132");
    assert!(output.contains("input=fixture-input"));
    assert!(output.contains("43 132"));
    session.close();
    println!("RECEIPT: native Ubuntu PTY startup, UTF-8, input, resize, and liveness passed");
}

#[test]
fn native_ubuntu_runtime_fixture_exercises_interrupt_teardown_and_idempotent_close() {
    if !require_supported_ubuntu_runtime() {
        return;
    }

    let (sender, receiver) = mpsc::channel();
    let session = PtySession::spawn(
        "/bin/sh",
        &[
            "-c",
            "trap 'printf interrupted; exit' INT; printf ready; while :; do sleep 1; done",
        ],
        size(),
        move |_, output| {
            let _ = sender.send(output);
        },
        |_| {},
    )
    .expect("native Ubuntu default shell starts");

    assert!(receive_until(&receiver, "ready").contains("ready"));
    session.interrupt().expect("interrupt succeeds");
    assert!(receive_until(&receiver, "interrupted").contains("interrupted"));
    session.close();
    session.close();
    assert!(!session.is_running().expect("liveness after close"));
    println!("RECEIPT: native Ubuntu PTY interrupt, teardown, and idempotent close passed");
}

#[test]
fn ci_declares_authoritative_windows_and_native_ubuntu_runtime_gates() {
    let workflow = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/ci.yml");
    let workflow = fs::read_to_string(workflow).expect("CI workflow is readable");

    for required_gate in [
        "ConPTY runtime regression gate (authoritative Windows)",
        "cargo test -p splice-pty --test conpty_smoke",
        "Native Ubuntu PTY runtime",
        "runs-on: ubuntu-24.04",
        "cargo test -p splice-pty --test platform_runtime_matrix",
        "cargo test -p splice-pty --test session_contract",
        "cargo test -p splice-shell-desktop --test platform_authority",
    ] {
        assert!(
            workflow.contains(required_gate),
            "CI workflow must retain runtime gate: {required_gate}"
        );
    }
}
