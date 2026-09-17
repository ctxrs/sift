//! Execute argv directly with bounded output capture and optional completion wait.
use std::ffi::OsString;
#[cfg(not(any(unix, windows)))]
use std::io::Write;
use std::io::{self, IsTerminal, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::time::{Duration, Instant};

use retok::{CompactResult, Compactor};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

const LIMIT: usize = 8 * 1024 * 1024;
const WINDOW: Duration = Duration::from_millis(250);
const TICK: Duration = Duration::from_millis(10);

type Packet = (bool, io::Result<Vec<u8>>);

/// One child stream. Token counts exist only for a complete UTF-8 buffer that
/// was measured and successfully emitted. Inherited streams have no byte counts.
#[derive(Default)]
pub struct StreamObservation<'a> {
    pub original: Option<&'a [u8]>,
    pub compacted: Option<&'a CompactResult>,
    pub read_bytes: Option<u64>,
    pub emitted_bytes: Option<u64>,
}

pub struct Observation<'a> {
    pub stdout: StreamObservation<'a>,
    pub stderr: StreamObservation<'a>,
    pub duration: Duration,
    pub status: i32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    /// Inherit output unchanged. Takes precedence over capture.
    pub raw: bool,
    /// Wait for complete output, even with terminal stdin/stdout. Intended for
    /// finite commands: progress/prompts are delayed until both pipes close or
    /// the combined 8 MiB limit switches output to raw streaming. Stdin remains
    /// inherited; this does not allocate a pseudo-terminal for the child.
    pub capture: bool,
}

/// Run argv directly. Windows batch files use Rust's standard batch escaping;
/// unsupported batch arguments return an error rather than being reinterpreted.
#[allow(dead_code)] // Convenience entry point when the caller does not record observations.
pub fn run(args: &[OsString], raw: bool) -> anyhow::Result<i32> {
    run_with_options(
        args,
        Options {
            raw,
            capture: false,
        },
    )
}

#[allow(dead_code)] // Convenience entry point without observations.
pub fn run_with_options(args: &[OsString], options: Options) -> anyhow::Result<i32> {
    run_observed_with_options(args, options, |_| {})
}

/// Observe one completed invocation. The callback borrows bounded originals;
/// streaming/raw-inherited output never accumulates a retrievable transcript.
/// Setup/I/O errors returning Err do not invoke the callback. Spawn statuses
/// 126/127 do invoke it, with both streams unmeasured.
#[allow(dead_code)] // Preserve the original API for callers without options.
pub fn run_observed(
    args: &[OsString],
    raw: bool,
    observer: impl FnOnce(Observation<'_>),
) -> anyhow::Result<i32> {
    run_observed_with_options(
        args,
        Options {
            raw,
            capture: false,
        },
        observer,
    )
}

/// Like `run_observed`, with explicit complete-capture control. Unix signal
/// termination/cancellation returns numeric 128 + signal; it does not re-raise
/// the signal in the caller (normal exit and signal termination differ to wait()).
pub fn run_observed_with_options(
    args: &[OsString],
    options: Options,
    observer: impl FnOnce(Observation<'_>),
) -> anyhow::Result<i32> {
    run_transformed(args, options, |_, _| None, observer)
}

/// Apply an explicitly requested view only to complete bounded streams. A
/// custom view has no CompactResult/token accounting; overflow stays raw.
pub fn run_transformed(
    args: &[OsString],
    options: Options,
    mut transform: impl FnMut(&[u8], bool) -> Option<Vec<u8>>,
    observer: impl FnOnce(Observation<'_>),
) -> anyhow::Result<i32> {
    let started = Instant::now();
    let (program, args) = args
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("missing command"))?;
    let interactive = io::stdin().is_terminal() || io::stdout().is_terminal();
    let capture = !options.raw && (options.capture || !interactive);
    #[cfg(unix)]
    let signals = Signals::new()?;
    #[cfg(windows)]
    let windows = windows::State::new()?;
    #[cfg(windows)]
    let resolved = windows::resolve_program(program);
    #[cfg(windows)]
    let mut command = Command::new(&resolved);
    #[cfg(not(windows))]
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::inherit());
    if capture {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    } else {
        command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
    }
    #[cfg(unix)]
    if !interactive {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(windows_sys::Win32::System::Threading::CREATE_SUSPENDED);
    }
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            eprintln!("retok: {}: {error}", program.to_string_lossy());
            observer(Observation {
                stdout: Default::default(),
                stderr: Default::default(),
                duration: started.elapsed(),
                status: 127,
            });
            return Ok(127);
        }
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            eprintln!("retok: {}: {error}", program.to_string_lossy());
            observer(Observation {
                stdout: Default::default(),
                stderr: Default::default(),
                duration: started.elapsed(),
                status: 126,
            });
            return Ok(126);
        }
        Err(error) => return Err(error.into()),
    };
    let read_bytes: [Arc<AtomicU64>; 2] = Default::default();
    let emitted_bytes: [Arc<AtomicU64>; 2] = Default::default();
    let mut originals: Option<[Vec<u8>; 2]> = None;
    let mut compacted: [Option<CompactResult>; 2] = [None, None];
    let mut process = Process {
        emitted_bytes: emitted_bytes.clone(),
        child,
        #[cfg(unix)]
        signals,
        #[cfg(unix)]
        group: !interactive,
        #[cfg(windows)]
        windows,
        cancelled: None,
        reaped: false,
        complete: false,
    };
    #[cfg(windows)]
    process.windows.assign_and_resume(&process.child)?;
    let result = (|| -> anyhow::Result<i32> {
        let (tx, rx) = mpsc::sync_channel(8);
        if capture {
            reader(
                process.child.stdout.take().unwrap(),
                false,
                tx.clone(),
                read_bytes[0].clone(),
            );
            reader(
                process.child.stderr.take().unwrap(),
                true,
                tx.clone(),
                read_bytes[1].clone(),
            );
        }
        drop(tx);
        let mut pending = [Vec::new(), Vec::new()];
        let mut size = 0;
        let mut first = None;
        let mut passthrough = !capture;
        let mut eof = !capture;
        let mut status = None;
        loop {
            process.check()?;
            if status.is_none() {
                status = process.child.try_wait()?;
                process.reaped = status.is_some();
            }
            if status.is_some() && eof {
                break;
            }
            if !passthrough
                && !options.capture
                && first.is_some_and(|time: Instant| time.elapsed() >= WINDOW)
            {
                // A descendant can keep the pipes open after the direct child exits.
                // The same deadline applies until both pipes close.
                flush_pending(&mut process, &mut pending)?;
                passthrough = true;
            }
            if !capture || eof {
                std::thread::sleep(TICK);
                continue;
            }
            let timeout = if passthrough || options.capture {
                TICK
            } else {
                first.map_or(TICK, |time: Instant| {
                    WINDOW.saturating_sub(time.elapsed()).min(TICK)
                })
            };
            match rx.recv_timeout(timeout) {
                Ok((stderr, bytes)) => {
                    let bytes = bytes?;
                    first.get_or_insert_with(Instant::now);
                    if !passthrough && size + bytes.len() >= LIMIT {
                        flush_pending(&mut process, &mut pending)?;
                        passthrough = true;
                    }
                    if passthrough {
                        process.write(stderr, &bytes)?;
                    } else {
                        size += bytes.len();
                        pending[usize::from(stderr)].extend_from_slice(&bytes);
                    }
                }
                Err(RecvTimeoutError::Disconnected) => eof = true,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        if !passthrough {
            originals = Some(pending);
            let mut compactor = None;
            for (index, bytes) in originals.as_ref().unwrap().iter().enumerate() {
                process.check()?;
                if let Some(output) = transform(bytes, index == 1) {
                    process.write(index == 1, &output)?;
                    process.check()?;
                    continue;
                }
                // Tiny responses cannot amortize tokenizer startup or framing.
                let candidate = if bytes.len() >= 256 {
                    std::str::from_utf8(bytes).ok().and_then(|text| {
                        compactor
                            .get_or_insert_with(Compactor::new)
                            .as_ref()
                            .ok()
                            .map(|compactor| compactor.compact(text))
                    })
                } else {
                    None
                };
                process.write(
                    index == 1,
                    candidate
                        .as_ref()
                        .map_or(bytes.as_slice(), |result| result.text.as_bytes()),
                )?;
                compacted[index] = candidate;
                // Tokenizer work and empty output must not hide a pending signal.
                process.check()?;
            }
        }
        process.check()?;
        #[cfg(windows)]
        if status.unwrap().success() && process.cancelled.is_none() {
            // A successful launcher may intentionally leave background children.
            // Captured execution also reaches here only after both pipes close;
            // those descendants have released our output and may keep running.
            process.windows.preserve_descendants()?;
            process.check()?;
        }
        process.complete = true;
        Ok(exit_code(status.unwrap()))
    })();
    let result = process
        .cancelled
        .map_or(result, |(signal, _)| Ok(128 + signal));
    // Reap/terminate before invoking application code, which may itself do I/O.
    drop(process);
    let status = result?;
    let stream = |index: usize| StreamObservation {
        original: originals.as_ref().map(|streams| streams[index].as_slice()),
        compacted: compacted[index].as_ref(),
        read_bytes: capture.then(|| read_bytes[index].load(Ordering::Relaxed)),
        emitted_bytes: capture.then(|| emitted_bytes[index].load(Ordering::Relaxed)),
    };
    observer(Observation {
        stdout: stream(0),
        stderr: stream(1),
        duration: started.elapsed(),
        status,
    });
    Ok(status)
}

fn reader(
    mut input: impl Read + Send + 'static,
    stderr: bool,
    tx: SyncSender<Packet>,
    read_bytes: Arc<AtomicU64>,
) {
    std::thread::spawn(move || {
        let mut buffer = [0; 16 * 1024];
        loop {
            match input.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    read_bytes.fetch_add(n as u64, Ordering::Relaxed);
                    if tx.send((stderr, Ok(buffer[..n].to_vec()))).is_err() {
                        break;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let _ = tx.send((stderr, Err(error)));
                    break;
                }
            }
        }
    });
}

fn flush_pending(process: &mut Process, pending: &mut [Vec<u8>; 2]) -> io::Result<()> {
    for (index, bytes) in pending.iter_mut().enumerate() {
        process.write(index == 1, bytes)?;
        *bytes = Vec::new();
    }
    Ok(())
}

fn exit_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
    }
    #[cfg(not(unix))]
    {
        status.code().unwrap_or(1)
    }
}

struct Process {
    emitted_bytes: [Arc<AtomicU64>; 2],
    child: Child,
    #[cfg(unix)]
    signals: Signals,
    #[cfg(unix)]
    group: bool,
    #[cfg(windows)]
    windows: windows::State,
    cancelled: Option<(i32, Instant)>,
    reaped: bool,
    complete: bool,
}
impl Process {
    fn check(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::sync::atomic::Ordering;
            let signal = self.signals.pending.swap(0, Ordering::Relaxed) as i32;
            if signal != 0 {
                self.signal(signal);
                self.cancelled.get_or_insert((signal, Instant::now()));
            }
            if self
                .cancelled
                .is_some_and(|(_, time)| time.elapsed() >= Duration::from_secs(1))
            {
                self.signal(libc::SIGKILL);
            }
            for fd in [libc::STDOUT_FILENO, libc::STDERR_FILENO] {
                let mut poll = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: one initialized pollfd, valid for the duration of this call.
                let result = unsafe { libc::poll(&mut poll, 1, 0) };
                if result >= 0
                    && poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0
                {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "output consumer closed",
                    ));
                }
            }
        }
        #[cfg(windows)]
        {
            self.windows.check_cancelled(&mut self.cancelled);
        }
        Ok(())
    }
    #[cfg(unix)]
    fn signal(&self, signal: i32) {
        // A reaped PID can be reused. A private process group is still ours while
        // its descendants hold the captured pipes open.
        if self.reaped && !self.group {
            return;
        }
        let pid = self.child.id() as i32;
        // SAFETY: kill takes integer identifiers and does not access memory.
        unsafe {
            libc::kill(if self.group { -pid } else { pid }, signal);
        }
    }
    fn write(&mut self, stderr: bool, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            self.check()?;
            if self
                .cancelled
                .is_some_and(|(_, time)| time.elapsed() >= Duration::from_secs(1))
            {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "command cancelled",
                ));
            }
            #[cfg(unix)]
            {
                let fd = if stderr {
                    libc::STDERR_FILENO
                } else {
                    libc::STDOUT_FILENO
                };
                let mut poll = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // Poll before bounded writes so cancellation also works under backpressure.
                // SAFETY: pollfd and byte slice remain valid through their calls.
                let ready = unsafe { libc::poll(&mut poll, 1, 10) };
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                if ready == 0 {
                    continue;
                }
                #[allow(clippy::unnecessary_cast)] // libc uses c_int on Solaris.
                let pipe_buf = libc::PIPE_BUF as usize;
                let n =
                    unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len().min(pipe_buf)) };
                if n < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
                self.emitted_bytes[usize::from(stderr)].fetch_add(n as u64, Ordering::Relaxed);
                bytes = &bytes[n as usize..];
                if self
                    .cancelled
                    .is_some_and(|(_, time)| time.elapsed() >= Duration::from_secs(1))
                {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "command cancelled",
                    ));
                }
            }
            #[cfg(windows)]
            {
                self.write_windows(stderr, bytes)?;
                bytes = &[];
            }
            #[cfg(not(any(unix, windows)))]
            {
                if stderr {
                    io::stderr().write_all(bytes)?;
                    io::stderr().flush()?;
                } else {
                    io::stdout().write_all(bytes)?;
                    io::stdout().flush()?;
                }
                self.emitted_bytes[usize::from(stderr)]
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                bytes = &[];
            }
        }
        Ok(())
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        // Also runs on read/write errors and unwinding. Never leave a child alive
        // when the consumer closes its pipe.
        #[cfg(unix)]
        if !self.complete || self.cancelled.is_some() {
            self.signal(libc::SIGKILL);
        }
        #[cfg(windows)]
        if !self.complete || self.cancelled.is_some() {
            // Also covers cancellation racing successful job disarming.
            self.windows.terminate();
        }
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(unix)]
struct Signals {
    pending: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ids: Vec<signal_hook::SigId>,
}
#[cfg(unix)]
impl Signals {
    fn new() -> io::Result<Self> {
        let mut signals = Self {
            pending: Default::default(),
            ids: Vec::new(),
        };
        for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            signals.ids.push(signal_hook::flag::register_usize(
                signal,
                signals.pending.clone(),
                signal as usize,
            )?);
        }
        Ok(signals)
    }
}
#[cfg(unix)]
impl Drop for Signals {
    fn drop(&mut self) {
        for id in self.ids.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

#[cfg(windows)]
impl Process {
    fn write_windows(&mut self, stderr: bool, bytes: &[u8]) -> io::Result<()> {
        use std::io::Write;
        use windows_sys::Win32::Storage::FileSystem::WriteFile;
        use windows_sys::Win32::System::Console::{
            GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        };
        let bytes = bytes.to_vec();
        let emitted_bytes = self.emitted_bytes[usize::from(stderr)].clone();
        self.windows_io(move || {
            let mut remaining = bytes.as_slice();
            while !remaining.is_empty() {
                let n = if (stderr && io::stderr().is_terminal())
                    || (!stderr && io::stdout().is_terminal())
                {
                    // Capture can explicitly target a terminal. Retain Rust's
                    // Unicode console conversion on either output stream.
                    if stderr {
                        io::stderr().write(remaining)?
                    } else {
                        let n = io::stdout().write(remaining)?;
                        io::stdout().flush()?;
                        n
                    }
                } else {
                    let handle = unsafe {
                        GetStdHandle(if stderr {
                            STD_ERROR_HANDLE
                        } else {
                            STD_OUTPUT_HANDLE
                        })
                    };
                    let mut written = 0;
                    // SAFETY: a borrowed standard handle and a live byte slice;
                    // synchronous WriteFile initializes the count before returning.
                    // Avoid Stdout's line buffering/retries so cancellation of a
                    // blocked pipe write cannot be swallowed as an interrupted I/O.
                    if unsafe {
                        WriteFile(
                            handle,
                            remaining.as_ptr(),
                            remaining.len().min(64 * 1024) as u32,
                            &mut written,
                            std::ptr::null_mut(),
                        )
                    } == 0
                    {
                        return Err(io::Error::last_os_error());
                    }
                    written as usize
                };
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "output write returned zero",
                    ));
                }
                emitted_bytes.fetch_add(n as u64, Ordering::Relaxed);
                remaining = &remaining[n..];
            }
            Ok(())
        })
    }

    fn windows_io(
        &mut self,
        operation: impl FnOnce() -> io::Result<()> + Send + 'static,
    ) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::IO::CancelSynchronousIo;

        // Synchronous pipe writes can block. Windows reports a closed consumer
        // on the next write; quiet children are not polled for pipe closure.
        // Supervise them off-thread; repeat cancellation to cover the race just
        // before the worker enters the system call.
        let (tx, rx) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let _ = tx.send(operation());
        });
        let mut cancelled = false;
        let result = loop {
            self.windows.check_cancelled(&mut self.cancelled);
            if self
                .cancelled
                .is_some_and(|(_, time)| time.elapsed() >= Duration::from_secs(1))
            {
                cancelled = true;
                // SAFETY: the join handle owns this live worker thread's handle.
                unsafe {
                    CancelSynchronousIo(worker.as_raw_handle());
                }
            }
            match rx.recv_timeout(TICK) {
                Ok(result) => break result,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    break Err(io::Error::other("output worker stopped"));
                }
            }
        };
        let _ = worker.join();
        if cancelled {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "command cancelled",
            ))
        } else {
            result
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::ffi::OsStr;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, SetConsoleCtrlHandler,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

    pub(super) fn resolve_program(program: &OsStr) -> OsString {
        use std::path::Path;

        // Command finds native executables, but not extensionless batch shims.
        // Resolve only directly runnable Windows formats, in PATH/PATHEXT order;
        // script associations (e.g. .ps1) still require an explicit interpreter.
        // Hand the path and untouched argv to std, including its batch escaping.
        let Ok(cwd) = std::env::current_dir() else {
            return program.to_owned();
        };
        let path = Path::new(program);
        if path.file_name().is_none() {
            return program.to_owned();
        }
        let executable_extension = path.extension().is_some_and(|extension| {
            ["com", "exe", "bat", "cmd"].iter().any(|supported| {
                extension
                    .as_encoded_bytes()
                    .eq_ignore_ascii_case(supported.as_bytes())
            })
        });
        let pathext =
            std::env::var_os("PATHEXT").unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD"));
        let pathext = pathext.to_string_lossy();
        let extensions: Vec<_> = pathext
            .split(';')
            .filter(|ext| {
                [".com", ".exe", ".bat", ".cmd"]
                    .iter()
                    .any(|supported| ext.eq_ignore_ascii_case(supported))
            })
            .collect();
        let mut directories = vec![cwd.clone()];
        if path.components().count() == 1
            && let Some(search) = std::env::var_os("PATH")
        {
            directories.extend(std::env::split_paths(&search).map(|dir| cwd.join(dir)));
        }
        for directory in directories {
            let base = directory.join(path);
            if path.extension().is_some() && base.is_file() {
                return base.into_os_string();
            }
            if executable_extension {
                continue;
            }
            for extension in &extensions {
                let mut candidate = base.clone().into_os_string();
                candidate.push(extension);
                if Path::new(&candidate).is_file() {
                    return candidate;
                }
            }
        }
        // Preserve std's native executable lookup and spawn error classification.
        program.to_owned()
    }

    static ACTIVE: AtomicBool = AtomicBool::new(false);
    static INTERRUPTED: AtomicU32 = AtomicU32::new(0);

    unsafe extern "system" fn console_event(event: u32) -> i32 {
        if event == CTRL_C_EVENT || event == CTRL_BREAK_EVENT {
            INTERRUPTED.store(2, Ordering::Relaxed);
            1
        } else {
            // Keep Windows' default close/logoff/shutdown action. The kernel
            // closes our noninherited job handle even on forced termination.
            0
        }
    }

    pub(super) struct State {
        job: OwnedHandle,
    }
    impl State {
        pub(super) fn new() -> io::Result<Self> {
            if ACTIVE
                .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                return Err(io::Error::other("a command is already running"));
            }
            INTERRUPTED.store(0, Ordering::Relaxed);
            let result = (|| {
                // Null security attributes make the job handle noninheritable.
                let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: CreateJobObjectW returned an owned, valid handle.
                let job = unsafe { OwnedHandle::from_raw_handle(handle) };
                let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                // SAFETY: limits is correctly sized and initialized; handle is live.
                if unsafe {
                    SetInformationJobObject(
                        job.as_raw_handle(),
                        JobObjectExtendedLimitInformation,
                        (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        std::mem::size_of_val(&limits) as u32,
                    )
                } == 0
                {
                    return Err(io::Error::last_os_error());
                }
                if unsafe { SetConsoleCtrlHandler(Some(console_event), 1) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Self { job })
            })();
            if result.is_err() {
                ACTIVE.store(false, Ordering::Release);
            }
            result
        }

        pub(super) fn preserve_descendants(&self) -> io::Result<()> {
            // This private job has only KILL_ON_JOB_CLOSE set. Clear it only
            // after an uncancelled zero exit and successful output delivery.
            let limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            // SAFETY: the live job handle and initialized native structure remain
            // valid for the call. Failure leaves the caller's cleanup guard armed.
            if unsafe {
                SetInformationJobObject(
                    self.job.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub(super) fn terminate(&self) {
            // SAFETY: the state owns this live job handle.
            unsafe {
                TerminateJobObject(self.job.as_raw_handle(), 130);
            }
        }

        pub(super) fn assign_and_resume(&self, child: &Child) -> io::Result<()> {
            // std::Command retains argv quoting, cwd/env and stream inheritance.
            // CREATE_SUSPENDED prevents child code (and grandchildren) running
            // before assignment. Errors leave Process's guard to kill/reap it.
            // SAFETY: both handles are owned and remain live through the call.
            if unsafe { AssignProcessToJobObject(self.job.as_raw_handle(), child.as_raw_handle()) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            // std drops the initial thread handle. Its documented ToolHelp ID
            // lets us reopen it; no custom native command-line builder is needed.
            let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: the checked snapshot handle is newly owned.
            let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
            let mut entry = THREADENTRY32 {
                dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
                ..Default::default()
            };
            // SAFETY: entry is initialized to the API's native structure size.
            let mut found = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
            while found != 0 {
                if entry.th32OwnerProcessID == child.id() {
                    let thread =
                        unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                    if thread.is_null() {
                        return Err(io::Error::last_os_error());
                    }
                    // SAFETY: OpenThread returned a new live handle; the initial
                    // child thread remains suspended until ResumeThread succeeds.
                    let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
                    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                        return Err(io::Error::last_os_error());
                    }
                    return Ok(());
                }
                // SAFETY: the snapshot and writable entry remain live.
                found = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
            }
            Err(io::Error::other("cannot find suspended command thread"))
        }

        pub(super) fn check_cancelled(&self, cancelled: &mut Option<(i32, Instant)>) {
            if INTERRUPTED.load(Ordering::Relaxed) != 0 {
                // Shared-console children already receive Ctrl+C/Break from the
                // OS. Rebroadcasting it would also interrupt Retok's caller.
                cancelled.get_or_insert((2, Instant::now()));
            }
            if cancelled.is_some_and(|(_, time)| time.elapsed() >= Duration::from_secs(1)) {
                self.terminate();
            }
        }
    }

    impl Drop for State {
        fn drop(&mut self) {
            unsafe {
                SetConsoleCtrlHandler(Some(console_event), 0);
            }
            ACTIVE.store(false, Ordering::Release);
            // OwnedHandle closes the job. Cleanup remains armed unless a
            // successful invocation explicitly preserved background descendants.
        }
    }
}
