pub mod flow;

/// A backend-independent event emitted by a PTY session.
///
/// Output always carries the originating session id, including output that
/// arrives before a frontend listener is ready. A natural exit is distinct
/// from an explicit close so callers do not restart a deliberately closed tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtySessionEvent {
    Output { session_id: u64, data: String },
    NaturalExit { session_id: u64 },
}

impl PtySessionEvent {
    pub fn from_output(session_id: u64, data: String) -> Self {
        Self::Output { session_id, data }
    }

    pub fn natural_exit(session_id: u64) -> Self {
        Self::NaturalExit { session_id }
    }

    pub fn session_id(&self) -> u64 {
        match self {
            Self::Output { session_id, .. } | Self::NaturalExit { session_id } => *session_id,
        }
    }

    pub fn output(&self) -> Option<&str> {
        match self {
            Self::Output { data, .. } => Some(data),
            Self::NaturalExit { .. } => None,
        }
    }
}

/// Shared lifecycle state for target-specific PTY sessions.
///
/// `begin_close` returns true only for the first close, which gives every
/// backend an exactly-once teardown boundary and suppresses natural-exit
/// notifications caused by an explicit close.
pub struct PtySessionLifecycle {
    id: u64,
    closing: std::sync::atomic::AtomicBool,
}

impl PtySessionLifecycle {
    pub fn new(id: u64) -> Self {
        Self {
            id,
            closing: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn begin_close(&self) -> bool {
        !self.closing.swap(true, std::sync::atomic::Ordering::SeqCst)
    }

    pub fn should_emit_natural_exit(&self) -> bool {
        !self.closing.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Observable methods every target-specific PTY session must provide.
///
/// Spawning takes output and natural-exit callbacks; implementations must
/// attribute both callbacks with `id`, preserve the shared credit-window ACK
/// semantics, and make `close` idempotent.
pub trait PtySessionContract {
    fn id(&self) -> u64;
    fn write(&self, data: &str) -> Result<(), PtyError>;
    fn interrupt(&self) -> Result<(), PtyError>;
    fn resize(&self, size: TerminalSize) -> Result<(), PtyError>;
    fn is_running(&self) -> Result<bool, PtyError>;
    fn active_process_name(&self) -> Result<String, PtyError>;
    fn active_process_candidates(&self) -> Result<Vec<String>, PtyError>;
    fn close(&self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub columns: u16,
    pub rows: u16,
}

impl TerminalSize {
    pub fn new(columns: u16, rows: u16) -> Result<Self, TerminalSizeError> {
        if columns == 0 {
            return Err(TerminalSizeError::ZeroColumns);
        }

        if rows == 0 {
            return Err(TerminalSizeError::ZeroRows);
        }

        Ok(Self { columns, rows })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalSizeError {
    ZeroColumns,
    ZeroRows,
}

#[derive(Debug)]
pub enum PtyError {
    #[cfg(windows)]
    Windows(windows::core::Error),
    Io(std::io::Error),
    CommandContainsNul,
    InvalidWorkingDirectory,
    InvalidEnvironment,
    InvalidOutput,
    SessionClosed,
    UnsupportedPlatform,
}

impl std::fmt::Display for PtyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(windows)]
            Self::Windows(error) => write!(f, "Windows API error: {error}"),
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::CommandContainsNul => write!(f, "command contains a NUL byte"),
            Self::InvalidWorkingDirectory => {
                write!(f, "working directory must be an absolute directory")
            }
            Self::InvalidEnvironment => write!(f, "environment override is invalid"),
            Self::InvalidOutput => write!(f, "PTY output was not valid UTF-8"),
            Self::SessionClosed => write!(f, "PTY session is closed"),
            Self::UnsupportedPlatform => write!(f, "ConPTY is only supported on Windows"),
        }
    }
}

impl std::error::Error for PtyError {}

impl PtyError {
    pub fn is_terminal_closed(&self) -> bool {
        match self {
            Self::SessionClosed => true,
            Self::Io(error) => matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::NotConnected
            ),
            #[cfg(windows)]
            Self::Windows(error) => {
                let code = error.code().0 as u32;
                code == 0x800700E8 || code == 0x8007006D
            }
            _ => false,
        }
    }
}

impl From<std::io::Error> for PtyError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(windows)]
impl From<windows::core::Error> for PtyError {
    fn from(error: windows::core::Error) -> Self {
        Self::Windows(error)
    }
}

#[cfg(not(windows))]
pub fn run_conpty_smoke_command(
    _program: &str,
    _args: &[&str],
    _size: TerminalSize,
) -> Result<String, PtyError> {
    Err(PtyError::UnsupportedPlatform)
}

#[cfg(not(windows))]
pub fn run_conpty_command_with_input(
    _program: &str,
    _args: &[&str],
    _input: &str,
    _size: TerminalSize,
) -> Result<String, PtyError> {
    Err(PtyError::UnsupportedPlatform)
}

#[cfg(not(windows))]
pub fn run_conpty_command_with_resize(
    _program: &str,
    _args: &[&str],
    _input: &str,
    _initial_size: TerminalSize,
    _resized_size: TerminalSize,
) -> Result<String, PtyError> {
    Err(PtyError::UnsupportedPlatform)
}

#[cfg(unix)]
#[derive(Debug, Default)]
pub struct PtySpawnOptions {
    pub cwd: Option<std::path::PathBuf>,
    pub env: Vec<(String, String)>,
}

#[cfg(unix)]
pub struct PtySession {
    lifecycle: std::sync::Arc<PtySessionLifecycle>,
    master: std::sync::Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    writer: std::sync::Mutex<Option<Box<dyn std::io::Write + Send>>>,
    killer: std::sync::Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    pid: Option<u32>,
    reader: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    waiter: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    process_name: String,
    on_closing: std::sync::Arc<dyn Fn() + Send + Sync>,
}

#[cfg(unix)]
impl PtySession {
    pub fn spawn<F, G>(
        program: &str,
        args: &[&str],
        size: TerminalSize,
        on_output: F,
        on_exit: G,
    ) -> Result<Self, PtyError>
    where
        F: FnMut(u64, String) + Send + 'static,
        G: FnOnce(u64) + Send + 'static,
    {
        Self::spawn_with_close_hook(program, args, size, on_output, on_exit, || {})
    }

    pub fn spawn_with_close_hook<F, G, H>(
        program: &str,
        args: &[&str],
        size: TerminalSize,
        on_output: F,
        on_exit: G,
        on_closing: H,
    ) -> Result<Self, PtyError>
    where
        F: FnMut(u64, String) + Send + 'static,
        G: FnOnce(u64) + Send + 'static,
        H: Fn() + Send + Sync + 'static,
    {
        Self::spawn_with_options_and_close_hook(
            program,
            args,
            PtySpawnOptions::default(),
            size,
            on_output,
            on_exit,
            on_closing,
        )
    }

    pub fn spawn_with_options<F, G>(
        program: &str,
        args: &[&str],
        options: PtySpawnOptions,
        size: TerminalSize,
        on_output: F,
        on_exit: G,
    ) -> Result<Self, PtyError>
    where
        F: FnMut(u64, String) + Send + 'static,
        G: FnOnce(u64) + Send + 'static,
    {
        Self::spawn_with_options_and_close_hook(
            program,
            args,
            options,
            size,
            on_output,
            on_exit,
            || {},
        )
    }

    fn spawn_with_options_and_close_hook<F, G, H>(
        program: &str,
        args: &[&str],
        options: PtySpawnOptions,
        size: TerminalSize,
        on_output: F,
        on_exit: G,
        on_closing: H,
    ) -> Result<Self, PtyError>
    where
        F: FnMut(u64, String) + Send + 'static,
        G: FnOnce(u64) + Send + 'static,
        H: Fn() + Send + Sync + 'static,
    {
        use portable_pty::{native_pty_system, CommandBuilder, PtySize};

        if program == "cmd.exe" {
            return Err(PtyError::UnsupportedPlatform);
        }
        validate_spawn_inputs(program, args, &options)?;

        let lifecycle = std::sync::Arc::new(PtySessionLifecycle::new(next_pty_session_id()));
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: size.rows,
                cols: size.columns,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| PtyError::Io(std::io::Error::other(error)))?;
        let mut command = CommandBuilder::new(program);
        command.args(args);
        if let Some(cwd) = &options.cwd {
            command.cwd(cwd);
        }
        for (key, value) in &options.env {
            command.env(key, value);
        }

        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|error| PtyError::Io(std::io::Error::other(error)))?;
        let pid = child.process_id();
        let killer = child.clone_killer();
        drop(pair.slave);
        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|error| PtyError::Io(std::io::Error::other(error)))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|error| PtyError::Io(std::io::Error::other(error)))?;
        let id = lifecycle.id();
        let reader_lifecycle = std::sync::Arc::clone(&lifecycle);

        let (reader_completed_tx, reader_completed) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            read_output(reader, id, reader_lifecycle, on_output);
            let _ = reader_completed_tx.send(());
        });

        let waiter_lifecycle = std::sync::Arc::clone(&lifecycle);
        let waiter_running = std::sync::Arc::clone(&running);
        let waiter = std::thread::spawn(move || {
            let exited = child.wait().is_ok();
            waiter_running.store(false, std::sync::atomic::Ordering::SeqCst);
            // A Unix PTY returns EOF/EIO only after the child releases its slave
            // side. Drain that final output before observers see natural exit.
            let _ = reader_completed.recv_timeout(std::time::Duration::from_millis(250));
            if exited && waiter_lifecycle.should_emit_natural_exit() {
                on_exit(waiter_lifecycle.id());
            }
        });

        Ok(Self {
            lifecycle,
            master: std::sync::Mutex::new(pair.master),
            writer: std::sync::Mutex::new(Some(writer)),
            killer: std::sync::Mutex::new(killer),
            pid,
            reader: std::sync::Mutex::new(Some(reader)),
            waiter: std::sync::Mutex::new(Some(waiter)),
            running,
            process_name: std::path::Path::new(program)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            on_closing: std::sync::Arc::new(on_closing),
        })
    }

    pub fn id(&self) -> u64 {
        self.lifecycle.id()
    }

    pub fn write(&self, data: &str) -> Result<(), PtyError> {
        if !self.lifecycle.should_emit_natural_exit() {
            return Err(PtyError::SessionClosed);
        }

        let mut writer = self.writer.lock().map_err(|_| PtyError::SessionClosed)?;
        let writer = writer.as_mut().ok_or(PtyError::SessionClosed)?;
        writer.write_all(data.as_bytes())?;
        writer.flush()?;
        Ok(())
    }

    pub fn interrupt(&self) -> Result<(), PtyError> {
        if !self.lifecycle.should_emit_natural_exit() {
            return Err(PtyError::SessionClosed);
        }
        match self.writer.try_lock() {
            Ok(mut writer) => {
                let writer = writer.as_mut().ok_or(PtyError::SessionClosed)?;
                writer.write_all(b"\x03")?;
                writer.flush()?;
                Ok(())
            }
            Err(std::sync::TryLockError::WouldBlock) if self.signal_process_group(libc::SIGINT) => {
                Ok(())
            }
            Err(_) => Err(PtyError::SessionClosed),
        }
    }

    pub fn resize(&self, size: TerminalSize) -> Result<(), PtyError> {
        use portable_pty::PtySize;
        if !self.lifecycle.should_emit_natural_exit() {
            return Err(PtyError::SessionClosed);
        }
        self.master
            .lock()
            .map_err(|_| PtyError::SessionClosed)?
            .resize(PtySize {
                rows: size.rows,
                cols: size.columns,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|error| PtyError::Io(std::io::Error::other(error)))
    }

    pub fn is_running(&self) -> Result<bool, PtyError> {
        Ok(self.running.load(std::sync::atomic::Ordering::SeqCst))
    }

    pub fn active_process_name(&self) -> Result<String, PtyError> {
        Ok(self.process_name.clone())
    }

    pub fn active_process_candidates(&self) -> Result<Vec<String>, PtyError> {
        Ok(vec![self.active_process_name()?])
    }

    pub fn close(&self) {
        if self.lifecycle.begin_close() {
            (self.on_closing)();
            for signal in [libc::SIGHUP, libc::SIGTERM, libc::SIGKILL] {
                let _ = self.signal_process_group(signal);
                if self.wait_for_teardown(std::time::Duration::from_millis(100)) {
                    break;
                }
            }
            if !self.teardown_complete() {
                if let Ok(mut killer) = self.killer.lock() {
                    let _ = killer.kill();
                }
                let _ = self.wait_for_teardown(std::time::Duration::from_millis(250));
            }
            if let Ok(mut writer) = self.writer.lock() {
                writer.take();
            }
            for thread in [&self.waiter, &self.reader] {
                if let Ok(mut thread) = thread.lock() {
                    if let Some(thread) = thread.take() {
                        let _ = thread.join();
                    }
                }
            }
        }
    }

    fn signal_process_group(&self, signal: libc::c_int) -> bool {
        self.pid.is_some_and(|pid| unsafe {
            libc::kill(-(pid as libc::pid_t), signal) == 0
                || (self.running.load(std::sync::atomic::Ordering::SeqCst)
                    && libc::kill(pid as libc::pid_t, signal) == 0)
        })
    }

    fn teardown_complete(&self) -> bool {
        !self.process_group_exists() && !self.running.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn process_group_exists(&self) -> bool {
        self.pid.is_some_and(|pid| unsafe {
            libc::kill(-(pid as libc::pid_t), 0) == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        })
    }

    fn wait_for_teardown(&self, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while !self.teardown_complete() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        self.teardown_complete()
    }
}

#[cfg(unix)]
impl PtySessionContract for PtySession {
    fn id(&self) -> u64 {
        self.id()
    }

    fn write(&self, data: &str) -> Result<(), PtyError> {
        self.write(data)
    }

    fn interrupt(&self) -> Result<(), PtyError> {
        self.interrupt()
    }

    fn resize(&self, size: TerminalSize) -> Result<(), PtyError> {
        self.resize(size)
    }

    fn is_running(&self) -> Result<bool, PtyError> {
        self.is_running()
    }

    fn active_process_name(&self) -> Result<String, PtyError> {
        self.active_process_name()
    }

    fn active_process_candidates(&self) -> Result<Vec<String>, PtyError> {
        self.active_process_candidates()
    }

    fn close(&self) {
        self.close();
    }
}

#[cfg(unix)]
impl Drop for PtySession {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(unix)]
fn next_pty_session_id() -> u64 {
    static NEXT_PTY_SESSION_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT_PTY_SESSION_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
}

#[cfg(unix)]
fn validate_spawn_inputs(
    program: &str,
    args: &[&str],
    options: &PtySpawnOptions,
) -> Result<(), PtyError> {
    if program.contains('\0') || args.iter().any(|arg| arg.contains('\0')) {
        return Err(PtyError::CommandContainsNul);
    }
    if options
        .cwd
        .as_ref()
        .is_some_and(|cwd| !cwd.is_absolute() || !cwd.is_dir())
    {
        return Err(PtyError::InvalidWorkingDirectory);
    }
    if options
        .env
        .iter()
        .any(|(key, value)| key.is_empty() || key.contains('\0') || value.contains('\0'))
    {
        return Err(PtyError::InvalidEnvironment);
    }
    Ok(())
}

#[cfg(unix)]
fn read_output<F>(
    mut reader: Box<dyn std::io::Read + Send>,
    id: u64,
    lifecycle: std::sync::Arc<PtySessionLifecycle>,
    mut on_output: F,
) where
    F: FnMut(u64, String),
{
    use std::io::Read;

    let mut bytes = [0; 4096];
    let mut pending = Vec::new();
    while lifecycle.should_emit_natural_exit() {
        let count = match reader.read(&mut bytes) {
            Ok(count) => count,
            // Linux PTYs report EIO once their slave side closes. It is EOF,
            // not a session error, and still needs the buffered UTF-8 flush.
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(_) => break,
        };
        if count == 0 {
            break;
        }
        pending.extend_from_slice(&bytes[..count]);

        while !pending.is_empty() {
            match std::str::from_utf8(&pending) {
                Ok(output) => {
                    on_output(id, output.to_owned());
                    pending.clear();
                }
                Err(error) if error.valid_up_to() > 0 => {
                    let valid = error.valid_up_to();
                    let output = std::str::from_utf8(&pending[..valid])
                        .expect("valid UTF-8 prefix reported by Utf8Error");
                    on_output(id, output.to_owned());
                    pending.drain(..valid);
                }
                Err(error) if error.error_len().is_none() => break,
                Err(error) => {
                    on_output(id, "\u{fffd}".to_owned());
                    pending.drain(..error.error_len().unwrap());
                }
            }
        }
    }
    if lifecycle.should_emit_natural_exit() && !pending.is_empty() {
        on_output(id, String::from_utf8_lossy(&pending).into_owned());
    }
}

#[cfg(windows)]
pub fn run_conpty_smoke_command(
    program: &str,
    args: &[&str],
    size: TerminalSize,
) -> Result<String, PtyError> {
    windows_conpty::run_command_with_input(program, args, "", size)
}

#[cfg(windows)]
pub fn run_conpty_command_with_input(
    program: &str,
    args: &[&str],
    input: &str,
    size: TerminalSize,
) -> Result<String, PtyError> {
    windows_conpty::run_command_with_input(program, args, input, size)
}

#[cfg(windows)]
pub fn run_conpty_command_with_resize(
    program: &str,
    args: &[&str],
    input: &str,
    initial_size: TerminalSize,
    resized_size: TerminalSize,
) -> Result<String, PtyError> {
    windows_conpty::run_command_with_resize(program, args, input, initial_size, resized_size)
}

#[cfg(windows)]
pub use windows_conpty::PtySession;

#[cfg(windows)]
impl PtySessionContract for PtySession {
    fn id(&self) -> u64 {
        self.id()
    }

    fn write(&self, data: &str) -> Result<(), PtyError> {
        self.write(data)
    }

    fn interrupt(&self) -> Result<(), PtyError> {
        self.interrupt()
    }

    fn resize(&self, size: TerminalSize) -> Result<(), PtyError> {
        self.resize(size)
    }

    fn is_running(&self) -> Result<bool, PtyError> {
        self.is_running()
    }

    fn active_process_name(&self) -> Result<String, PtyError> {
        self.active_process_name()
    }

    fn active_process_candidates(&self) -> Result<Vec<String>, PtyError> {
        self.active_process_candidates()
    }

    fn close(&self) {
        self.close();
    }
}

#[cfg(windows)]
mod windows_conpty {
    use super::{PtyError, PtySessionLifecycle, TerminalSize};
    use std::{
        ffi::c_void,
        mem::size_of,
        ptr::null_mut,
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc, Mutex,
        },
        thread::{self, JoinHandle},
        time::Duration,
    };
    use windows::{
        core::{PCWSTR, PWSTR},
        Win32::{
            Foundation::{
                CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, INVALID_HANDLE_VALUE,
            },
            Storage::FileSystem::{ReadFile, WriteFile},
            System::{
                Console::{
                    AttachConsole, ClosePseudoConsole, CreatePseudoConsole, FreeConsole,
                    GenerateConsoleCtrlEvent, GetStdHandle, ResizePseudoConsole,
                    SetConsoleCtrlHandler, SetStdHandle, COORD, CTRL_C_EVENT, HPCON,
                    STD_ERROR_HANDLE, STD_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
                },
                Diagnostics::ToolHelp::{
                    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                    TH32CS_SNAPPROCESS,
                },
                JobObjects::{
                    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                },
                Pipes::CreatePipe,
                Threading::{
                    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess,
                    InitializeProcThreadAttributeList, ResumeThread, TerminateProcess,
                    UpdateProcThreadAttribute, WaitForSingleObject, CREATE_BREAKAWAY_FROM_JOB,
                    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
                    INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROCESS_TERMINATE,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
                },
            },
        },
    };

    /// Process-wide monotonic source of `PtySession` ids. Assigned at spawn
    /// via `fetch_add`, exposed through `PtySession::id()`, echoed back from
    /// the Tauri `pty_spawn` command, and carried in the `pty-exit` event
    /// payload so the frontend can distinguish a live session's exit from a
    /// stale (already-superseded) one. Starts at 1 so 0 stays available as a
    /// sentinel for callers that need one.
    static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

    pub struct PtySession {
        inner: Mutex<Option<PtySessionInner>>,
        // Published `true` by `close()` BEFORE it terminates the child, so the
        // per-session waiter thread — when its wait is released by that
        // teardown rather than by the child exiting on its own — suppresses
        // the natural-exit `on_exit` callback. Stored OUTSIDE `inner` on
        // purpose: the waiter must be able to read it without ever locking the
        // session mutex, which keeps the waiter off `inner` entirely and makes
        // a lock cycle with `close()` impossible.
        lifecycle: Arc<PtySessionLifecycle>,
        // Join handle for this session's waiter thread. `close()` joins it (via
        // `Option::take`, so it is idempotent across `close()`/`Drop`) so
        // neither the thread nor the process handle it waits on outlives the
        // session. Kept outside `inner` so the join never runs while the
        // session lock is held.
        waiter: Mutex<Option<JoinHandle<()>>>,
        // Invoked exactly once, at the very top of `close()`, BEFORE the child
        // is terminated and long before the reader thread is joined.
        //
        // Why this exists: with credit-based flow control the consumer's
        // `on_output` callback is ALLOWED to block (that is how backpressure
        // reaches the child — a parked reader stops calling `ReadFile`, the
        // ConPTY pipe fills, and the child blocks on write). But `close()`
        // joins the reader thread, so a reader parked inside `on_output` would
        // wedge `close()`, `kill()` and app shutdown forever. This hook is the
        // consumer's deterministic chance to release whatever the reader is
        // parked on (in `splice-shell` it closes the session's credit window,
        // which makes the flusher exit and drop the channel receiver, which in
        // turn makes the reader's `send` return `Err`).
        //
        // Fired from `close()` only, guarded by the lifecycle's
        // false->true transition, so it runs exactly once even though `close()`
        // is idempotent and also runs from `Drop`.
        on_closing: Box<dyn Fn() + Send + Sync>,
    }

    struct PtySessionInner {
        // Wrapped in `Arc` so `write`/`interrupt` can clone a strong
        // reference while holding the session lock, then release the lock
        // before performing the blocking `WriteFile` call. This keeps the
        // handle alive (no use-after-close) even if `close()` runs
        // concurrently and drops `PtySessionInner`, without forcing
        // `interrupt()` to wait behind a blocked `write()`.
        input_write: Arc<OwnedHandle>,
        process: OwnedHandle,
        _process_thread: OwnedHandle,
        conpty: OwnedPseudoConsole,
        reader: Option<JoinHandle<()>>,
        root_process_id: u32,
        root_process_name: String,
        // `Some` when the child was placed in a `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
        // job at spawn time (see `create_kill_on_close_job`/`spawn_process`).
        // Closing this handle in `close()` atomically kills every process
        // still in the job, including anything spawned after assignment,
        // with no snapshot race and no PID-recycling hazard. `None` only if
        // job creation/assignment failed at spawn time, in which case
        // `close()` falls back to the legacy snapshot-based tree walk.
        job: Option<OwnedHandle>,
    }

    impl PtySession {
        pub fn spawn<F, G>(
            program: &str,
            args: &[&str],
            size: TerminalSize,
            on_output: F,
            on_exit: G,
        ) -> Result<Self, PtyError>
        where
            F: FnMut(u64, String) + Send + 'static,
            G: FnOnce(u64) + Send + 'static,
        {
            Self::spawn_with_close_hook(program, args, size, on_output, on_exit, || {})
        }

        /// Same as [`PtySession::spawn`], plus a teardown hook invoked once at
        /// the top of [`PtySession::close`] (see `PtySession::on_closing`).
        ///
        /// Callers that make `on_output` blocking — which credit-based flow
        /// control necessarily does — MUST use this constructor and release the
        /// reader from the hook, otherwise `close()` deadlocks joining a reader
        /// parked inside `on_output`.
        pub fn spawn_with_close_hook<F, G, H>(
            program: &str,
            args: &[&str],
            size: TerminalSize,
            mut on_output: F,
            on_exit: G,
            on_closing: H,
        ) -> Result<Self, PtyError>
        where
            F: FnMut(u64, String) + Send + 'static,
            G: FnOnce(u64) + Send + 'static,
            H: Fn() + Send + Sync + 'static,
        {
            let handles = spawn_process(program, args, size)?;
            let id = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);

            // Start the exit waiter only AFTER `spawn_process` returned `Ok`,
            // so its early resume-failure path (which terminates the child and
            // returns `Err`) never leaves a waiter parked on a handle that will
            // never be observed by a live session. Duplicate the child process
            // handle into an independent `OwnedHandle` and MOVE it into the
            // waiter thread: the waiter is its sole owner and closes it when it
            // ends, so this handle is deliberately NOT stored in
            // `PtySessionInner` (exactly one owner).
            let lifecycle = Arc::new(PtySessionLifecycle::new(id));
            let lifecycle_for_waiter = Arc::clone(&lifecycle);
            let waiter_process = duplicate_process_handle(handles.process.raw())?;
            let waiter_handle = SendHandle(waiter_process.into_raw());
            let waiter = thread::spawn(move || {
                // Sole owner of the duplicated process handle; dropped (and
                // thus `CloseHandle`d) when this thread ends. This closure
                // captures only lightweight data (`id`, an `Arc<PtySessionLifecycle>`,
                // the duplicated handle, and `on_exit`) — never `inner` and
                // never an `Arc<PtySession>` — so it can never form a lock
                // cycle with `close()`.
                let process = OwnedHandle::new(waiter_handle.into_inner());
                unsafe {
                    WaitForSingleObject(process.raw(), INFINITE);
                }
                // Only a NATURAL exit fires the callback. If `close()`
                // began close before terminating the child, this wait
                // was released by that intentional teardown, so suppress
                // `on_exit` (no spurious frontend restart). The lifecycle's
                // SeqCst load pairs with its SeqCst close transition.
                if lifecycle_for_waiter.should_emit_natural_exit() {
                    on_exit(id);
                }
            });

            let output_reader = SendHandle(handles.output_read.into_raw());
            let reader = thread::spawn(move || {
                // Accumulate raw bytes and only decode up to a UTF-8 character
                // boundary, holding back any multibyte sequence split across a
                // chunk boundary until the next read completes it.
                let mut pending: Vec<u8> = Vec::new();
                let _ = read_chunks_from_send(output_reader, |bytes| {
                    pending.extend_from_slice(bytes);
                    let split = pending.len() - incomplete_utf8_tail_len(&pending);
                    if split > 0 {
                        on_output(id, String::from_utf8_lossy(&pending[..split]).into_owned());
                        pending.drain(..split);
                    }
                });
                // Flush trailing bytes at EOF; an incomplete sequence here will
                // never complete, so decode it lossily rather than drop it.
                if !pending.is_empty() {
                    on_output(id, String::from_utf8_lossy(&pending).into_owned());
                }
            });

            Ok(Self {
                inner: Mutex::new(Some(PtySessionInner {
                    input_write: Arc::new(handles.input_write),
                    process: handles.process,
                    _process_thread: handles.process_thread,
                    conpty: handles.conpty,
                    reader: Some(reader),
                    root_process_id: handles.process_id,
                    root_process_name: program.to_owned(),
                    job: handles.job,
                })),
                lifecycle,
                waiter: Mutex::new(Some(waiter)),
                on_closing: Box::new(on_closing),
            })
        }

        /// Monotonic id assigned to this session at spawn. Stable for the
        /// session's lifetime and carried in the `pty-exit` event payload.
        pub fn id(&self) -> u64 {
            self.lifecycle.id()
        }

        pub fn write(&self, data: &str) -> Result<(), PtyError> {
            // Hold the session lock only long enough to read out the input
            // pipe handle; the blocking `WriteFile` call runs after the lock
            // is released so a hung child cannot stall other session
            // operations (e.g. `interrupt`) that need the same lock.
            let input_write = {
                let guard = self.inner.lock().map_err(|_| {
                    PtyError::Io(std::io::Error::other("PTY session lock poisoned"))
                })?;
                let inner = guard.as_ref().ok_or(PtyError::SessionClosed)?;
                if !inner.is_running() {
                    return Err(PtyError::SessionClosed);
                }

                Arc::clone(&inner.input_write)
            };

            write_all(input_write.raw(), data.as_bytes())
        }

        pub fn interrupt(&self) -> Result<(), PtyError> {
            // Same rationale as `write`: release the session lock before the
            // blocking pipe write and the console-attach dance below, so a
            // hung child's blocked `write()` cannot also block `interrupt()`.
            let (input_write, root_process_id) = {
                let guard = self.inner.lock().map_err(|_| {
                    PtyError::Io(std::io::Error::other("PTY session lock poisoned"))
                })?;
                let inner = guard.as_ref().ok_or(PtyError::SessionClosed)?;
                if !inner.is_running() {
                    return Err(PtyError::SessionClosed);
                }

                (Arc::clone(&inner.input_write), inner.root_process_id)
            };

            let input_result = write_all(input_write.raw(), b"\x03");
            let signal_result = send_console_interrupt(root_process_id);

            // Either leg succeeding is still a usable interrupt, and that is
            // deliberate: a raw-mode TUI (Claude, Codex, ...) reads the `\x03`
            // byte from stdin itself and never needs a console signal, while a
            // console application (`cmd.exe` running `ping -t`) only reacts to
            // CTRL_C_EVENT and ignores the byte entirely. Keep those semantics.
            //
            // But the console-signal leg must NEVER fail invisibly. The pipe
            // write practically always succeeds, so folding a failed signal
            // into a bare `Ok(())` made `interrupt()` report success while
            // doing nothing at all -- which is precisely how a broken
            // `send_console_interrupt` (missing `FreeConsole`, so `AttachConsole`
            // returned ERROR_ACCESS_DENIED on every call) shipped unnoticed.
            // Surface it so a persistent console-signal failure is observable.
            // `log::warn!` (not `eprintln!`): the windowed release build has no
            // stderr, so the previous print went nowhere in production. The host
            // app registers a real log backend that writes this to a file.
            if let Err(ref error) = signal_result {
                log::warn!(
                    "splice-pty: console interrupt signal failed for root pid {root_process_id}: \
                     {error}; only the raw \\x03 byte was delivered, so console applications will \
                     not be interrupted"
                );
            }

            match (input_result, signal_result) {
                (Ok(()), _) | (_, Ok(())) => Ok(()),
                (Err(error), Err(_)) => Err(error),
            }
        }

        pub fn resize(&self, size: TerminalSize) -> Result<(), PtyError> {
            let guard = self
                .inner
                .lock()
                .map_err(|_| PtyError::Io(std::io::Error::other("PTY session lock poisoned")))?;
            let inner = guard.as_ref().ok_or(PtyError::SessionClosed)?;

            resize_console(inner.conpty.0, size)
        }

        pub fn is_running(&self) -> Result<bool, PtyError> {
            let guard = self
                .inner
                .lock()
                .map_err(|_| PtyError::Io(std::io::Error::other("PTY session lock poisoned")))?;
            let inner = guard.as_ref().ok_or(PtyError::SessionClosed)?;

            Ok(inner.is_running())
        }

        pub fn active_process_name(&self) -> Result<String, PtyError> {
            let guard = self
                .inner
                .lock()
                .map_err(|_| PtyError::Io(std::io::Error::other("PTY session lock poisoned")))?;
            let inner = guard.as_ref().ok_or(PtyError::SessionClosed)?;

            process_tree_target_name(inner.root_process_id)
                .map(|name| name.unwrap_or_else(|| inner.root_process_name.clone()))
        }

        pub fn active_process_candidates(&self) -> Result<Vec<String>, PtyError> {
            let guard = self
                .inner
                .lock()
                .map_err(|_| PtyError::Io(std::io::Error::other("PTY session lock poisoned")))?;
            let inner = guard.as_ref().ok_or(PtyError::SessionClosed)?;

            let mut candidates = process_tree_candidate_names(inner.root_process_id)?;
            if candidates.is_empty() {
                candidates.push(inner.root_process_name.clone());
            }

            Ok(candidates)
        }

        pub fn close(&self) {
            // Publish the closing intent BEFORE any teardown so the waiter
            // thread, when its wait is released by the `TerminateProcess`
            // below, observes it and suppresses the natural-exit callback.
            // SeqCst pairs with the waiter's SeqCst load. Must run at the very
            // top, before `terminate_process_tree`/`TerminateProcess` and the
            // job drop, otherwise the waiter could wake and fire `on_exit`
            // during teardown.
            //
            // `swap` (not `store`) so the false->true transition also fires the
            // teardown hook EXACTLY once, even though `close()` is idempotent
            // and runs again from `Drop`.
            if self.lifecycle.begin_close() {
                // MUST run before the reader join below: with credit-based flow
                // control the reader can be parked inside a blocking
                // `on_output`, and this hook is what releases it. Running it
                // after the join would be too late — the join is the thing that
                // would never return.
                (self.on_closing)();
            }

            if let Ok(mut guard) = self.inner.lock() {
                if let Some(mut inner) = guard.take() {
                    unsafe {
                        // The kill-on-close job (when present) is the
                        // race-free tree-teardown mechanism: closing its
                        // handle below atomically kills every process still
                        // in the job, including grandchildren spawned after
                        // `spawn_process` returned. Running the old
                        // snapshot-and-kill walk on top of a working job
                        // would add no correctness benefit and reintroduces
                        // the exact PID-recycling hazard this fix removes
                        // (a snapshot taken now could target a PID already
                        // recycled by the time it's opened). Only fall back
                        // to it when no job was assigned at spawn time.
                        if inner.job.is_none() {
                            let _ = terminate_process_tree(inner.root_process_id);
                        }
                        let _ = TerminateProcess(inner.process.raw(), 0);
                        WaitForSingleObject(inner.process.raw(), INFINITE);
                    }
                    // Drop the job before the other handles so the
                    // kill-on-close tree teardown fires as early as
                    // possible during shutdown; `ClosePseudoConsole` and the
                    // reader-thread join below don't depend on job members
                    // being alive, so this ordering is deadlock-free.
                    drop(inner.job.take());
                    drop(inner.input_write);
                    drop(inner.conpty);
                    if let Some(reader) = inner.reader.take() {
                        let _ = reader.join();
                    }
                }
            }

            // Join the waiter AFTER releasing the inner lock so neither the
            // thread nor the duplicated process handle it owns outlives the
            // session. Deadlock-free: the waiter's wait was released by the
            // `TerminateProcess` above (or the child already exited), and the
            // waiter never touches `inner`, so this join cannot cycle on the
            // session lock. Idempotent via `Option::take`, matching
            // `close()`/`Drop`. INVARIANT: `close()` is never called from the
            // waiter thread itself (the waiter holds no `Arc<PtySession>`), so
            // this can never self-join.
            if let Ok(mut waiter_guard) = self.waiter.lock() {
                if let Some(waiter) = waiter_guard.take() {
                    let _ = waiter.join();
                }
            }
        }
    }

    impl PtySessionInner {
        fn is_running(&self) -> bool {
            const WAIT_TIMEOUT_VALUE: u32 = 0x0000_0102;

            unsafe { WaitForSingleObject(self.process.raw(), 0).0 == WAIT_TIMEOUT_VALUE }
        }
    }

    /// Serializes every mutation of this process's console state: the whole
    /// attach -> signal -> detach sequence in `send_console_interrupt`, and the
    /// `SetConsoleCtrlHandler` + `CreateProcessW` pair in `spawn_process`.
    ///
    /// `FreeConsole`, `AttachConsole` and `SetConsoleCtrlHandler` all mutate
    /// PROCESS-WIDE state: a process is attached to at most one console, and it
    /// has exactly one Ctrl-handler setting. The app runs several PTY sessions
    /// at once (tabs), so without this lock:
    ///
    /// - Two concurrent `interrupt()` calls interleave destructively: thread A's
    ///   `ConsoleSignalGuard` detaches the console thread B just attached to (so
    ///   B signals nothing, or signals the wrong console), or A restores Ctrl+C
    ///   handling while B's `CTRL_C_EVENT` is still in flight, which delivers B's
    ///   event to *this* process and kills the app.
    /// - An `interrupt()` racing a `spawn()` is worse still: `send_console_interrupt`
    ///   sets "ignore Ctrl+C" for the duration of its signal, and that attribute is
    ///   INHERITED at `CreateProcess` time -- so a shell spawned inside that window
    ///   would be permanently un-interruptible.
    static CONSOLE_LOCK: Mutex<()> = Mutex::new(());

    /// Acquires [`CONSOLE_LOCK`], recovering from poisoning rather than
    /// propagating it.
    ///
    /// The guarded value is `()`: there is no invariant a panicking holder could
    /// have left half-updated, because every console mutation is undone by an
    /// RAII guard that also runs during unwind. Propagating the poison would
    /// instead make Ctrl+C — and, worse, `spawn` — permanently unusable for the
    /// rest of the process's life after a single unrelated panic.
    fn lock_console() -> std::sync::MutexGuard<'static, ()> {
        CONSOLE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn send_console_interrupt(process_id: u32) -> Result<(), PtyError> {
        let _console_lock = lock_console();

        // `AttachConsole` can rebind this process's STD_INPUT/OUTPUT/ERROR to
        // the console it attaches to. Those handles die when we detach again a
        // few lines later, which would silently break the caller's own stdout
        // and stderr (the Tauri dev console, the cargo test harness, the
        // `eprintln!` diagnostic in `interrupt()` itself). Snapshot them here
        // and restore them on the way out, on every path including failure.
        let _std_handles = StdHandlesGuard::capture();

        unsafe {
            // ROOT CAUSE OF THE Ctrl+C BUG: a process can be attached to at
            // most one console, and the caller is normally already attached to
            // its own (the debug/dev Tauri build and the cargo test harness are
            // console-subsystem binaries; a release GUI build is not, thanks to
            // the `windows_subsystem = "windows"` attribute in main.rs). Without
            // detaching first, `AttachConsole` below returns ERROR_ACCESS_DENIED
            // (0x80070005) every single time and no CTRL_C_EVENT is ever sent.
            //
            // `FreeConsole` when no console is attached fails harmlessly with
            // ERROR_INVALID_HANDLE -- that is the expected result in the release
            // GUI build -- so its error is deliberately discarded.
            let _ = FreeConsole();

            AttachConsole(process_id)?;

            // Ignore Ctrl+C in *this* process before generating the event.
            // `CTRL_C_EVENT` cannot be scoped to a process group (Win32 ignores
            // a non-zero group id for it), so it necessarily goes to EVERY
            // process attached to the console -- and we are now one of them.
            // Without this the app kills itself with the signal it just raised.
            //
            // Installed before the guard so that a failure here still runs the
            // guard's `FreeConsole` (we are already attached at this point);
            // the guard's handler-restore is a harmless no-op if this failed.
            let handler_result = SetConsoleCtrlHandler(None, true);
            let _console_guard = ConsoleSignalGuard;
            handler_result?;

            GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0)?;
        }

        Ok(())
    }

    /// How long to let an in-flight `CTRL_C_EVENT` land before this process
    /// stops ignoring Ctrl+C again. See `ConsoleSignalGuard::drop`.
    const CTRL_EVENT_SETTLE: Duration = Duration::from_millis(50);

    /// Undoes, in the only safe order, the two process-global console mutations
    /// `send_console_interrupt` makes.
    ///
    /// Order is load-bearing and was found the hard way: restoring the Ctrl+C
    /// handler while still attached to the child's console terminates THIS
    /// process with STATUS_CONTROL_C_EXIT. `GenerateConsoleCtrlEvent` delivers
    /// asynchronously (conhost raises the event in each attached client), so
    /// the event we raised is typically still in flight when the call returns.
    /// Clearing the ignore flag at that moment hands our own copy of the event
    /// to the default handler, which kills us.
    ///
    /// So: detach from the console first, then let any already-dispatched event
    /// settle, and only then restore the handler. Detaching alone is not
    /// enough, because a `CtrlRoutine` thread may already have been created in
    /// this process before the detach; the ignore flag must still be set when
    /// that thread runs. The two steps together close the window.
    struct ConsoleSignalGuard;

    impl Drop for ConsoleSignalGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = FreeConsole();
            }
            thread::sleep(CTRL_EVENT_SETTLE);
            unsafe {
                let _ = SetConsoleCtrlHandler(None, false);
            }
        }
    }

    /// Saves and restores this process's standard handles around the console
    /// attach/detach dance in `send_console_interrupt`. See the call site for
    /// why that is necessary.
    struct StdHandlesGuard([(STD_HANDLE, Option<HANDLE>); 3]);

    impl StdHandlesGuard {
        fn capture() -> Self {
            let capture_one = |which: STD_HANDLE| (which, unsafe { GetStdHandle(which) }.ok());

            Self([
                capture_one(STD_INPUT_HANDLE),
                capture_one(STD_OUTPUT_HANDLE),
                capture_one(STD_ERROR_HANDLE),
            ])
        }
    }

    impl Drop for StdHandlesGuard {
        fn drop(&mut self) {
            for (which, handle) in self.0 {
                if let Some(handle) = handle {
                    unsafe {
                        let _ = SetStdHandle(which, handle);
                    }
                }
            }
        }
    }

    impl Drop for PtySession {
        fn drop(&mut self) {
            self.close();
        }
    }

    unsafe impl Send for PtySession {}
    unsafe impl Sync for PtySession {}

    pub fn run_command_with_input(
        program: &str,
        args: &[&str],
        input: &str,
        size: TerminalSize,
    ) -> Result<String, PtyError> {
        run_command(program, args, input, size, None)
    }

    pub fn run_command_with_resize(
        program: &str,
        args: &[&str],
        input: &str,
        initial_size: TerminalSize,
        resized_size: TerminalSize,
    ) -> Result<String, PtyError> {
        run_command(program, args, input, initial_size, Some(resized_size))
    }

    fn run_command(
        program: &str,
        args: &[&str],
        input: &str,
        size: TerminalSize,
        resize_to: Option<TerminalSize>,
    ) -> Result<String, PtyError> {
        let handles = spawn_process(program, args, size)?;
        let input_write = handles.input_write;
        let conpty = handles.conpty;
        let process = handles.process;
        let process_thread = handles.process_thread;
        let job = handles.job;
        let output_reader = SendHandle(handles.output_read.into_raw());
        let reader = thread::spawn(move || read_all_from_send(output_reader));
        drop(process_thread);
        thread::sleep(Duration::from_millis(100));
        if let Some(resized_size) = resize_to {
            resize_console(conpty.0, resized_size)?;
        }
        write_all(input_write.raw(), input.as_bytes())?;
        thread::sleep(Duration::from_millis(1_000));
        drop(input_write);

        unsafe {
            WaitForSingleObject(process.raw(), INFINITE);
        }
        drop(process);
        drop(conpty);
        // Drop the job last so any child the shell spawned but didn't clean
        // up before exiting is still reaped via kill-on-close, keeping this
        // one-shot helper orphan-free too.
        if job.is_none() {
            let _ = terminate_process_tree(handles.process_id);
        }
        drop(job);

        let bytes = reader
            .join()
            .map_err(|_| PtyError::Io(std::io::Error::other("output reader thread panicked")))??;

        String::from_utf8(bytes).map_err(|_| PtyError::InvalidOutput)
    }

    struct SpawnedProcess {
        input_write: OwnedHandle,
        output_read: OwnedHandle,
        process: OwnedHandle,
        process_thread: OwnedHandle,
        process_id: u32,
        conpty: OwnedPseudoConsole,
        // See `PtySessionInner::job` for the kill-on-close contract this
        // handle establishes.
        job: Option<OwnedHandle>,
    }

    fn spawn_process(
        program: &str,
        args: &[&str],
        size: TerminalSize,
    ) -> Result<SpawnedProcess, PtyError> {
        let mut command_line = command_line(program, args)?;
        let mut input_read = HANDLE::default();
        let mut input_write = HANDLE::default();

        unsafe {
            CreatePipe(&mut input_read, &mut input_write, None, 0)?;
        }

        // Wrap the first pipe pair immediately so a failure from the second
        // `CreatePipe` call below closes them via `Drop` instead of leaking
        // the raw handles.
        let input_read = OwnedHandle::new(input_read);
        let input_write = OwnedHandle::new(input_write);

        let mut output_read = HANDLE::default();
        let mut output_write = HANDLE::default();

        unsafe {
            CreatePipe(&mut output_read, &mut output_write, None, 0)?;
        }

        let output_read = OwnedHandle::new(output_read);
        let output_write = OwnedHandle::new(output_write);

        let conpty = unsafe {
            CreatePseudoConsole(
                COORD {
                    X: size.columns as i16,
                    Y: size.rows as i16,
                },
                input_read.raw(),
                output_write.raw(),
                0,
            )?
        };
        let conpty = OwnedPseudoConsole(conpty);
        drop(input_read);
        drop(output_write);

        let mut attribute_list = ProcThreadAttributeList::new(conpty.0)?;
        let startup_info = attribute_list.startup_info_mut();
        let mut process_info = PROCESS_INFORMATION::default();

        // Guarantee the child inherits "process Ctrl+C normally".
        //
        // The ignore-Ctrl+C attribute toggled by `SetConsoleCtrlHandler(NULL, ...)`
        // is INHERITED by child processes at `CreateProcess` time. So if this
        // process was itself started with Ctrl+C disabled -- which is exactly what
        // `CREATE_NEW_PROCESS_GROUP` does, and what CI runners, task launchers and
        // "detached" process spawners routinely use -- then every shell this PTY
        // spawns, and every command that shell runs, silently inherits "ignore
        // Ctrl+C". Both interrupt legs then *appear* to work (the `\x03` byte is
        // written, `GenerateConsoleCtrlEvent` returns `Ok`) while no console client
        // ever reacts. Clearing the flag here makes an interruptible child an
        // invariant of `spawn`, independent of how the host app was launched.
        //
        // Held under `CONSOLE_LOCK` across the `CreateProcessW` calls below because
        // `send_console_interrupt` flips this same process-global flag to `true` for
        // the duration of its signal: a spawn racing an interrupt would otherwise
        // hand a brand-new child the "ignore Ctrl+C" attribute permanently.
        let console_lock = lock_console();
        unsafe {
            let _ = SetConsoleCtrlHandler(None, false);
        }

        let mut create_result = unsafe {
            CreateProcessW(
                None,
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                false,
                // CREATE_SUSPENDED holds the primary thread until this
                // function explicitly resumes it below, *after* the process
                // has been assigned to the kill-on-close job. Without this,
                // the child could start running (and spawning its own
                // children) before it is a job member, reopening the exact
                // race this whole mechanism exists to close.
                EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_UNICODE_ENVIRONMENT
                    | CREATE_SUSPENDED
                    | CREATE_BREAKAWAY_FROM_JOB,
                None,
                None,
                &startup_info.StartupInfo,
                &mut process_info,
            )
        };

        if let Err(ref e) = create_result {
            if e.code().0 == -2147024891 {
                // 0x80070005 as i32 (Access is denied)
                create_result = unsafe {
                    CreateProcessW(
                        None,
                        Some(PWSTR(command_line.as_mut_ptr())),
                        None,
                        None,
                        false,
                        EXTENDED_STARTUPINFO_PRESENT
                            | CREATE_UNICODE_ENVIRONMENT
                            | CREATE_SUSPENDED,
                        None,
                        None,
                        &startup_info.StartupInfo,
                        &mut process_info,
                    )
                };
            }
        }
        drop(console_lock);
        create_result?;

        let process = OwnedHandle::new(process_info.hProcess);
        let process_thread = OwnedHandle::new(process_info.hThread);

        // Attach the still-suspended child to a kill-on-close Job Object.
        // Ordering is load-bearing: CREATE_SUSPENDED (above) -> assign ->
        // resume (below). Assigning before resuming guarantees the OS kills
        // the *whole* tree — including anything the child spawns after this
        // point — atomically when the job's last handle closes, with no
        // snapshot race and no PID-recycling hazard (the job tracks
        // membership by kernel object, not by PID).
        //
        // `AssignProcessToJobObject` can fail on some hosts (e.g. this
        // process is itself confined to a job created without
        // JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK on pre-Windows-8 semantics).
        // Degrade gracefully rather than failing the spawn: `close()` falls
        // back to the legacy snapshot-based `terminate_process_tree` walk
        // for sessions where `job` ends up `None`.
        let job = match create_kill_on_close_job() {
            Ok(job) => match unsafe { AssignProcessToJobObject(job.raw(), process.raw()) } {
                Ok(()) => Some(job),
                Err(_) => None,
            },
            Err(_) => None,
        };

        let resume_result = unsafe { ResumeThread(process_thread.raw()) };
        if resume_result == u32::MAX {
            // The child is suspended and about to be leaked (no other
            // handle will observe it once this function returns an error).
            // Terminate it explicitly rather than relying solely on `job`
            // being dropped, since `job` may be `None` here.
            let error = windows::core::Error::from_win32();
            unsafe {
                let _ = TerminateProcess(process.raw(), 0);
            }
            return Err(PtyError::Windows(error));
        }

        Ok(SpawnedProcess {
            input_write,
            output_read,
            process,
            process_thread,
            process_id: process_info.dwProcessId,
            conpty,
            job,
        })
    }

    /// Duplicates the child process handle into an independent, owned handle
    /// the waiter thread can block on with `WaitForSingleObject` without racing
    /// the main session handle stored in `PtySessionInner` (which `close()`
    /// may terminate and drop concurrently). `DUPLICATE_SAME_ACCESS` copies the
    /// source access rights, which include `SYNCHRONIZE`, so the duplicate is
    /// waitable. Both source and target process are the current process.
    fn duplicate_process_handle(process: HANDLE) -> Result<OwnedHandle, PtyError> {
        let mut duplicate = HANDLE::default();
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                process,
                GetCurrentProcess(),
                &mut duplicate,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )?;
        }

        Ok(OwnedHandle::new(duplicate))
    }

    /// Creates a Job Object configured with
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: when the last handle to the
    /// returned job is closed, Windows atomically terminates every process
    /// still assigned to it. This is the race-free replacement for
    /// snapshot-and-kill tree teardown (see `terminate_process_tree`) —
    /// there is no window between "enumerate processes" and "terminate
    /// them" for a concurrently spawned grandchild to slip through, and no
    /// PID-recycling hazard, because job membership is tracked by kernel
    /// object rather than by PID.
    fn create_kill_on_close_job() -> Result<OwnedHandle, PtyError> {
        let job = unsafe { CreateJobObjectW(None, PCWSTR::null())? };
        let job = OwnedHandle::new(job);

        let mut limit_info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limit_info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                &limit_info as *const _ as *const c_void,
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
        }

        Ok(job)
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct ProcessSnapshotEntry {
        process_id: u32,
        parent_process_id: u32,
        name: String,
    }

    fn process_tree_target_name(root_process_id: u32) -> Result<Option<String>, PtyError> {
        let processes = process_snapshot()?;
        Ok(select_process_tree_target(root_process_id, &processes).map(|entry| entry.name.clone()))
    }

    fn process_tree_candidate_names(root_process_id: u32) -> Result<Vec<String>, PtyError> {
        let processes = process_snapshot()?;
        Ok(select_process_tree_candidates(root_process_id, &processes)
            .into_iter()
            .map(|entry| entry.name.clone())
            .collect())
    }

    /// Best-effort, racy fallback tree teardown: takes a ToolHelp snapshot,
    /// walks descendants of `root_process_id` by PID, and terminates each
    /// one found. Only used by `close()` when no kill-on-close Job Object
    /// was assigned to the session at spawn time (see
    /// `create_kill_on_close_job`), which should be rare (Windows 8+).
    ///
    /// This is *not* race-free: a process spawned between the snapshot and
    /// the kill loop is not observed and is orphaned, and a PID reused
    /// between snapshot and `OpenProcess` could in principle be targeted
    /// instead of the intended descendant. It is kept only as a
    /// last-resort fallback, not as a general teardown mechanism.
    fn terminate_process_tree(root_process_id: u32) -> Result<(), PtyError> {
        let processes = process_snapshot()?;
        let mut descendants = descendants_of(root_process_id, &processes);
        descendants.sort_by_key(|process| std::cmp::Reverse(process.process_id));

        for process in descendants {
            unsafe {
                if let Ok(handle) = windows::Win32::System::Threading::OpenProcess(
                    PROCESS_TERMINATE,
                    false,
                    process.process_id,
                ) {
                    let handle = OwnedHandle::new(handle);
                    let _ = TerminateProcess(handle.raw(), 0);
                    let _ = WaitForSingleObject(handle.raw(), 2_000);
                }
            }
        }

        Ok(())
    }

    fn select_process_tree_target(
        root_process_id: u32,
        processes: &[ProcessSnapshotEntry],
    ) -> Option<&ProcessSnapshotEntry> {
        select_process_tree_candidates(root_process_id, processes)
            .into_iter()
            .next()
            .or_else(|| {
                processes
                    .iter()
                    .find(|process| process.process_id == root_process_id)
            })
    }

    fn select_process_tree_candidates(
        root_process_id: u32,
        processes: &[ProcessSnapshotEntry],
    ) -> Vec<&ProcessSnapshotEntry> {
        let descendants = descendants_of(root_process_id, processes);
        let Some(target_leaf) = descendants
            .iter()
            .copied()
            .filter(|candidate| {
                !descendants
                    .iter()
                    .any(|process| process.parent_process_id == candidate.process_id)
            })
            .max_by_key(|process| process.process_id)
        else {
            return processes
                .iter()
                .filter(|process| process.process_id == root_process_id)
                .collect();
        };

        let mut candidates = Vec::new();
        let mut current_process_id = target_leaf.process_id;

        while let Some(process) = processes
            .iter()
            .find(|process| process.process_id == current_process_id)
        {
            candidates.push(process);

            if process.process_id == root_process_id {
                break;
            }

            current_process_id = process.parent_process_id;
        }

        candidates
    }

    fn descendants_of(
        root_process_id: u32,
        processes: &[ProcessSnapshotEntry],
    ) -> Vec<&ProcessSnapshotEntry> {
        let mut descendants = Vec::new();
        let mut frontier = vec![root_process_id];

        while let Some(parent_id) = frontier.pop() {
            for process in processes
                .iter()
                .filter(|process| process.parent_process_id == parent_id)
            {
                if descendants
                    .iter()
                    .any(|descendant: &&ProcessSnapshotEntry| {
                        descendant.process_id == process.process_id
                    })
                {
                    continue;
                }

                frontier.push(process.process_id);
                descendants.push(process);
            }
        }

        descendants
    }

    fn process_snapshot() -> Result<Vec<ProcessSnapshotEntry>, PtyError> {
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)? };
        let snapshot = OwnedHandle::new(snapshot);
        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..PROCESSENTRY32W::default()
        };
        let mut processes = Vec::new();

        let mut has_entry = unsafe { Process32FirstW(snapshot.raw(), &mut entry).is_ok() };
        while has_entry {
            processes.push(ProcessSnapshotEntry {
                process_id: entry.th32ProcessID,
                parent_process_id: entry.th32ParentProcessID,
                name: process_entry_name(&entry),
            });
            has_entry = unsafe { Process32NextW(snapshot.raw(), &mut entry).is_ok() };
        }

        Ok(processes)
    }

    fn process_entry_name(entry: &PROCESSENTRY32W) -> String {
        let end = entry
            .szExeFile
            .iter()
            .position(|ch| *ch == 0)
            .unwrap_or(entry.szExeFile.len());

        String::from_utf16_lossy(&entry.szExeFile[..end])
    }

    struct ProcThreadAttributeList {
        _storage: Box<[u8]>,
        startup_info: STARTUPINFOEXW,
    }

    impl ProcThreadAttributeList {
        fn new(conpty: HPCON) -> Result<Self, PtyError> {
            let mut attribute_list_size = 0usize;
            unsafe {
                let _ = InitializeProcThreadAttributeList(None, 1, None, &mut attribute_list_size);
            }

            let mut storage = vec![0u8; attribute_list_size].into_boxed_slice();
            let attribute_list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr() as *mut c_void);

            unsafe {
                InitializeProcThreadAttributeList(
                    Some(attribute_list),
                    1,
                    None,
                    &mut attribute_list_size,
                )?;
            }

            let mut startup_info = STARTUPINFOEXW::default();
            startup_info.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup_info.StartupInfo.hStdInput.0 = null_mut();
            startup_info.StartupInfo.hStdOutput.0 = null_mut();
            startup_info.StartupInfo.hStdError.0 = null_mut();
            startup_info.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
            startup_info.lpAttributeList = attribute_list;

            // Construct `Self` as soon as the attribute list is initialized
            // (before `UpdateProcThreadAttribute`) so that `Drop` runs
            // `DeleteProcThreadAttributeList` even if the update call below
            // fails. Otherwise an early `?` return would leak the list's
            // heap allocation and its OS-side registration.
            let attribute_list_owner = Self {
                _storage: storage,
                startup_info,
            };

            unsafe {
                UpdateProcThreadAttribute(
                    attribute_list,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    Some(conpty.0 as *const c_void),
                    size_of::<HPCON>(),
                    None,
                    None,
                )?;
            }

            Ok(attribute_list_owner)
        }

        fn startup_info_mut(&mut self) -> &mut STARTUPINFOEXW {
            &mut self.startup_info
        }
    }

    impl Drop for ProcThreadAttributeList {
        fn drop(&mut self) {
            unsafe {
                DeleteProcThreadAttributeList(self.startup_info.lpAttributeList);
            }
        }
    }

    fn resize_console(conpty: HPCON, size: TerminalSize) -> Result<(), PtyError> {
        unsafe {
            ResizePseudoConsole(
                conpty,
                COORD {
                    X: size.columns as i16,
                    Y: size.rows as i16,
                },
            )?;
        }

        Ok(())
    }

    fn command_line(program: &str, args: &[&str]) -> Result<Vec<u16>, PtyError> {
        let mut parts = Vec::with_capacity(args.len() + 1);
        parts.push(quote_windows_arg(program)?);
        for arg in args {
            parts.push(quote_windows_arg(arg)?);
        }

        let command = parts.join(" ");
        let mut wide: Vec<u16> = command.encode_utf16().collect();
        wide.push(0);
        Ok(wide)
    }

    fn quote_windows_arg(value: &str) -> Result<String, PtyError> {
        if value.contains('\0') {
            return Err(PtyError::CommandContainsNul);
        }

        if value.is_empty() || value.chars().any(|ch| ch.is_whitespace() || ch == '"') {
            Ok(format!("\"{}\"", value.replace('"', "\\\"")))
        } else {
            Ok(value.to_owned())
        }
    }

    fn read_all(handle: HANDLE) -> Result<Vec<u8>, PtyError> {
        let handle = OwnedHandle::new(handle);
        let mut output = Vec::new();
        let mut buffer = [0u8; 4096];

        loop {
            let mut bytes_read = 0u32;
            let result =
                unsafe { ReadFile(handle.raw(), Some(&mut buffer), Some(&mut bytes_read), None) };

            match result {
                Ok(()) if bytes_read == 0 => break,
                Ok(()) => output.extend_from_slice(&buffer[..bytes_read as usize]),
                Err(error) if !output.is_empty() => {
                    let _ = error;
                    break;
                }
                Err(error) => return Err(PtyError::Windows(error)),
            }
        }

        Ok(output)
    }

    fn write_all(handle: HANDLE, mut input: &[u8]) -> Result<(), PtyError> {
        while !input.is_empty() {
            let mut bytes_written = 0u32;
            unsafe {
                WriteFile(handle, Some(input), Some(&mut bytes_written), None)?;
            }

            if bytes_written == 0 {
                return Err(PtyError::Io(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "failed to write to ConPTY input pipe",
                )));
            }

            input = &input[bytes_written as usize..];
        }

        Ok(())
    }

    fn read_all_from_send(handle: SendHandle) -> Result<Vec<u8>, PtyError> {
        read_all(handle.into_inner())
    }

    fn read_chunks_from_send<F>(handle: SendHandle, mut on_chunk: F) -> Result<(), PtyError>
    where
        F: FnMut(&[u8]),
    {
        let handle = OwnedHandle::new(handle.into_inner());
        let mut buffer = [0u8; 4096];

        loop {
            let mut bytes_read = 0u32;
            let result =
                unsafe { ReadFile(handle.raw(), Some(&mut buffer), Some(&mut bytes_read), None) };

            match result {
                Ok(()) if bytes_read == 0 => break,
                Ok(()) => on_chunk(&buffer[..bytes_read as usize]),
                Err(_) => break,
            }
        }

        Ok(())
    }

    /// Number of trailing bytes that begin an as-yet-incomplete UTF-8 sequence.
    ///
    /// ConPTY output is read in fixed-size chunks, so a multibyte character can
    /// straddle a chunk boundary. Decoding each raw chunk independently would
    /// turn that split character into replacement characters. This returns how
    /// many bytes at the end of `buf` must be held back until the next chunk
    /// arrives; everything before them can be decoded now (`from_utf8_lossy`
    /// still replaces any genuinely invalid interior bytes). Returns 0 when
    /// `buf` ends on a character boundary or the trailing bytes are invalid
    /// rather than incomplete. The hold-back is at most 3 bytes.
    fn incomplete_utf8_tail_len(buf: &[u8]) -> usize {
        let max_lookback = buf.len().min(3);
        for back in 1..=max_lookback {
            let byte = buf[buf.len() - back];
            // Skip continuation bytes (0b10xx_xxxx) to find the lead byte.
            if byte & 0b1100_0000 == 0b1000_0000 {
                continue;
            }
            let expected = if byte & 0b1000_0000 == 0 {
                1
            } else if byte & 0b1110_0000 == 0b1100_0000 {
                2
            } else if byte & 0b1111_0000 == 0b1110_0000 {
                3
            } else if byte & 0b1111_1000 == 0b1111_0000 {
                4
            } else {
                // Not a valid lead byte: let lossy decoding handle it now.
                return 0;
            };
            return if back < expected { back } else { 0 };
        }
        0
    }

    struct OwnedHandle(HANDLE);

    impl OwnedHandle {
        fn new(handle: HANDLE) -> Self {
            Self(handle)
        }

        fn raw(&self) -> HANDLE {
            self.0
        }

        fn into_raw(self) -> HANDLE {
            let handle = self.0;
            std::mem::forget(self);
            handle
        }
    }

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if !self.0.is_invalid() && self.0 != INVALID_HANDLE_VALUE {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
    }

    unsafe impl Send for OwnedHandle {}

    // Needed so `PtySessionInner.input_write` can be `Arc<OwnedHandle>`
    // (`write`/`interrupt` clone the `Arc` across threads before releasing
    // the session lock; see Finding 4). Sound because `raw()` only hands out
    // a `Copy` `HANDLE` behind `&self` and the Win32 APIs used on it
    // (`WriteFile`) are safe to call concurrently from multiple threads on
    // the same handle; the only exclusive access, `Drop`/`into_raw`, already
    // requires `&mut self`/`self` and is governed by `Arc`'s own refcounting.
    unsafe impl Sync for OwnedHandle {}

    struct OwnedPseudoConsole(HPCON);

    impl Drop for OwnedPseudoConsole {
        fn drop(&mut self) {
            unsafe {
                ClosePseudoConsole(self.0);
            }
        }
    }

    unsafe impl Send for OwnedPseudoConsole {}

    struct SendHandle(HANDLE);

    impl SendHandle {
        fn into_inner(self) -> HANDLE {
            self.0
        }
    }

    unsafe impl Send for SendHandle {}

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::AtomicBool;

        #[test]
        fn test_create_breakaway_from_job_constant_value() {
            assert_eq!(CREATE_BREAKAWAY_FROM_JOB.0, 0x01000000);
        }

        /// Root-cause regression guard: the CONSOLE-SIGNAL leg of `interrupt()`,
        /// exercised in isolation, must actually terminate a running console
        /// command.
        ///
        /// Why this exists and why the end-to-end test in `tests/conpty_smoke.rs`
        /// is NOT sufficient on its own: `interrupt()` fires two independent legs
        /// (a raw `\x03` byte into the ConPTY input pipe, and a real
        /// `CTRL_C_EVENT` raised on the child's console) and deliberately reports
        /// success if EITHER works. The end-to-end test can only observe the
        /// combined outcome, so it stays green whenever *some* leg happens to
        /// work -- which is precisely how a completely dead console-signal leg
        /// shipped unnoticed. This test calls `send_console_interrupt` and
        /// NOTHING else: no `\x03` is written, so a regression in the signal path
        /// cannot be masked by the other leg.
        ///
        /// It pins both real defects that were found here:
        ///   1. Missing `FreeConsole()` before `AttachConsole()`, which made every
        ///      attach fail with ERROR_ACCESS_DENIED (this binary, like the Tauri
        ///      dev build, is already attached to its own console).
        ///   2. The inherited ignore-Ctrl+C attribute: children spawned by a host
        ///      whose own Ctrl+C is disabled inherit "ignore Ctrl+C", so the event
        ///      is raised successfully and then discarded by every console client.
        ///
        /// Note that (2) is invisible to a mere `result.is_ok()` assertion --
        /// `GenerateConsoleCtrlEvent` returns `Ok` either way. Only asserting that
        /// the child process is actually GONE catches it, which is why this test
        /// checks the effect on a real `ping -t` rather than the return value.
        #[test]
        fn console_signal_leg_alone_terminates_a_running_console_command() {
            let session = PtySession::spawn(
                "cmd.exe",
                &["/D", "/K"],
                TerminalSize {
                    columns: 80,
                    rows: 24,
                },
                |_id, _output| {},
                |_id| {},
            )
            .expect("should spawn PtySession");

            let root_process_id = {
                let guard = session.inner.lock().expect("session lock should be held");
                guard
                    .as_ref()
                    .expect("session should still be open")
                    .root_process_id
            };

            session
                .write("ping -t 127.0.0.1\r\n")
                .expect("long-running console command should start");

            // Wait for the real `ping.exe` grandchild to exist, so the assertion
            // below proves the signal killed it rather than it never having run.
            let mut ping_process_id = None;
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while ping_process_id.is_none() && std::time::Instant::now() < deadline {
                thread::sleep(Duration::from_millis(100));
                let snapshot = process_snapshot().expect("process snapshot should be readable");
                ping_process_id = descendants_of(root_process_id, &snapshot)
                    .iter()
                    .find(|process| process.name.to_ascii_uppercase().starts_with("PING"))
                    .map(|process| process.process_id);
            }
            let ping_process_id =
                ping_process_id.expect("`ping -t` should be running before the interrupt is sent");

            // The leg under test, and only it.
            let signal_result = send_console_interrupt(root_process_id);

            let mut ping_is_alive = true;
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while ping_is_alive && std::time::Instant::now() < deadline {
                thread::sleep(Duration::from_millis(100));
                ping_is_alive = process_snapshot()
                    .expect("process snapshot should be readable")
                    .iter()
                    .any(|process| process.process_id == ping_process_id);
            }

            let shell_survived = session.is_running().unwrap_or(false);

            session.close();

            signal_result.expect("the console-signal leg must not fail");
            assert!(
                !ping_is_alive,
                "the console-signal leg alone must terminate the running command: \
                 CTRL_C_EVENT was raised successfully on the child's console, yet ping.exe \
                 (pid {ping_process_id}) is still running. The signal is being ignored -- \
                 most likely the PTY child inherited the ignore-Ctrl+C attribute from this \
                 process (see the SetConsoleCtrlHandler call in spawn_process)"
            );
            assert!(
                shell_survived,
                "Ctrl+C must interrupt only the running command; the shell itself must survive"
            );
        }

        #[test]
        fn test_fallback_tree_termination_when_job_is_none() {
            use std::sync::mpsc;
            use std::time::{Duration, Instant};

            // Spawn cmd.exe to run a long-running ping command, as a child of the PTY session.
            let (tx, _rx) = mpsc::channel::<String>();
            let session = PtySession::spawn(
                "cmd.exe",
                &["/D", "/C", "ping -t 127.0.0.1 >NUL"],
                TerminalSize {
                    columns: 80,
                    rows: 24,
                },
                move |_id, out| {
                    let _ = tx.send(out);
                },
                |_id| {},
            )
            .expect("should spawn PtySession");

            // Sleep to let the grandchild (ping.exe) spawn.
            thread::sleep(Duration::from_millis(1000));

            // Find the grandchild PID.
            let root_pid = {
                let guard = session.inner.lock().unwrap();
                let inner = guard.as_ref().unwrap();
                inner.root_process_id
            };

            let snapshot = process_snapshot().expect("should take process snapshot");
            let descendants = descendants_of(root_pid, &snapshot);
            let grandchild = descendants
                .iter()
                .find(|p| p.name.to_lowercase().contains("ping"));

            assert!(
                grandchild.is_some(),
                "should find grandchild ping.exe process under root_pid {}",
                root_pid
            );
            let grandchild_pid = grandchild.unwrap().process_id;

            // Verify grandchild is alive.
            let alive_before = process_snapshot()
                .expect("should take process snapshot")
                .into_iter()
                .any(|p| p.process_id == grandchild_pid);
            assert!(alive_before, "grandchild should be alive before close()");

            // Simulate fallback by setting job to None.
            {
                let mut guard = session.inner.lock().unwrap();
                let inner = guard.as_mut().unwrap();
                let _job_to_drop = inner.job.take();
            }

            // Close session. This should trigger fallback tree termination since job is None.
            session.close();

            // Verify grandchild is killed.
            let mut alive_after = true;
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(5) {
                let snapshot = process_snapshot().expect("should take process snapshot");
                if !snapshot.into_iter().any(|p| p.process_id == grandchild_pid) {
                    alive_after = false;
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }

            assert!(
                !alive_after,
                "grandchild process {} should be terminated by fallback tree termination",
                grandchild_pid
            );
        }

        /// DEADLOCK GUARD for credit-based backpressure.
        ///
        /// With flow control in place, `on_output` is allowed to BLOCK (that is
        /// the whole point: a blocked reader stops calling `ReadFile`, the
        /// ConPTY pipe fills, and the child blocks on write). But `close()`
        /// joins the reader thread, so a reader parked inside `on_output`
        /// would wedge `close()` — and therefore `kill()` and app shutdown —
        /// forever.
        ///
        /// `spawn_with_close_hook` exists precisely to break that cycle:
        /// `close()` invokes the hook BEFORE it joins the reader, giving the
        /// consumer a deterministic chance to release whatever the reader is
        /// parked on. Here the reader is parked on a rendezvous `send` that
        /// nobody will ever receive; the hook drops the receiver, the `send`
        /// returns `Err`, and the reader can finish.
        ///
        /// Timing out (rather than hanging) is deliberate so a regression is a
        /// red test, not a wedged CI job.
        #[test]
        fn close_hook_unblocks_a_reader_parked_inside_on_output() {
            use std::sync::mpsc::{channel, sync_channel, Receiver};
            use std::time::Instant;

            // Rendezvous channel: the very first chunk `on_output` hands over
            // blocks the reader thread until someone receives it. Nobody does.
            let (tx, rx) = sync_channel::<String>(0);
            let receiver_slot: Arc<Mutex<Option<Receiver<String>>>> =
                Arc::new(Mutex::new(Some(rx)));
            let receiver_for_hook = Arc::clone(&receiver_slot);
            let parked = Arc::new(AtomicBool::new(false));
            let parked_for_reader = Arc::clone(&parked);

            let session = PtySession::spawn_with_close_hook(
                "cmd.exe",
                &["/D", "/K"],
                TerminalSize {
                    columns: 80,
                    rows: 24,
                },
                move |_id, output| {
                    parked_for_reader.store(true, Ordering::SeqCst);
                    let _ = tx.send(output);
                },
                |_id| {},
                move || {
                    // Dropping the receiver makes the parked `send` return Err.
                    if let Ok(mut slot) = receiver_for_hook.lock() {
                        drop(slot.take());
                    }
                },
            )
            .expect("session should spawn");

            // Wait until the reader is genuinely parked inside `on_output`, so
            // the assertion below proves `close()` unblocked it rather than the
            // reader never having reached the blocking call.
            let deadline = Instant::now() + Duration::from_secs(10);
            while !parked.load(Ordering::SeqCst) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            assert!(
                parked.load(Ordering::SeqCst),
                "the shell's first ConPTY output should have reached on_output"
            );

            let (done_tx, done_rx) = channel::<()>();
            thread::spawn(move || {
                session.close();
                let _ = done_tx.send(());
            });

            done_rx
                .recv_timeout(Duration::from_secs(15))
                .expect("close() must not wedge on a reader parked inside on_output");
        }

        #[test]
        fn select_process_tree_target_prefers_leaf_descendant() {
            let processes = vec![
                ProcessSnapshotEntry {
                    process_id: 10,
                    parent_process_id: 1,
                    name: "cmd.exe".to_owned(),
                },
                ProcessSnapshotEntry {
                    process_id: 20,
                    parent_process_id: 10,
                    name: "codex.exe".to_owned(),
                },
                ProcessSnapshotEntry {
                    process_id: 30,
                    parent_process_id: 20,
                    name: "node.exe".to_owned(),
                },
            ];

            assert_eq!(
                select_process_tree_target(10, &processes).map(|process| process.name.as_str()),
                Some("node.exe")
            );
        }

        #[test]
        fn select_process_tree_target_falls_back_to_root_process() {
            let processes = vec![ProcessSnapshotEntry {
                process_id: 10,
                parent_process_id: 1,
                name: "cmd.exe".to_owned(),
            }];

            assert_eq!(
                select_process_tree_target(10, &processes).map(|process| process.name.as_str()),
                Some("cmd.exe")
            );
        }

        #[test]
        fn select_process_tree_candidates_walk_from_leaf_to_root() {
            let processes = vec![
                ProcessSnapshotEntry {
                    process_id: 10,
                    parent_process_id: 1,
                    name: "cmd.exe".to_owned(),
                },
                ProcessSnapshotEntry {
                    process_id: 20,
                    parent_process_id: 10,
                    name: "codex.exe".to_owned(),
                },
                ProcessSnapshotEntry {
                    process_id: 30,
                    parent_process_id: 20,
                    name: "node.exe".to_owned(),
                },
            ];

            let names = select_process_tree_candidates(10, &processes)
                .into_iter()
                .map(|process| process.name.as_str())
                .collect::<Vec<_>>();

            assert_eq!(names, vec!["node.exe", "codex.exe", "cmd.exe"]);
        }

        #[test]
        fn incomplete_utf8_tail_len_holds_back_split_two_byte_sequence() {
            // "é" is 0xC3 0xA9. A lone lead byte must be held back.
            assert_eq!(incomplete_utf8_tail_len(&[b'a', 0xC3]), 1);
            // The completed sequence holds nothing back.
            assert_eq!(incomplete_utf8_tail_len(&[b'a', 0xC3, 0xA9]), 0);
        }

        #[test]
        fn incomplete_utf8_tail_len_handles_three_and_four_byte_sequences() {
            // "€" = E2 82 AC: 1 byte present -> hold 1, 2 present -> hold 2.
            assert_eq!(incomplete_utf8_tail_len(&[0xE2]), 1);
            assert_eq!(incomplete_utf8_tail_len(&[0xE2, 0x82]), 2);
            assert_eq!(incomplete_utf8_tail_len(&[0xE2, 0x82, 0xAC]), 0);
            // "😀" = F0 9F 98 80: 3 of 4 bytes present -> hold 3.
            assert_eq!(incomplete_utf8_tail_len(&[0xF0, 0x9F, 0x98]), 3);
            assert_eq!(incomplete_utf8_tail_len(&[0xF0, 0x9F, 0x98, 0x80]), 0);
        }

        #[test]
        fn incomplete_utf8_tail_len_ignores_ascii_and_invalid_tails() {
            assert_eq!(incomplete_utf8_tail_len(b"plain ascii"), 0);
            assert_eq!(incomplete_utf8_tail_len(&[]), 0);
            // A stray continuation byte is invalid, not incomplete.
            assert_eq!(incomplete_utf8_tail_len(&[b'a', 0x80]), 0);
            // An invalid lead byte is left for lossy decoding, not held back.
            assert_eq!(incomplete_utf8_tail_len(&[0xFF]), 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_size_requires_positive_dimensions() {
        assert_eq!(
            TerminalSize::new(0, 24),
            Err(TerminalSizeError::ZeroColumns)
        );
        assert_eq!(TerminalSize::new(80, 0), Err(TerminalSizeError::ZeroRows));
        assert_eq!(
            TerminalSize::new(80, 24),
            Ok(TerminalSize {
                columns: 80,
                rows: 24
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_interrupt_uses_process_group_fallback_while_writer_lock_is_held() {
        use std::{
            sync::mpsc,
            time::{Duration, Instant},
        };

        let (sender, receiver) = mpsc::channel();
        let session = PtySession::spawn(
            "/bin/sh",
            &[
                "-c",
                "trap 'printf interrupted; exit' INT; printf ready; while :; do :; done",
            ],
            TerminalSize::new(80, 24).unwrap(),
            move |_, output| {
                let _ = sender.send(output);
            },
            |_| {},
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut output = String::new();
        while Instant::now() < deadline && !output.contains("ready") {
            if let Ok(chunk) = receiver.recv_timeout(Duration::from_millis(50)) {
                output.push_str(&chunk);
            }
        }
        assert!(output.contains("ready"));
        assert_eq!(
            unsafe { libc::getpgid(session.pid.unwrap() as libc::pid_t) },
            session.pid.unwrap() as libc::pid_t
        );

        let writer = session.writer.lock().unwrap();
        let started = Instant::now();
        session.interrupt().unwrap();
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .contains("interrupted"));
        drop(writer);
        session.close();
    }
}
