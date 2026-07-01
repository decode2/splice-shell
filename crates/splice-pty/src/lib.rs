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

#[cfg(not(windows))]
pub struct PtySession;

#[cfg(not(windows))]
impl PtySession {
    pub fn spawn<F>(
        _program: &str,
        _args: &[&str],
        _size: TerminalSize,
        _on_output: F,
    ) -> Result<Self, PtyError>
    where
        F: FnMut(String) + Send + 'static,
    {
        Err(PtyError::UnsupportedPlatform)
    }

    pub fn write(&self, _data: &str) -> Result<(), PtyError> {
        Err(PtyError::UnsupportedPlatform)
    }

    pub fn interrupt(&self) -> Result<(), PtyError> {
        Err(PtyError::UnsupportedPlatform)
    }

    pub fn resize(&self, _size: TerminalSize) -> Result<(), PtyError> {
        Err(PtyError::UnsupportedPlatform)
    }

    pub fn is_running(&self) -> Result<bool, PtyError> {
        Err(PtyError::UnsupportedPlatform)
    }

    pub fn active_process_name(&self) -> Result<String, PtyError> {
        Err(PtyError::UnsupportedPlatform)
    }

    pub fn active_process_candidates(&self) -> Result<Vec<String>, PtyError> {
        Err(PtyError::UnsupportedPlatform)
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
mod windows_conpty {
    use super::{PtyError, TerminalSize};
    use std::{
        ffi::c_void,
        mem::size_of,
        ptr::null_mut,
        sync::Mutex,
        thread::{self, JoinHandle},
        time::Duration,
    };
    use windows::{
        core::PWSTR,
        Win32::{
            Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE},
            Storage::FileSystem::{ReadFile, WriteFile},
            System::{
                Console::{
                    AttachConsole, ClosePseudoConsole, CreatePseudoConsole, FreeConsole,
                    GenerateConsoleCtrlEvent, ResizePseudoConsole, SetConsoleCtrlHandler, COORD,
                    CTRL_C_EVENT, HPCON,
                },
                Diagnostics::ToolHelp::{
                    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                    TH32CS_SNAPPROCESS,
                },
                Pipes::CreatePipe,
                Threading::{
                    CreateProcessW, DeleteProcThreadAttributeList,
                    InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
                    WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT,
                    INFINITE, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION, PROCESS_TERMINATE,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
                },
            },
        },
    };

    pub struct PtySession {
        inner: Mutex<Option<PtySessionInner>>,
    }

    struct PtySessionInner {
        input_write: OwnedHandle,
        process: OwnedHandle,
        _process_thread: OwnedHandle,
        conpty: OwnedPseudoConsole,
        reader: Option<JoinHandle<()>>,
        root_process_id: u32,
        root_process_name: String,
    }

    impl PtySession {
        pub fn spawn<F>(
            program: &str,
            args: &[&str],
            size: TerminalSize,
            mut on_output: F,
        ) -> Result<Self, PtyError>
        where
            F: FnMut(String) + Send + 'static,
        {
            let handles = spawn_process(program, args, size)?;
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
                        on_output(String::from_utf8_lossy(&pending[..split]).into_owned());
                        pending.drain(..split);
                    }
                });
                // Flush trailing bytes at EOF; an incomplete sequence here will
                // never complete, so decode it lossily rather than drop it.
                if !pending.is_empty() {
                    on_output(String::from_utf8_lossy(&pending).into_owned());
                }
            });

            Ok(Self {
                inner: Mutex::new(Some(PtySessionInner {
                    input_write: handles.input_write,
                    process: handles.process,
                    _process_thread: handles.process_thread,
                    conpty: handles.conpty,
                    reader: Some(reader),
                    root_process_id: handles.process_id,
                    root_process_name: program.to_owned(),
                })),
            })
        }

        pub fn write(&self, data: &str) -> Result<(), PtyError> {
            let guard = self
                .inner
                .lock()
                .map_err(|_| PtyError::Io(std::io::Error::other("PTY session lock poisoned")))?;
            let inner = guard.as_ref().ok_or(PtyError::SessionClosed)?;
            if !inner.is_running() {
                return Err(PtyError::SessionClosed);
            }

            write_all(inner.input_write.raw(), data.as_bytes())
        }

        pub fn interrupt(&self) -> Result<(), PtyError> {
            let guard = self
                .inner
                .lock()
                .map_err(|_| PtyError::Io(std::io::Error::other("PTY session lock poisoned")))?;
            let inner = guard.as_ref().ok_or(PtyError::SessionClosed)?;
            if !inner.is_running() {
                return Err(PtyError::SessionClosed);
            }

            let input_result = write_all(inner.input_write.raw(), b"\x03");
            let signal_result = send_console_interrupt(inner.root_process_id);

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

        pub fn wait(&self) -> Result<(), PtyError> {
            let guard = self
                .inner
                .lock()
                .map_err(|_| PtyError::Io(std::io::Error::other("PTY session lock poisoned")))?;
            let inner = guard.as_ref().ok_or(PtyError::SessionClosed)?;

            unsafe {
                WaitForSingleObject(inner.process.raw(), INFINITE);
            }

            Ok(())
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
            if let Ok(mut guard) = self.inner.lock() {
                if let Some(mut inner) = guard.take() {
                    unsafe {
                        let _ = terminate_process_tree(inner.root_process_id);
                        let _ = TerminateProcess(inner.process.raw(), 0);
                        WaitForSingleObject(inner.process.raw(), INFINITE);
                    }
                    drop(inner.input_write);
                    drop(inner.conpty);
                    if let Some(reader) = inner.reader.take() {
                        let _ = reader.join();
                    }
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

    fn send_console_interrupt(process_id: u32) -> Result<(), PtyError> {
        unsafe {
            AttachConsole(process_id)?;
            let _console_guard = AttachedConsoleGuard;
            SetConsoleCtrlHandler(None, true)?;
            let _handler_guard = ConsoleCtrlHandlerGuard;
            GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0)?;
        }

        Ok(())
    }

    struct AttachedConsoleGuard;

    impl Drop for AttachedConsoleGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = FreeConsole();
            }
        }
    }

    struct ConsoleCtrlHandlerGuard;

    impl Drop for ConsoleCtrlHandlerGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = SetConsoleCtrlHandler(None, false);
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
    }

    fn spawn_process(
        program: &str,
        args: &[&str],
        size: TerminalSize,
    ) -> Result<SpawnedProcess, PtyError> {
        let mut command_line = command_line(program, args)?;
        let mut input_read = HANDLE::default();
        let mut input_write = HANDLE::default();
        let mut output_read = HANDLE::default();
        let mut output_write = HANDLE::default();

        unsafe {
            CreatePipe(&mut input_read, &mut input_write, None, 0)?;
            CreatePipe(&mut output_read, &mut output_write, None, 0)?;
        }

        let input_read = OwnedHandle::new(input_read);
        let input_write = OwnedHandle::new(input_write);
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
        unsafe {
            CreateProcessW(
                None,
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                false,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                None,
                None,
                &startup_info.StartupInfo,
                &mut process_info,
            )?;
        }

        Ok(SpawnedProcess {
            input_write,
            output_read,
            process: OwnedHandle::new(process_info.hProcess),
            process_thread: OwnedHandle::new(process_info.hThread),
            process_id: process_info.dwProcessId,
            conpty,
        })
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

            let mut startup_info = STARTUPINFOEXW::default();
            startup_info.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup_info.StartupInfo.hStdInput.0 = null_mut();
            startup_info.StartupInfo.hStdOutput.0 = null_mut();
            startup_info.StartupInfo.hStdError.0 = null_mut();
            startup_info.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
            startup_info.lpAttributeList = attribute_list;

            Ok(Self {
                _storage: storage,
                startup_info,
            })
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
}
