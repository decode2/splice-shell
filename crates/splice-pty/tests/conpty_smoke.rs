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
        move |_id, output| {
            let _ = sender.send(output);
        },
        |_id| {},
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
fn on_output_receives_the_spawned_session_id() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    // The `on_output` callback must receive the emitting session's monotonic
    // id as its first argument (mirroring `on_exit`), so the Tauri layer can
    // attribute each `pty-output` chunk to the session that produced it.
    let (sender, receiver) = mpsc::channel::<(u64, String)>();
    let session = splice_pty::PtySession::spawn(
        "cmd.exe",
        &["/D", "/K"],
        splice_pty::TerminalSize::new(80, 24).expect("valid terminal size"),
        move |id, output| {
            let _ = sender.send((id, output));
        },
        |_id| {},
    )
    .expect("live ConPTY session should start");

    let session_id = session.id();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut observed_id: Option<u64> = None;
    while Instant::now() < deadline && observed_id.is_none() {
        if let Ok((id, _chunk)) = receiver.recv_timeout(Duration::from_millis(250)) {
            observed_id = Some(id);
        }
    }

    session.close();

    assert_eq!(
        observed_id,
        Some(session_id),
        "on_output must receive the emitting session's monotonic id as its first argument"
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
        move |_id, output| {
            let _ = sender.send(output);
        },
        |_id| {},
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
        move |_id, output| {
            let _ = sender.send(output);
        },
        |_id| {},
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

#[cfg(windows)]
#[test]
fn live_pty_session_kills_grandchild_process_tree_on_close() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    // Regression test for orphan-freedom: `close()` must tear down the
    // *entire* process tree rooted at the PTY's shell, not just the
    // immediate shell process. This spawns a real two-level-deep
    // descendant (root PowerShell -> cmd.exe -> ping.exe) and asserts it is
    // gone after `close()`, exercising the
    // `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` teardown path added to
    // `crates/splice-pty/src/lib.rs`.
    //
    // What this does NOT prove: it does not pin the exact TOCTOU window
    // that motivated the fix (a grandchild spawned *during* the old
    // snapshot-and-kill enumeration in `close()`). Reliably winning that
    // race deterministically would require sub-millisecond control over
    // when the grandchild process is created relative to `close()`'s
    // internal snapshot, which this environment cannot guarantee. Instead,
    // this proves the achievable, still-meaningful invariant: a real
    // grandchild that is fully alive and confirmed running by the time
    // `close()` is called is nonetheless terminated by it. Any change that
    // stops assigning the shell to a kill-on-close job, or that stops
    // closing the job handle during teardown (leaving cleanup to the old
    // PID-tree walk alone), will make this test fail if that walk ever
    // misses the grandchild -- and always regresses the job-based
    // guarantee even when the walk happens to still catch it.
    let (sender, receiver) = mpsc::channel::<String>();
    let session = splice_pty::PtySession::spawn(
        "powershell.exe",
        &["-NoLogo", "-NoProfile"],
        splice_pty::TerminalSize::new(120, 30).expect("valid terminal size"),
        move |_id, output| {
            let _ = sender.send(output);
        },
        |_id| {},
    )
    .expect("live ConPTY session should start");

    // Ask PowerShell to launch `cmd.exe` (a child of the PTY's root
    // process), have that `cmd.exe` run a long-lived `ping -t` (a
    // grandchild of the root process), then report the grandchild's real
    // PID so the test can verify it directly rather than inferring it.
    let script = concat!(
        r#"$child = Start-Process -FilePath cmd.exe -ArgumentList '/D','/C','ping -t 127.0.0.1 >NUL' -PassThru; "#,
        r#"Start-Sleep -Milliseconds 800; "#,
        r#"$gc = (Get-CimInstance Win32_Process -Filter "ParentProcessId=$($child.Id)" | Select-Object -First 1 -ExpandProperty ProcessId); "#,
        r#"Write-Output "SPLICE-GRANDCHILD-PID:$gc""#,
        "\r\n"
    );
    session
        .write(script)
        .expect("PowerShell should accept the grandchild-spawning script");

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut output = String::new();
    let mut grandchild_pid: Option<u32> = None;
    while Instant::now() < deadline && grandchild_pid.is_none() {
        if let Ok(chunk) = receiver.recv_timeout(Duration::from_millis(250)) {
            output.push_str(&chunk);
            grandchild_pid = extract_marker_pid(&output, "SPLICE-GRANDCHILD-PID:");
        }
    }

    let grandchild_pid = match grandchild_pid {
        Some(pid) => pid,
        None => {
            session.close();
            panic!(
                "expected PowerShell to report a grandchild PID within the timeout, got: {output:?}"
            );
        }
    };

    assert!(
        process_is_alive(grandchild_pid),
        "expected grandchild ping.exe (pid {grandchild_pid}) to already be alive before close() \
         is called, so this test actually proves close() kills it rather than it having already \
         exited on its own"
    );

    session.close();

    let kill_deadline = Instant::now() + Duration::from_secs(10);
    let mut alive = process_is_alive(grandchild_pid);
    while alive && Instant::now() < kill_deadline {
        std::thread::sleep(Duration::from_millis(200));
        alive = process_is_alive(grandchild_pid);
    }

    assert!(
        !alive,
        "expected grandchild process (pid {grandchild_pid}) to be terminated as part of \
         close()'s process-tree teardown, but it was still running after the timeout -- \
         this is an orphaned process"
    );
}

#[cfg(windows)]
#[test]
fn pty_spawn_close_cycles_do_not_leak_process_handles() {
    // Regression test for the "no memory/handle leaks" invariant:
    // `PtySession::spawn` allocates several OS handles per session (input
    // pipe, output pipe, process, primary thread, pseudoconsole, and now a
    // Job Object) plus a reader thread, and `close()`/`Drop` must tear all
    // of it down. If `close()` ever stops closing one of those handles, this
    // test proves it by watching this *process's own* open-handle count
    // grow across many spawn+close cycles.
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

    fn handle_count() -> u32 {
        let mut count: u32 = 0;
        unsafe {
            GetProcessHandleCount(GetCurrentProcess(), &mut count)
                .expect("GetProcessHandleCount should succeed for the current process");
        }
        count
    }

    fn spawn_and_close_pty_session() {
        // `close()` forcibly terminates the child (`TerminateProcess`) and
        // joins the reader thread before returning, so there is no need to
        // write an `exit` command first: every handle/thread this session
        // owns must be gone by the time this function returns.
        let session = splice_pty::PtySession::spawn(
            "cmd.exe",
            &["/D", "/K"],
            splice_pty::TerminalSize::new(80, 24).expect("valid terminal size"),
            |_id, _output| {},
            |_id| {},
        )
        .expect("PTY session should spawn for the handle-leak regression test");
        session.close();
    }

    // Warm up first so lazy one-time allocations (thread pool spin-up,
    // first-use console/ToolHelp subsystem initialization, allocator arena
    // growth, etc.) happen *before* the baseline is measured, not during the
    // measured loop where they'd be misread as a per-session leak.
    const WARMUP_ITERATIONS: usize = 5;
    for _ in 0..WARMUP_ITERATIONS {
        spawn_and_close_pty_session();
    }

    let baseline = handle_count();

    // Enough iterations that a real per-session leak (even one stray handle
    // per `close()`) would dwarf `HANDLE_COUNT_SLACK` below, while staying
    // small enough to keep this test fast (a few seconds).
    const ITERATIONS: usize = 40;
    for _ in 0..ITERATIONS {
        spawn_and_close_pty_session();
    }

    let final_count = handle_count();

    // Handle counts jitter by a small amount run-to-run from unrelated
    // activity in this test process (allocator/runtime bookkeeping, other
    // threads, etc.). This slack absorbs that jitter but is far below the
    // ~ITERATIONS handles a genuine per-session leak would add: leaking just
    // one handle per `close()` over 40 iterations would blow past it by 4x.
    const HANDLE_COUNT_SLACK: u32 = 10;

    assert!(
        final_count <= baseline + HANDLE_COUNT_SLACK,
        "expected process handle count to stay within {HANDLE_COUNT_SLACK} of baseline \
         ({baseline}) after {ITERATIONS} PtySession spawn+close cycles, got {final_count} \
         (delta {}); this indicates PtySession::close()/Drop is leaking OS handles",
        final_count.saturating_sub(baseline)
    );
}

#[cfg(windows)]
#[test]
fn waiter_thread_fires_on_exit_when_child_exits_naturally() {
    use std::sync::mpsc;
    use std::time::Duration;

    // A child that exits on its own must drive the per-session waiter thread
    // to invoke `on_exit` with this session's id — the backend-push that
    // replaces the old frontend liveness poll.
    let (exit_tx, exit_rx) = mpsc::channel::<u64>();
    let session = splice_pty::PtySession::spawn(
        "cmd.exe",
        &["/D", "/C", "exit"],
        splice_pty::TerminalSize::new(80, 24).expect("valid terminal size"),
        |_id, _output| {},
        move |id| {
            let _ = exit_tx.send(id);
        },
    )
    .expect("short-lived ConPTY session should start");

    let session_id = session.id();

    let exited_id = exit_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("waiter thread should invoke on_exit after the child exits naturally");

    assert_eq!(
        exited_id, session_id,
        "on_exit must receive the exiting session's monotonic id"
    );

    session.close();
}

#[cfg(windows)]
#[test]
fn intentional_close_suppresses_on_exit_callback() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    // A `cmd /D /K` shell waits for input and never exits on its own, so the
    // only thing that can release the waiter is `close()`'s TerminateProcess.
    // Because `close()` publishes `closing = true` *before* terminating the
    // child, the waiter must observe it and suppress `on_exit`: an intentional
    // teardown is not a natural exit and must not trigger a frontend restart.
    let fired = Arc::new(AtomicBool::new(false));
    let fired_in_callback = Arc::clone(&fired);
    let session = splice_pty::PtySession::spawn(
        "cmd.exe",
        &["/D", "/K"],
        splice_pty::TerminalSize::new(80, 24).expect("valid terminal size"),
        |_id, _output| {},
        move |_id| {
            fired_in_callback.store(true, Ordering::SeqCst);
        },
    )
    .expect("live ConPTY session should start");

    // Let the shell come fully up so the waiter is parked in its wait, then
    // tear the session down intentionally.
    std::thread::sleep(Duration::from_millis(300));
    session.close();

    // `close()` joins the waiter before returning, so by now the waiter has
    // run to completion and either fired or suppressed the callback.
    assert!(
        !fired.load(Ordering::SeqCst),
        "intentional close() must suppress the natural-exit on_exit callback"
    );
}

#[cfg(windows)]
#[test]
fn natural_exit_spawn_close_cycles_do_not_leak_handles_or_threads() {
    // Natural-exit variant of `pty_spawn_close_cycles_do_not_leak_process_handles`.
    // The waiter thread duplicates the child process handle and waits on it;
    // `close()`/`Drop` must join the waiter and release that duplicated handle
    // every cycle. If the waiter thread or its handle ever leaks, this
    // process's open-handle count grows across many natural-exit cycles.
    use std::sync::mpsc;
    use std::time::Duration;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};

    fn handle_count() -> u32 {
        let mut count: u32 = 0;
        unsafe {
            GetProcessHandleCount(GetCurrentProcess(), &mut count)
                .expect("GetProcessHandleCount should succeed for the current process");
        }
        count
    }

    fn spawn_wait_natural_exit_and_close() {
        let (exit_tx, exit_rx) = mpsc::channel::<u64>();
        let session = splice_pty::PtySession::spawn(
            "cmd.exe",
            &["/D", "/C", "exit"],
            splice_pty::TerminalSize::new(80, 24).expect("valid terminal size"),
            |_id, _output| {},
            move |id| {
                let _ = exit_tx.send(id);
            },
        )
        .expect("PTY session should spawn for the natural-exit handle-leak test");

        // Wait for the natural exit so the waiter has fired `on_exit` and is
        // finishing; then `close()` must join it and free its handle.
        let _ = exit_rx.recv_timeout(Duration::from_secs(10));
        session.close();
    }

    const WARMUP_ITERATIONS: usize = 5;
    for _ in 0..WARMUP_ITERATIONS {
        spawn_wait_natural_exit_and_close();
    }

    let baseline = handle_count();

    const ITERATIONS: usize = 40;
    for _ in 0..ITERATIONS {
        spawn_wait_natural_exit_and_close();
    }

    let final_count = handle_count();
    const HANDLE_COUNT_SLACK: u32 = 10;

    assert!(
        final_count <= baseline + HANDLE_COUNT_SLACK,
        "expected process handle count to stay within {HANDLE_COUNT_SLACK} of baseline \
         ({baseline}) after {ITERATIONS} natural-exit spawn+close cycles, got {final_count} \
         (delta {}); this indicates the waiter thread or its duplicated process handle is \
         leaking",
        final_count.saturating_sub(baseline)
    );
}

/// Finds `marker` in `output` and parses the run of ASCII digits immediately
/// following it as a `u32`.
///
/// PowerShell's PSReadLine echoes the command line back with syntax
/// highlighting *before* it executes, so a literal marker string containing
/// a variable reference (e.g. `"...:$gc"`) appears twice in the captured
/// output: once as echoed source text (followed by the variable's *name*,
/// not its value) and once as the real result (followed by digits). A plain
/// `str::find` would match the echoed occurrence first and fail to parse a
/// PID from it. This scans forward past any occurrence that isn't followed
/// by at least one digit, so it finds the real result wherever it appears.
#[cfg(windows)]
fn extract_marker_pid(output: &str, marker: &str) -> Option<u32> {
    let mut search_from = 0;
    while let Some(relative_pos) = output[search_from..].find(marker) {
        let pos = search_from + relative_pos;
        let tail = &output[pos + marker.len()..];
        let digits: String = tail.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        if !digits.is_empty() {
            if let Ok(pid) = digits.parse::<u32>() {
                return Some(pid);
            }
        }
        search_from = pos + marker.len();
    }
    None
}

/// Checks whether a process with the given PID is currently running.
///
/// Shells out to `tasklist` rather than linking the `windows` crate
/// directly into the test binary, so this regression test doesn't need its
/// own Win32 dependency just to assert a PID is gone.
#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .expect("tasklist should run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pid_text = pid.to_string();
    stdout.split_whitespace().any(|token| token == pid_text)
}
