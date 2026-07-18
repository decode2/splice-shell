#![cfg(unix)]

#[rustfmt::skip]
mod tests {
use std::{path::PathBuf, sync::mpsc, time::{Duration, Instant}};
use splice_pty::{PtyError, PtySession, PtySpawnOptions, TerminalSize};

fn size() -> TerminalSize { TerminalSize::new(80, 24).unwrap() } fn receive_until(receiver: &mpsc::Receiver<String>, marker: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5); let mut output = String::new();
    while Instant::now() < deadline && !output.contains(marker) {
        if let Ok(chunk) = receiver.recv_timeout(Duration::from_millis(250)) { output.push_str(&chunk); }
    } output
}
fn spawn(command: &str) -> (PtySession, mpsc::Receiver<String>) {
    let (sender, receiver) = mpsc::channel(); let session = PtySession::spawn("/bin/sh", &[
        "-c", command], size(), move |_, output| { let _ = sender.send(output); }, |_| {}).unwrap();
    (session, receiver)
}
fn pid(output: &str) -> String { output.split("pid=").nth(1).unwrap().trim().to_owned() } fn is_alive(pid: &str) -> bool { std::process::Command::new("kill").args(["-0", pid]).status().unwrap().success() }
#[test] fn unix_pty_uses_structured_argv_absolute_cwd_env_and_utf8_output() {
    let cwd = std::env::temp_dir().join(format!("splice-pty-unix-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).unwrap(); let (sender, receiver) = mpsc::channel();
    let session = PtySession::spawn_with_options("/bin/sh", &["-c", "printf 'cwd=%s env=%s utf8=%s\\n' \"$PWD\" \"$SPLICE_PTY_ENV\" 'café'"],
        PtySpawnOptions { cwd: Some(cwd.clone()), env: vec![("SPLICE_PTY_ENV".into(), "configured".into())] }, size(),
        move |_, output| { let _ = sender.send(output); }, |_| {}).unwrap();
    let output = receive_until(&receiver, "utf8=café"); session.close(); std::fs::remove_dir_all(&cwd).unwrap();
    assert!(output.contains(&format!("cwd={}", cwd.display())) && output.contains("env=configured") && output.contains("utf8=café")); }
#[test] fn unix_pty_starts_shell_accepts_input_and_reports_liveness_and_name() {
    let (session, receiver) = spawn("read line; printf 'input=%s\\n' \"$line\"");
    assert!(session.is_running().unwrap()); assert_eq!(session.active_process_name().unwrap(), "sh"); session.write("accepted\n").unwrap();
    assert!(receive_until(&receiver, "input=accepted").contains("input=accepted")); session.close(); }
#[test] fn unix_pty_rejects_a_non_absolute_working_directory() {
    let result = PtySession::spawn_with_options("/bin/sh", &["-c", "true"],
        PtySpawnOptions { cwd: Some(PathBuf::from("relative")), ..PtySpawnOptions::default() }, size(), |_, _| {}, |_| {});
    assert!(matches!(result, Err(PtyError::InvalidWorkingDirectory))); }
#[test] fn unix_pty_close_and_drop_terminate_children_without_natural_exit() {
    let (exit_sender, exit_receiver) = mpsc::channel(); let (sender, receiver) = mpsc::channel();
    let session = PtySession::spawn("/bin/sh", &["-c", "printf 'pid=%s\\n' $$; sleep 30"], size(),
        move |_, output| { let _ = sender.send(output); }, move |_| { let _ = exit_sender.send(()); }).unwrap();
    let child = pid(&receive_until(&receiver, "pid=")); session.close(); session.close();
    assert!(!is_alive(&child) && exit_receiver.recv_timeout(Duration::from_millis(100)).is_err());
    let (session, receiver) = spawn("printf 'pid=%s\\n' $$; sleep 30"); let child = pid(&receive_until(&receiver, "pid="));
    drop(session); assert!(!is_alive(&child)); }
#[test] fn unix_pty_interrupt_writes_etx() {
    let (session, receiver) = spawn("trap 'printf interrupted; exit' INT; printf ready; while :; do sleep 1; done");
    assert!(receive_until(&receiver, "ready").contains("ready")); session.interrupt().unwrap();
    assert!(receive_until(&receiver, "interrupted").contains("interrupted")); session.close(); }
#[test] fn unix_pty_replaces_malformed_utf8_and_preserves_split_sequences() {
    let (session, receiver) = spawn("printf '\\303'; sleep 0.1; printf '\\251\\377ok'");
    assert!(receive_until(&receiver, "ok").contains("é�ok")); session.close(); }
#[test] fn unix_pty_rejects_exact_windows_default_but_runs_unix_programs() {
    let result = PtySession::spawn("cmd.exe", &[], size(), |_, _| {}, |_| {});
    assert!(matches!(result, Err(PtyError::UnsupportedPlatform))); let (session, receiver) = spawn("printf explicit-unix");
    assert!(receive_until(&receiver, "explicit-unix").contains("explicit-unix")); session.close(); }
#[test] fn unix_pty_close_is_bounded_when_child_ignores_hup() {
    let (session, receiver) = spawn("trap '' HUP; printf 'pid=%s\\n' $$; while :; do sleep 1; done");
    let child = pid(&receive_until(&receiver, "pid=")); let started = Instant::now(); session.close();
    assert!(started.elapsed() < Duration::from_secs(2) && !is_alive(&child)); }
#[test] fn unix_pty_resize_changes_live_terminal_dimensions() {
    let (session, receiver) = spawn("stty size; sleep 0.2; stty size");
    assert!(receive_until(&receiver, "24 80").contains("24 80"));
    session.resize(TerminalSize::new(132, 43).unwrap()).unwrap();
    assert!(receive_until(&receiver, "43 132").contains("43 132")); session.close(); }
}
