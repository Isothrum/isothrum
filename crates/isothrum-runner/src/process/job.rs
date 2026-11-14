//! A tool started inside a job object of its own (Windows), the crate's only `unsafe`.
//!
//! `process_wrap`'s `JobObject` created a tool suspended, put it in its job, then resumed it: a
//! process killed in between (Task Manager's End task, a crash, `TerminateProcess`) left the tool
//! suspended in no job, with nothing to resume or end it. Here the kernel creates the tool in its
//! job (`PROC_THREAD_ATTRIBUTE_JOB_LIST`), so there is no moment it is outside it, and the job
//! ends the tool's whole tree when its last handle closes (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`):
//! a tool cut short, and every tool of a process that dies however it dies, end with their trees.
//! A tool that exited on its own leaves what it started alone: the limit is cleared before the
//! job is closed. The tool inherits its own three standard handles only
//! (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`), never another handle of this process.
//!
//! Each `unsafe` block is a Win32 call on handles and buffers owned here, with a `SAFETY:` comment
//! saying why it is sound; everything else in the crate keeps `unsafe_code` denied.

use std::ffi::{OsStr, OsString, c_void};
use std::future::Future;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsHandle as _, AsRawHandle as _, BorrowedHandle, FromRawHandle as _, OwnedHandle};
use std::os::windows::process::ExitStatusExt as _;
use std::pin::Pin;
use std::process::ExitStatus;
use std::sync::Arc;

use process_wrap::tokio::ChildWrapper;
use tokio::process::{ChildStderr, ChildStdin, ChildStdout};
use windows::Win32::Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::JobObjects::{
    CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    TerminateJobObject,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE,
    InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

use super::system::Launch;
use super::win_command;

/// Starts `launch` in a new kill-on-close job, headless, its output piped.
pub(super) fn spawn(mut launch: Launch) -> io::Result<Box<dyn ChildWrapper>> {
    let vars = win_command::environment(std::env::vars_os(), &launch.env);
    let program = win_command::resolve(&launch.program, win_command::path_of(&vars), std::path::Path::is_file)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{} isn't on PATH", launch.program)))?;
    let stdin: OwnedHandle = match launch.stdin.take() {
        Some(file) => file.into(),
        None => std::fs::File::open("NUL")?.into(),
    };
    let (stdout, stdout_end) = std::io::pipe()?;
    let (stderr, stderr_end) = std::io::pipe()?;
    let ends: [OwnedHandle; 3] = [stdin, stdout_end.into(), stderr_end.into()];
    let job = Job::new()?;
    let created = create(&program, &launch, &vars, &job, &ends)?;
    // The tool holds its own ends now: this process's copies go, so its output ends with it.
    drop(ends);
    Ok(Box::new(JobChild {
        pid: created.pid,
        process: Arc::new(created.process),
        job,
        stdout: Some(ChildStdout::from_std(std::process::ChildStdout::from(OwnedHandle::from(stdout)))?),
        stderr: Some(ChildStderr::from_std(std::process::ChildStderr::from(OwnedHandle::from(stderr)))?),
        stdin: None,
        exited: None,
    }))
}

/// A process created: its id and handle.
struct Created {
    pid: u32,
    process: OwnedHandle,
}

/// `s` as a NUL-terminated UTF-16 string.
fn wide(s: &OsStr) -> Vec<u16> {
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// The environment block of `vars`: `name=value` strings, each NUL-terminated, then a NUL.
fn block(vars: &[(OsString, OsString)]) -> Vec<u16> {
    let mut out = Vec::new();
    for (name, value) in vars {
        out.extend(name.encode_wide());
        out.push(u16::from(b'='));
        out.extend(value.encode_wide());
        out.push(0);
    }
    if vars.is_empty() {
        out.push(0);
    }
    out.push(0);
    out
}

/// Creates the tool `program` of `launch` in `job`, `ends` (its stdin, stdout and stderr) the only
/// handles it inherits.
fn create(program: &std::path::Path, launch: &Launch, vars: &[(OsString, OsString)], job: &Job, ends: &[OwnedHandle; 3]) -> io::Result<Created> {
    for end in ends {
        inheritable(end)?;
    }
    let application = wide(program.as_os_str());
    let mut line = wide(OsStr::new(&win_command::command_line(&program.to_string_lossy(), &launch.args)));
    let environment = block(vars);
    let cwd = launch.cwd.as_deref().map(|d| wide(d.as_os_str()));
    let jobs = [raw(&job.0)];
    let inherited = ends.each_ref().map(raw);
    let mut attributes = Attributes::new(2)?;
    attributes.set(PROC_THREAD_ATTRIBUTE_JOB_LIST, &jobs)?;
    attributes.set(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &inherited)?;
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = u32::try_from(std::mem::size_of::<STARTUPINFOEXW>()).map_err(io::Error::other)?;
    startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    let [stdin, stdout, stderr] = inherited;
    startup.StartupInfo.hStdInput = stdin;
    startup.StartupInfo.hStdOutput = stdout;
    startup.StartupInfo.hStdError = stderr;
    startup.lpAttributeList = attributes.list();
    let mut info = PROCESS_INFORMATION::default();
    let flags = CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT;
    let cwd_ptr = cwd.as_ref().map_or_else(PCWSTR::null, |d| PCWSTR(d.as_ptr()));
    #[expect(unsafe_code, reason = "CreateProcessW: the call that creates the tool in its job")]
    // SAFETY: every pointer is to a buffer alive for the call (`application`, `line`, which the
    // call may write to, `environment`, `cwd`, `startup` and its attribute list, which `jobs` and
    // `inherited` back); the handles in them are open, owned by `job` and `ends`. On success
    // `info` holds two handles the call opened for this process, owned below.
    let created = unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            true,
            flags,
            Some(environment.as_ptr().cast::<c_void>()),
            cwd_ptr,
            &raw const startup.StartupInfo,
            &raw mut info,
        )
    };
    created?;
    let thread = own(info.hThread);
    drop(thread);
    Ok(Created {
        pid: info.dwProcessId,
        process: own(info.hProcess),
    })
}

/// The raw handle of `h`, for a Win32 structure.
fn raw(h: &OwnedHandle) -> HANDLE {
    HANDLE(h.as_raw_handle())
}

/// Takes ownership of `h`, a handle a Win32 call just opened for this process.
fn own(h: HANDLE) -> OwnedHandle {
    #[expect(unsafe_code, reason = "a handle CreateProcessW or CreateJobObjectW opened, owned from here on")]
    // SAFETY: `h` was just opened for this process by the call that returned it, and nothing else
    // owns or closes it.
    unsafe {
        OwnedHandle::from_raw_handle(h.0)
    }
}

/// Lets a process created with handle inheritance inherit `h`.
fn inheritable(h: &OwnedHandle) -> io::Result<()> {
    #[expect(unsafe_code, reason = "SetHandleInformation on a handle owned here")]
    // SAFETY: `h` is an open handle this process owns; only its inherit flag changes.
    let set = unsafe { SetHandleInformation(raw(h), HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT) };
    set.map_err(io::Error::from)
}

/// A process attribute list (`STARTUPINFOEXW`'s), deleted when dropped.
struct Attributes {
    /// Its buffer, of the size Windows asked for (`usize`s, for the alignment it needs).
    buffer: Vec<usize>,
}

impl Attributes {
    /// A list room for `count` attributes.
    fn new(count: u32) -> io::Result<Attributes> {
        let mut size = 0usize;
        #[expect(unsafe_code, reason = "InitializeProcThreadAttributeList, sizing call")]
        // SAFETY: the sizing call: no list, only `size` written. It "fails" with the size needed.
        let _sized = unsafe { InitializeProcThreadAttributeList(None, count, None, &raw mut size) };
        let mut buffer = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
        #[expect(unsafe_code, reason = "InitializeProcThreadAttributeList on a buffer owned here")]
        // SAFETY: `buffer` is at least `size` bytes, aligned for a pointer, and lives as long as
        // the list (it is the list's).
        let made = unsafe { InitializeProcThreadAttributeList(Some(LPPROC_THREAD_ATTRIBUTE_LIST(buffer.as_mut_ptr().cast())), count, None, &raw mut size) };
        made?;
        Ok(Attributes { buffer })
    }

    fn list(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.buffer.as_mut_ptr().cast())
    }

    /// Sets `attribute` to the handles `value`, which must outlive the list's use.
    fn set(&mut self, attribute: u32, value: &[HANDLE]) -> io::Result<()> {
        let list = self.list();
        let attribute = usize::try_from(attribute).map_err(io::Error::other)?;
        #[expect(unsafe_code, reason = "UpdateProcThreadAttribute on the list owned here")]
        // SAFETY: `list` was initialized by `new`; `value` points to `value.len()` handles, which
        // the caller keeps alive until the process is created.
        let set = unsafe { UpdateProcThreadAttribute(list, 0, attribute, Some(value.as_ptr().cast()), std::mem::size_of_val(value), None, None) };
        set.map_err(io::Error::from)
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        let list = self.list();
        #[expect(unsafe_code, reason = "DeleteProcThreadAttributeList on the list owned here")]
        // SAFETY: `list` was initialized by `new` and is deleted once, its buffer still alive.
        unsafe {
            DeleteProcThreadAttributeList(list);
        }
    }
}

/// A job object, ending every process in it when its last handle closes until released.
#[derive(Debug)]
struct Job(OwnedHandle);

impl Job {
    fn new() -> io::Result<Job> {
        #[expect(unsafe_code, reason = "CreateJobObjectW: a new, unnamed job")]
        // SAFETY: no attributes, no name: a plain call that returns a new handle or an error.
        let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }?;
        let job = Job(own(job));
        job.kill_on_close(true)?;
        Ok(job)
    }

    /// Sets or clears `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
    fn kill_on_close(&self, on: bool) -> io::Result<()> {
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        if on {
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        }
        let size = u32::try_from(std::mem::size_of_val(&info)).map_err(io::Error::other)?;
        #[expect(unsafe_code, reason = "SetInformationJobObject on the job owned here")]
        // SAFETY: `info` is a whole JOBOBJECT_EXTENDED_LIMIT_INFORMATION of `size` bytes, read
        // during the call only; the job handle is open.
        let set = unsafe { SetInformationJobObject(raw(&self.0), JobObjectExtendedLimitInformation, (&raw const info).cast(), size) };
        set.map_err(io::Error::from)
    }

    /// Ends every process in the job.
    fn terminate(&self) -> io::Result<()> {
        #[expect(unsafe_code, reason = "TerminateJobObject on the job owned here")]
        // SAFETY: the job handle is open.
        let ended = unsafe { TerminateJobObject(raw(&self.0), 1) };
        ended.map_err(io::Error::from)
    }
}

/// A tool started in its job, waited for on its process handle.
#[derive(Debug)]
struct JobChild {
    pid: u32,
    process: Arc<OwnedHandle>,
    job: Job,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    /// How it exited, once seen.
    exited: Option<ExitStatus>,
}

/// Waits `ms` milliseconds at most for `process` to exit: how it exited, or none yet.
fn wait_for(process: &OwnedHandle, ms: u32) -> io::Result<Option<ExitStatus>> {
    #[expect(unsafe_code, reason = "WaitForSingleObject on a process handle owned here")]
    // SAFETY: the process handle is open for the call.
    let waited = unsafe { WaitForSingleObject(raw(process), ms) };
    if waited == WAIT_TIMEOUT {
        return Ok(None);
    }
    if waited != WAIT_OBJECT_0 {
        return Err(io::Error::last_os_error());
    }
    let mut code = 0u32;
    #[expect(unsafe_code, reason = "GetExitCodeProcess on a process handle owned here")]
    // SAFETY: the process handle is open; `code` is writable.
    let read = unsafe { GetExitCodeProcess(raw(process), &raw mut code) };
    read?;
    Ok(Some(ExitStatus::from_raw(code)))
}

impl ChildWrapper for JobChild {
    fn inner(&self) -> &dyn ChildWrapper {
        self
    }

    fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
        self
    }

    fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
        self
    }

    fn process_handle(&self) -> Option<BorrowedHandle<'_>> {
        Some(self.process.as_handle())
    }

    fn stdin(&mut self) -> &mut Option<ChildStdin> {
        &mut self.stdin
    }

    fn stdout(&mut self) -> &mut Option<ChildStdout> {
        &mut self.stdout
    }

    fn stderr(&mut self) -> &mut Option<ChildStderr> {
        &mut self.stderr
    }

    fn id(&self) -> Option<u32> {
        self.exited.is_none().then_some(self.pid)
    }

    fn start_kill(&mut self) -> io::Result<()> {
        self.job.terminate()
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.exited.is_none() {
            self.exited = wait_for(&self.process, 0)?;
        }
        Ok(self.exited)
    }

    fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
        Box::pin(async move {
            if let Some(status) = self.exited {
                return Ok(status);
            }
            let process = Arc::clone(&self.process);
            let status = tokio::task::spawn_blocking(move || wait_for(&process, INFINITE))
                .await
                .map_err(io::Error::other)??;
            let status = status.ok_or_else(|| io::Error::other("the tool's wait returned before it exited"))?;
            self.exited = Some(status);
            Ok(status)
        })
    }
}

impl Drop for JobChild {
    /// A tool that exited on its own: what it left running is its business, so the job no longer
    /// ends it on close. One still running (cut short, or this process exiting) ends with its tree.
    fn drop(&mut self) {
        if self.exited.is_some()
            && let Err(e) = self.job.kill_on_close(false)
        {
            tracing::debug!(pid = self.pid, error = %e, "a tool's job couldn't be released: what it left running ends with it");
        }
    }
}
