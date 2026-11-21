//! Launching one installed package child inside a Job Object this process owns.
//!
//! Windows has no process group a signal can reach, so a package child is
//! placed in a Job Object instead. The job is created with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and with neither breakaway flag, which
//! gives two guarantees the shared supervisor relies on:
//!
//! - every process the child starts is in the same job, and nothing it does can
//!   move itself or a descendant out of it; and
//! - closing the last handle to the job terminates every member, whether this
//!   process closes it on purpose, drops it, or dies.
//!
//! The child is created suspended, assigned to the job, and only then resumed.
//! The Job Object is not passed through `PROC_THREAD_ATTRIBUTE_JOB_LIST`,
//! because the shared supervisor hands the standard streams out as
//! `std::process::ChildStdin`/`ChildStdout`/`ChildStderr`: `std` owns the pipe
//! ends and the `CreateProcessW` call that passes them, and a `ChildStdout`
//! cannot be built from a handle this crate created. Suspension closes the same
//! window: the primary thread never runs, so the child starts no descendant
//! before it is in the job.
//!
//! If the host process is already inside a Job Object that does not allow
//! nesting (either job sets UI limits, or the host predates Windows 8), the
//! assignment fails and the suspended child is terminated: a package child
//! that cannot be owned is not started at all rather than left unsupervised.
//!
//! The installed executable is opened share-read only and held across the
//! spawn, so between that open and the image being mapped no other process can
//! write, rename or delete it. The digest check that decides *whether* these
//! bytes run happens immediately before this call, in
//! `horon_agent_base::packages`; this handle covers the remaining window.
//!
//! One limit is worth stating plainly: the child runs with the same user's
//! permissions, so files and the network stay reachable. The job owns its
//! lifetime, not its access.

use std::io;
use std::mem::size_of;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use horon_agent_platform_api::package_child::{PackageChildProcess, PackageChildSpawn};

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    TerminateJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
};

/// The longest single pause while the grace period runs out.
const TERMINATE_POLL: Duration = Duration::from_millis(25);

/// One started package child and the Job Object that owns its whole tree.
///
/// The job handle is the lifetime owner: it is closed by
/// [`PackageChildProcess::terminate`] after the tree was forced down, and
/// closing it at any other time - including this process dying - ends every
/// member through `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`.
pub struct WindowsPackageChild {
    child: Child,
    job: Option<OwnedHandle>,
    status: Option<ExitStatus>,
    id: u32,
}

/// Start `request`'s executable in a Job Object this process owns.
///
/// The child is created suspended, placed in a fresh job that no member may
/// leave, and resumed. A failure to own the child - most often a host process
/// inside a Job Object that does not allow nested jobs - terminates the
/// suspended child and returns the diagnostic, so nothing is ever left running
/// outside the job.
pub fn spawn(request: &PackageChildSpawn<'_>) -> io::Result<WindowsPackageChild> {
    // Held across the spawn: see the module documentation for the exact window
    // this closes.
    let _locked = lock_executable(request.program)?;
    let job = create_job()?;

    let mut command = Command::new(request.program);
    command
        .args(request.arguments)
        .current_dir(request.working_directory)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW);
    for (key, value) in request.environment {
        command.env(key, value);
    }
    let mut child = command.spawn().map_err(|error| {
        described(
            format!("start the installed executable {}", request.program.display()),
            error,
        )
    })?;
    let id = child.id();

    if let Err(error) = assign_to_job(&child, &job) {
        abandon(&mut child);
        return Err(error);
    }
    if let Err(error) = resume_primary_thread(id) {
        abandon(&mut child);
        return Err(error);
    }
    Ok(WindowsPackageChild {
        child,
        job: Some(job),
        status: None,
        id,
    })
}

impl WindowsPackageChild {
    /// Terminate every member of the job and close its handle.
    fn terminate_job(&self) -> io::Result<()> {
        let Some(job) = self.job.as_ref() else {
            return Ok(());
        };
        // SAFETY: the handle is the job this value owns and stays open for the
        // call. `TerminateJobObject` only acts on that job's members.
        let ok = unsafe { TerminateJobObject(job.as_raw_handle(), 1) };
        if ok == 0 {
            return Err(described(
                format!("terminate the package Job Object of child {}", self.id),
                io::Error::last_os_error(),
            ));
        }
        Ok(())
    }

    /// Record an exit only after its tree was swept, then release the job.
    ///
    /// Reporting an exit means no descendant outlives it: the job is terminated
    /// before the status is remembered, and its handle is closed afterwards so
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` still covers anything that appeared
    /// in between.
    fn settle(&mut self, status: ExitStatus) -> io::Result<ExitStatus> {
        self.terminate_job()?;
        self.job = None;
        self.status = Some(status);
        Ok(status)
    }
}

impl PackageChildProcess for WindowsPackageChild {
    fn id(&self) -> u32 {
        self.id
    }

    fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.child.stdin.take()
    }

    fn take_stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }

    fn take_stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    fn exited(&self) -> bool {
        self.status.is_some()
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = self.status {
            return Ok(Some(status));
        }
        match self.child.try_wait()? {
            Some(status) => self.settle(status).map(Some),
            None => Ok(None),
        }
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        let status = self.child.wait()?;
        self.settle(status)
    }

    fn terminate(&mut self, grace: Duration) -> io::Result<ExitStatus> {
        if let Some(status) = self.status {
            return Ok(status);
        }
        // A `shutdown` request the wire protocol already delivered gets the
        // grace period to finish on its own. The leader's exit is observed
        // without a reaping dependency: `try_wait` is enough here because the
        // job, not a process-group id, is what the sweep below acts on.
        let deadline = Instant::now() + grace;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return self.settle(status);
            }
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            std::thread::sleep(TERMINATE_POLL.min(deadline - now));
        }
        // Windows has no catchable termination request, so the forced stop is
        // the whole stop: descendants still writing are killed with it.
        self.terminate_job()?;
        let status = self.child.wait()?;
        self.settle(status)
    }
}

/// Open the installed executable so nothing can replace it during the spawn.
///
/// `FILE_SHARE_READ` alone denies every other handle the write, delete and
/// therefore rename access the image would need to be swapped. The returned
/// handle is dropped by the caller once the child's image is mapped.
fn lock_executable(program: &Path) -> io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(program)
        .map_err(|error| described(format!("open the installed executable {}", program.display()), error))?;
    file.metadata()
        .map_err(|error| described(format!("inspect {}", program.display()), error))
        .and_then(|metadata| {
            if metadata.is_file() {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("the installed executable {} is not a regular file", program.display()),
                ))
            }
        })?;
    Ok(file)
}

/// Create the job that owns one package child's tree.
fn create_job() -> io::Result<OwnedHandle> {
    // SAFETY: a null name asks for an unnamed job and a null attribute pointer
    // asks for default (non-inheritable) security, so nothing but this value
    // receives the handle.
    let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if handle.is_null() {
        return Err(described("create the package Job Object", io::Error::last_os_error()));
    }
    // SAFETY: `CreateJobObjectW` returned a fresh, uniquely owned handle.
    let job = unsafe { OwnedHandle::from_raw_handle(handle) };

    // `KILL_ON_JOB_CLOSE` is the lifetime guarantee; the breakaway flags are
    // deliberately absent so no member can leave the job, and no limit is set
    // that would make the job non-nestable.
    // SAFETY: the structure is plain data; zeroing it is the documented initial
    // state and the only field written is a flag set.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: the job handle is open, and the pointer and length name exactly
    // one `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`.
    let ok = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const core::ffi::c_void,
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        return Err(described(
            "configure the package Job Object",
            io::Error::last_os_error(),
        ));
    }
    Ok(job)
}

/// Place the suspended child in `job`, refusing to run it when that fails.
fn assign_to_job(child: &Child, job: &OwnedHandle) -> io::Result<()> {
    // SAFETY: both handles are open for this call; the child's process handle
    // carries `PROCESS_SET_QUOTA` because `CreateProcess` returns a handle with
    // all access rights.
    let assigned = unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) };
    if assigned == 0 {
        let error = io::Error::last_os_error();
        return Err(io::Error::new(
            error.kind(),
            format!(
                "the package child {} was not started because Windows refused to place it in its own Job Object \
                 ({error}); a host process inside a Job Object that does not allow nested jobs cannot own a \
                 package child, so none was started",
                child.id(),
            ),
        ));
    }
    // The assignment above is authoritative, and this second check is what turns
    // a documented edge case into a refusal instead of an unsupervised child.
    let mut member = 0;
    // SAFETY: the handles stay open and `member` is a writable `BOOL`.
    let queried = unsafe { IsProcessInJob(child.as_raw_handle(), job.as_raw_handle(), &mut member) };
    if queried == 0 || member == 0 {
        return Err(described(
            format!("confirm package child {} is in its Job Object", child.id()),
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

/// Resume the primary thread of the suspended child.
///
/// Windows exposes no way to obtain that thread's handle through
/// `std::process::Child`, and the suspended process has exactly one thread, so
/// the thread is found by its owning process and resumed by handle.
fn resume_primary_thread(pid: u32) -> io::Result<()> {
    let thread = open_primary_thread(pid)?;
    // SAFETY: `thread` is an open thread handle for this pid with
    // `THREAD_SUSPEND_RESUME`; resuming it is the inverse of the
    // `CREATE_SUSPENDED` the child was created with.
    let previous = unsafe { ResumeThread(thread.as_raw_handle()) };
    if previous == u32::MAX {
        return Err(described(
            format!("resume the suspended package child {pid}"),
            io::Error::last_os_error(),
        ));
    }
    Ok(())
}

/// Open the primary thread of the suspended process `pid`.
fn open_primary_thread(pid: u32) -> io::Result<OwnedHandle> {
    // SAFETY: a thread snapshot is read-only system state and the returned
    // handle is closed by the value below.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(described(
            "enumerate the threads of the suspended package child",
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: the snapshot call returned a fresh, uniquely owned handle.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };

    // SAFETY: `THREADENTRY32` is plain data and the zeroed value is only a
    // starting point: `dwSize` is the documented input.
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<THREADENTRY32>() as u32;
    let mut thread_id = None;
    // SAFETY: both calls fill `entry` for the snapshot handle that stays open.
    unsafe {
        let mut more = Thread32First(snapshot.as_raw_handle(), &mut entry) != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                thread_id = Some(entry.th32ThreadID);
                break;
            }
            entry.dwSize = size_of::<THREADENTRY32>() as u32;
            more = Thread32Next(snapshot.as_raw_handle(), &mut entry) != 0;
        }
    }
    let thread_id = thread_id.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("the suspended package child {pid} reported no thread to resume"),
        )
    })?;
    // SAFETY: opening a thread only needs the id and the access right; the
    // handle is owned by the value returned.
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id) };
    if thread.is_null() {
        return Err(described(
            format!("open the primary thread of the suspended package child {pid}"),
            io::Error::last_os_error(),
        ));
    }
    // SAFETY: `OpenThread` returned a fresh, uniquely owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(thread) })
}

/// Terminate a child that never ran, and reap it.
///
/// The child is still suspended here, so no package code has executed and no
/// descendant exists; terminating it is the only cleanup left.
fn abandon(child: &mut Child) {
    drop(child.kill());
    drop(child.wait());
}

/// Add the operation to an operating-system error without losing its kind.
fn described(operation: impl Into<String>, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", operation.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    use windows_sys::Win32::System::JobObjects::{
        JOB_OBJECT_UILIMIT_HANDLES, JOBOBJECT_BASIC_UI_RESTRICTIONS, JobObjectBasicUIRestrictions,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    /// Set in the helper process that provokes the refusal below.
    const HELPER_ENV: &str = "HORON_PACKAGE_WINDOWS_RESTRICTED_JOB_HELPER";
    /// Where the helper's would-be child would report that it ran.
    const MARKER_ENV: &str = "HORON_PACKAGE_WINDOWS_RESTRICTED_JOB_MARKER";

    /// A host process inside a job chain that forbids nesting is refused, not
    /// quietly given an unowned child.
    ///
    /// The provocation is a Job Object with UI limits, which the system refuses
    /// to place inside a nested hierarchy. It is not reversible, so it runs in a
    /// helper copy of this test binary: assigning this process would make every
    /// later package launch in the same binary fail.
    #[test]
    fn a_host_job_that_forbids_nesting_refuses_the_child_before_it_runs() {
        if std::env::var_os(HELPER_ENV).is_some() {
            provoke_in_restricted_job();
            return;
        }
        let directory = tempfile::tempdir().expect("marker directory");
        let marker = directory.path().join("started.txt");
        let output = Command::new(std::env::current_exe().expect("the test executable"))
            .args([
                "--exact",
                "package_child::tests::a_host_job_that_forbids_nesting_refuses_the_child_before_it_runs",
                "--nocapture",
            ])
            .env(HELPER_ENV, "1")
            .env(MARKER_ENV, &marker)
            .output()
            .expect("start the restricted-job helper");
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !marker.exists(),
            "no package child may run when the host cannot put it in a job",
        );
    }

    /// The helper body: enter a job with UI limits, then launch a real child.
    fn provoke_in_restricted_job() {
        let marker = std::env::var_os(MARKER_ENV).expect("the marker path");

        // Created here rather than through `create_job`: this fixture is the
        // *host's* job, and it must not carry `KILL_ON_JOB_CLOSE`, which would
        // end the helper when the handle closes.
        // SAFETY: an unnamed job with default (non-inheritable) security.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        assert!(
            !job.is_null(),
            "create the restricted job: {}",
            io::Error::last_os_error()
        );
        // SAFETY: the call returned a fresh, uniquely owned handle.
        let job = unsafe { OwnedHandle::from_raw_handle(job) };

        // SAFETY: `JOBOBJECT_BASIC_UI_RESTRICTIONS` is plain data.
        let mut ui: JOBOBJECT_BASIC_UI_RESTRICTIONS = unsafe { std::mem::zeroed() };
        ui.UIRestrictionsClass = JOB_OBJECT_UILIMIT_HANDLES;
        // SAFETY: the handle is open and the pointer and length name exactly one
        // `JOBOBJECT_BASIC_UI_RESTRICTIONS`.
        let configured = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectBasicUIRestrictions,
                &ui as *const _ as *const core::ffi::c_void,
                size_of::<JOBOBJECT_BASIC_UI_RESTRICTIONS>() as u32,
            )
        };
        assert_ne!(configured, 0, "restrict the job: {}", io::Error::last_os_error());
        // SAFETY: assigning this process to an open job; both handles are valid
        // for the call.
        let entered = unsafe { AssignProcessToJobObject(job.as_raw_handle(), GetCurrentProcess()) } != 0;
        // Either this process is now in a job with UI limits, or the job it was
        // already in refused the nesting. Both are the environment this test is
        // about, and both mean the launcher must refuse the child.
        println!("restricted job entered: {entered}");

        let program = PathBuf::from(std::env::var_os("COMSPEC").expect("COMSPEC is set on Windows"));
        let arguments = [
            OsString::from("/D"),
            OsString::from("/C"),
            OsString::from(format!("echo started>\"{}\"", Path::new(&marker).display())),
        ];
        let working_directory = std::env::temp_dir();
        // A minimal environment of the kind the launcher builds, so the refusal
        // below cannot come from an empty environment block.
        let environment: Vec<(OsString, OsString)> = ["SYSTEMROOT", "PATH", "COMSPEC"]
            .into_iter()
            .filter_map(|key| std::env::var_os(key).map(|value| (OsString::from(key), value)))
            .collect();
        let request = PackageChildSpawn {
            program: &program,
            arguments: &arguments,
            working_directory: &working_directory,
            environment: &environment,
        };
        let error = match spawn(&request) {
            Ok(_started) => panic!("a host job that forbids nesting must refuse the package child"),
            Err(error) => error,
        };
        let message = error.to_string();
        assert!(
            message.contains("nested jobs"),
            "the refusal must name the cause: {message}",
        );
        assert!(
            !Path::new(&marker).exists(),
            "a refused package child must not run before it is refused",
        );
    }
}
