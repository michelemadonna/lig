use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use super::config::{Backend, Engine};

pub struct Arguments<'a> {
    pub file: &'a Path,
    pub cwd: &'a Path,
    pub search_path: Option<&'a str>,
    pub width: u16,
    pub height: u16,
    pub max_lines: usize,
    pub extension: &'a str,
}

impl Arguments<'_> {
    fn values(&self) -> [(&'static str, &'static str, OsString); 5] {
        [
            ("{file}", "LIG_PREVIEW_FILE", self.file.as_os_str().into()),
            ("{width}", "LIG_PREVIEW_WIDTH", self.width.to_string().into()),
            ("{height}", "LIG_PREVIEW_HEIGHT", self.height.to_string().into()),
            ("{max_lines}", "LIG_PREVIEW_MAX_LINES", self.max_lines.to_string().into()),
            ("{extension}", "LIG_PREVIEW_EXT", self.extension.into()),
        ]
    }
}

fn expand(mut argument: &str, values: &[(&str, &str, OsString)]) -> OsString {
    let mut out = OsString::new();
    while let Some(index) = argument.find('{') {
        let (before, tail) = argument.split_at(index);
        out.push(before);
        if let Some((key, _, value)) = values.iter().find(|(key, ..)| tail.starts_with(key)) {
            out.push(value);
            argument = &tail[key.len()..];
        } else {
            out.push("{");
            argument = &tail[1..];
        }
    }
    out.push(argument);
    out
}

pub fn command(engine: &Engine, arguments: &Arguments<'_>) -> anyhow::Result<Command> {
    let values = arguments.values();
    let mut command = match engine.backend {
        Backend::Command => {
            let (exe, args) = engine.command.split_first().ok_or_else(|| anyhow::anyhow!("missing command"))?;
            anyhow::ensure!(!exe.contains("{file}"), "the executable must be explicitly configured");
            checked_program(exe.as_ref())?;
            let mut cmd = Command::new(exe);
            cmd.args(args.iter().map(|arg| expand(arg, &values)));
            cmd
        }
        Backend::Shell => {
            let interpreter =
                engine.interpreter.as_deref().unwrap_or(if cfg!(windows) { "powershell.exe" } else { "sh" });
            checked_program(interpreter.as_ref())?;
            let script = engine.script.as_deref().ok_or_else(|| anyhow::anyhow!("missing script"))?;
            let name = Path::new(interpreter).file_stem().and_then(OsStr::to_str).unwrap_or("").to_ascii_lowercase();
            let mut cmd = Command::new(interpreter);
            if matches!(name.as_str(), "pwsh" | "powershell") {
                let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
                cmd.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-EncodedCommand"])
                    .arg(crate::remote::base64(&utf16));
            } else {
                cmd.arg("-c").arg(script);
            }
            cmd
        }
        Backend::Native => anyhow::bail!("native preview has no external command"),
    };
    command.current_dir(arguments.cwd);
    if let Some(path) = arguments.search_path {
        command.env("PATH", path);
    }
    command.envs(values.iter().map(|(_, key, value)| (key, value)));
    Ok(command)
}

fn checked_program(_path: &OsStr) -> anyhow::Result<()> {
    #[cfg(windows)]
    if let Some(ext) = Path::new(_path).extension().and_then(OsStr::to_str) {
        // CreateProcess routes batch files through cmd.exe, which violates argv mode.
        anyhow::ensure!(
            ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("com"),
            "preview executable on Windows must be an .exe or .com"
        );
    }
    Ok(())
}

/// Read and write the pipes without blocking or detached reader threads. A process that
/// leaves the group or inherits a pipe cannot keep the preview worker waiting indefinitely.
pub fn run(
    mut cmd: Command,
    input: Option<&[u8]>,
    limit: usize,
    timeout: Duration,
    cancelled: &dyn Fn() -> bool,
) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(!cancelled(), "cancelled");
    #[cfg(windows)]
    let input: Option<&[u8]> = {
        // Windows anonymous pipes have no nonblocking writer. A seekable file
        // keeps the same child and timeout handling with bounded input.
        use std::io::Seek;
        if let Some(bytes) = input {
            let mut file = tempfile::tempfile()?;
            file.write_all(bytes)?;
            file.rewind()?;
            cmd.stdin(Stdio::from(file));
        } else {
            cmd.stdin(Stdio::null());
        }
        None
    };
    #[cfg(unix)]
    cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    platform::prepare(&mut cmd);
    let mut process = platform::Process::spawn(cmd)?;
    let child = &mut process.child;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let mut stdin = child.stdin.take();
    let mut input = input.unwrap_or_default();
    platform::configure(&stdout)?;
    platform::configure(&stderr)?;
    if let Some(pipe) = &stdin {
        platform::configure(pipe)?;
    }

    let deadline = Instant::now() + timeout;
    let mut output = Vec::new();
    let mut err_len = 0usize;
    let mut out_eof = false;
    let mut err_eof = false;
    let mut buffer = [0; 8192];
    loop {
        anyhow::ensure!(!cancelled(), "cancelled");
        anyhow::ensure!(Instant::now() < deadline, "timed out");
        let exited = process.exited()?;
        if exited {
            // Kill remaining children while the leader still owns the process group ID.
            process.terminate();
        }
        let (out, err) = (
            pull(&mut stdout, &mut buffer, &mut out_eof, |bytes| {
                anyhow::ensure!(output.len() + bytes.len() <= limit, "stdout exceeded configured limit");
                output.extend_from_slice(bytes);
                Ok(())
            })?,
            pull(&mut stderr, &mut buffer, &mut err_eof, |bytes| {
                err_len += bytes.len();
                anyhow::ensure!(err_len <= limit.min(65_536), "stderr exceeded capture limit");
                Ok(())
            })?,
        );
        if input.is_empty() {
            stdin = None;
        } else if let Some(pipe) = &mut stdin {
            match pipe.write(&input[..input.len().min(4096)]) {
                Ok(n) => input = &input[n..],
                Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => stdin = None,
                Err(e) => return Err(e.into()),
            }
        }
        if exited && !out && !err {
            let status = process.wait()?;
            anyhow::ensure!(status.success(), "{status} ({err_len} stderr bytes)");
            return Ok(output);
        }
        if !(out || err) {
            std::thread::sleep(Duration::from_millis(4));
        }
    }
}

fn pull<T: platform::Pipe>(
    pipe: &mut T,
    buf: &mut [u8],
    eof: &mut bool,
    use_bytes: impl FnOnce(&[u8]) -> anyhow::Result<()>,
) -> anyhow::Result<bool> {
    if *eof {
        return Ok(false);
    }
    match platform::read(pipe, buf) {
        Ok(0) => *eof = true,
        Ok(n) => {
            use_bytes(&buf[..n])?;
            return Ok(true);
        }
        Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
        Err(e) => return Err(e.into()),
    }
    Ok(false)
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::{fd::AsRawFd, unix::process::CommandExt};

    pub trait Pipe: Read + AsRawFd {}
    impl<T: Read + AsRawFd> Pipe for T {}

    pub fn configure(pipe: &impl AsRawFd) -> io::Result<()> {
        let fd = pipe.as_raw_fd();
        // SAFETY: The handle is a live, owned std process pipe.
        if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn read(pipe: &mut impl Pipe, buf: &mut [u8]) -> io::Result<usize> {
        pipe.read(buf)
    }

    pub fn prepare(cmd: &mut Command) {
        cmd.process_group(0);
    }

    pub struct Process {
        pub child: Child,
        terminated: bool,
    }

    impl Process {
        pub fn spawn(mut cmd: Command) -> io::Result<Self> {
            Ok(Self { child: cmd.spawn()?, terminated: false })
        }

        pub fn exited(&mut self) -> io::Result<bool> {
            // SAFETY: waitid fills a valid siginfo_t. WNOWAIT reserves the process group ID.
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
            if unsafe { libc::waitid(libc::P_PID, self.child.id() as libc::id_t, &mut info, flags) } < 0 {
                let err = io::Error::last_os_error();
                return if err.kind() == io::ErrorKind::Interrupted { Ok(false) } else { Err(err) };
            }
            Ok(info.si_signo != 0)
        }

        pub fn terminate(&mut self) {
            if !std::mem::replace(&mut self.terminated, true) {
                // SAFETY: The group belongs to our unreaped child (created with process_group(0)).
                unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
            }
        }

        pub fn wait(&mut self) -> io::Result<ExitStatus> {
            self.child.wait()
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            self.terminate();
            let _ = self.child.wait();
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::{
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::CommandExt,
    };
    use windows_sys::Win32::{
        Foundation::{ERROR_BROKEN_PIPE, ERROR_NO_DATA, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
            },
            JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
                TerminateJobObject,
            },
            Pipes::PeekNamedPipe,
            Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
        },
    };

    pub trait Pipe: Read + AsRawHandle {}
    impl<T: Read + AsRawHandle> Pipe for T {}

    pub fn configure(_: &impl AsRawHandle) -> io::Result<()> {
        Ok(())
    }

    pub fn read(pipe: &mut impl Pipe, buf: &mut [u8]) -> io::Result<usize> {
        let mut available = 0;
        // SAFETY: The anonymous pipe handle is live and only this worker reads it.
        let ok = unsafe {
            PeekNamedPipe(
                pipe.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            let e = io::Error::last_os_error();
            return if matches!(e.raw_os_error(), Some(code) if code == ERROR_BROKEN_PIPE as i32 || code == ERROR_NO_DATA as i32)
            {
                Ok(0)
            } else {
                Err(e)
            };
        }
        if available == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let n = buf.len().min(available as usize);
        pipe.read(&mut buf[..n])
    }

    pub fn prepare(cmd: &mut Command) {
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
    }

    fn resume(pid: u32) -> io::Result<()> {
        // std::process keeps the process handle, but not the suspended primary
        // thread handle. The new process has one thread until it is resumed.
        // SAFETY: A successful snapshot is an owned, non-inheritable HANDLE.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };
        let size = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut entry = THREADENTRY32 { dwSize: size, ..Default::default() };
        // SAFETY: The snapshot handle and initialized entry are valid for both calls.
        let mut more = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
        while more != 0 {
            if entry.th32OwnerProcessID == pid {
                let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if thread.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let thread = unsafe { OwnedHandle::from_raw_handle(thread) };
                if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                    return Err(io::Error::last_os_error());
                }
                return Ok(());
            }
            entry.dwSize = size;
            more = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
        }
        Err(io::Error::last_os_error())
    }

    pub struct Process {
        pub child: Child,
        job: OwnedHandle,
        terminated: bool,
    }

    impl Process {
        pub fn spawn(mut cmd: Command) -> io::Result<Self> {
            // SAFETY: Default security attributes, unnamed job, and exclusive handle ownership.
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = unsafe { OwnedHandle::from_raw_handle(job) };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: The info class and structure match; the input is copied during the call.
            if unsafe {
                SetInformationJobObject(
                    job.as_raw_handle(),
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    std::mem::size_of_val(&limits) as u32,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            let mut child = cmd.spawn()?;
            // SAFETY: Both handles are live, and the child cannot start descendants
            // before assignment because CreateProcess left its first thread suspended.
            let assigned = unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) };
            let ready = if assigned == 0 { Err(io::Error::last_os_error()) } else { resume(child.id()) };
            if let Err(error) = ready {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Ok(Self { child, job, terminated: false })
        }

        pub fn exited(&mut self) -> io::Result<bool> {
            self.child.try_wait().map(|status| status.is_some())
        }

        pub fn terminate(&mut self) {
            if !std::mem::replace(&mut self.terminated, true) {
                // SAFETY: The job contains only the preview process and its descendants.
                unsafe { TerminateJobObject(self.job.as_raw_handle(), 1) };
            }
        }

        pub fn wait(&mut self) -> io::Result<ExitStatus> {
            self.child.wait()
        }
    }

    impl Drop for Process {
        fn drop(&mut self) {
            self.terminate();
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
