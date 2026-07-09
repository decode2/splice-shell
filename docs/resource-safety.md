# Resource & Process Safety

Splice Shell runs a Windows ConPTY process and manages clipboard images in `%TEMP%`. This document describes how the application protects the host machine from resource leaks.

## Problems addressed

| Problem | Symptom | Fix |
|---------|---------|-----|
| Blocking Tauri commands | UI thread freeze, PC unresponsive | All Tauri commands marked `async fn` |
| PTY output flooding | CPU spike, IPC saturation | 16 ms output throttle buffer |
| Clipboard PNG accumulation | Unbounded disk usage in `%TEMP%` | Age-based sweeper + lifecycle hooks |
| Orphan PTY processes on crash | Ghost processes after app close | Windows Job objects + tree walk fallback |

---

## Tauri async commands

Every Tauri backend command runs on a background worker thread. None execute on the main UI thread.

```rust
// All commands are declared as async fn.
// Tauri schedules them on its async runtime.
#[tauri::command]
async fn pty_spawn(...) -> Result<...> { ... }
```

**Why it matters:** a blocking call on the main thread (e.g. ConPTY creation, pipe write) would freeze the WebView2 event loop and make the app appear hung.

---

## PTY output throttle (16 ms)

The ConPTY reader pushes output into an `mpsc` channel. A background flusher wakes every 16 ms, drains the channel, and emits one batched IPC event to the frontend.

```
ConPTY reader → mpsc::channel → flusher (16 ms) → tauri::emit → xterm.js
```

At 60 FPS the budget per frame is ~16 ms, so this groups high-frequency output into at most one IPC call per frame. CPU and JSON serialization overhead drop significantly under heavy output (e.g. `cargo build` logs).

---

## Clipboard image lifecycle

Clipboard previews are written to `%TEMP%\splice-shell\clipboard\splice-clipboard-*.png`.

| Event | Action |
|-------|--------|
| App startup | Delete entire `%TEMP%\splice-shell\clipboard\` directory |
| PTY session close | Delete the image for that session immediately |
| App shutdown | Delete entire `%TEMP%\splice-shell\clipboard\` directory |
| Background sweep | Delete any `.png` older than 5 minutes |

Files are matched by `metadata.modified()` time (falling back to `metadata.created()` on filesystems that do not track modification time). Access errors (locked files, permission denied) are ignored silently.

---

## PTY process tree termination (Windows)

When a PTY session is closed, Splice Shell terminates the full process tree — not just the immediate child — to prevent orphan processes.

**Primary path — Windows Job object:**
The ConPTY process is assigned to a Job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. When the Job handle is dropped, Windows terminates every process in the job.

The process is spawned with `CREATE_BREAKAWAY_FROM_JOB` to allow nested jobs (required when the parent process already owns a job, for example inside some CI environments or IDE wrappers).

**Fallback path — process tree walk:**
If job assignment fails (e.g. nested job limit reached on older Windows versions), Splice Shell walks the process tree from the leaf processes upward and terminates each one with `TerminateProcess`. This prevents orphans even when Job objects are unavailable.

---

## Verification

All safety behaviours are covered by the test suite:

```powershell
cargo test --workspace   # 74 tests, 0 failures
cargo clippy --workspace --all-targets -- -D warnings  # 0 warnings
```

Key test names:
- `test_flusher_aggregates_high_frequency_output`
- `test_flusher_idle_flushes_immediately`
- `test_startup_cleanup_deletes_all_temp_files`
- `test_session_close_deletes_specific_image_immediately`
- `test_shutdown_cleanup_deletes_temp_directory`
- `test_fallback_tree_termination_when_job_is_none`
- `live_pty_session_kills_grandchild_process_tree_on_close`
- `pty_spawn_close_cycles_do_not_leak_process_handles`
