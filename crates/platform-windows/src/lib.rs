//! Windows platform adapter: durable file sync and the system certificate
//! verifier for TLS.
//!
//! The crate is compiled as an empty library on other targets so a
//! `--workspace` build never has to compile Windows-only syscalls; only
//! `horon-agent-platform` selects it, and only on Windows.
#![cfg(windows)]

mod fs;
pub mod ipc;
mod power;
pub mod signal;
pub mod term;
mod tls;
pub mod watch;

pub use power::PowerGuard;

use std::fs::{File, Metadata, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

use horon_agent_platform_api::{
    FileIds, FileStamp, PlatformAdapter, ProcessMemoryFacts, ReadyChild, SleepAssertion, StdinState, StopRequest,
    SyncMode,
};
use rustls::ClientConfig;

/// Windows implementation of [`PlatformAdapter`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Adapter;

impl PlatformAdapter for Adapter {
    type PowerAssertion = PowerAssertion;
    type Inhibitor = PowerGuard;

    fn sync_file(file: &File, _mode: SyncMode) -> io::Result<()> {
        // `sync_all` is FlushFileBuffers; there is no cheaper durable call, so
        // both modes match.
        file.sync_all()
    }

    fn sync_dir(_path: &Path, _mode: SyncMode) -> io::Result<()> {
        // A directory cannot be opened for FlushFileBuffers without backup
        // semantics and NTFS journals its entries itself, so the durability
        // boundary is the file's own flush. The caller keeps the check that the
        // path is really a directory.
        Ok(())
    }

    fn tls_client_config() -> Arc<ClientConfig> {
        tls::client_config()
    }

    fn is_process_running(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut exit_code = 0u32;
            let ok = GetExitCodeProcess(handle, &mut exit_code);
            CloseHandle(handle);
            ok != 0 && exit_code == STILL_ACTIVE as u32
        }
    }

    fn signal_detached_process_group(pid: u32, _signal: i32) -> io::Result<()> {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            CREATE_NO_WINDOW, OpenProcess, PROCESS_TERMINATE, TerminateProcess,
        };

        // Detached commands commonly run through cmd.exe or PowerShell. Killing
        // only that shell leaves compilers, test runners, and other descendants
        // alive. taskkill's /T flag walks the Windows process tree. Keep the
        // direct Win32 termination below as a fallback if taskkill is missing or
        // the tree operation fails.
        let tree_status = std::process::Command::new("taskkill.exe")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .status();
        if tree_status.is_ok_and(|status| status.success()) {
            return Ok(());
        }
        // taskkill can report failure when a descendant exits while it walks the
        // tree, even though it successfully terminated the leader and remaining
        // descendants. Avoid turning that benign race into a misleading access
        // denied error from the direct-handle fallback.
        for _ in 0..20 {
            if !Self::is_process_running(pid) {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(25));
        }

        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let ok = TerminateProcess(handle, 1);
            CloseHandle(handle);
            if ok == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        }
    }

    fn terminate_process_group(pid: u32, _grace: Duration) -> bool {
        // Windows has no catchable termination signal, so the tree-termination
        // path runs immediately.
        if !Self::is_process_running(pid) {
            return true;
        }
        drop(Self::signal_detached_process_group(pid, 0));
        !Self::is_process_running(pid)
    }

    fn try_reap_child_process(_pid: u32) -> io::Result<Option<i32>> {
        // Windows has no zombie state: closing the process handle is enough.
        Ok(None)
    }

    fn atomic_symlink_swap(src: &Path, dst: &Path, temp: &Path) -> io::Result<()> {
        // Not atomic, but the best Windows can do without `ReplaceFile` on a
        // symlink.
        // Best effort: both names may be absent, which is not an error.
        drop(std::fs::remove_file(temp));
        drop(std::fs::remove_file(dst));
        std::fs::copy(src, dst).map(|_| ())
    }

    fn spawn_detached(cmd: &mut Command) -> io::Result<Child> {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};

        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
        cmd.spawn()
    }

    fn spawn_detached_without_console_window(cmd: &mut Command) -> io::Result<Child> {
        // Why not `spawn_detached`: `DETACHED_PROCESS` leaves the child with no
        // console at all, so the first console-subsystem grandchild it starts -
        // a `curl.exe` inside a hook script, say - allocates a brand new console
        // for itself, which Windows Terminal surfaces as a tab.
        // `CREATE_NO_WINDOW` instead gives the child a console that has no
        // window, and descendants inherit it, so the whole subtree stays
        // invisible. The two flags cannot be combined: `CREATE_NO_WINDOW` is
        // documented as ignored when `DETACHED_PROCESS` is also specified.
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        cmd.spawn()
    }

    fn spawn_background_consoleless(cmd: &mut Command) -> io::Result<Child> {
        // Same flags as the SDK has always used for the runtime it launches: a
        // new process group keeps the host's Ctrl+C or exit from reaching the
        // daemon, and `CREATE_NO_WINDOW` keeps the console hidden.
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
        cmd.spawn()
    }

    fn reap_detached(child: Child) {
        // Closing the process handle is sufficient on Windows. Unlike Unix,
        // the child does not need to be waited on to avoid a zombie process.
        drop(child);
    }

    fn replace_process(cmd: &mut Command) -> io::Error {
        match spawn_replacement_process(cmd) {
            Ok(_child) => std::process::exit(0),
            Err(e) => e,
        }
    }

    fn raise_nofile_limit_best_effort(_minimum_soft_limit: u64) {
        // `RLIMIT_NOFILE` does not exist on Windows.
    }

    fn symlink_or_copy(src: &Path, dst: &Path) -> io::Result<()> {
        if src.is_dir() {
            std::os::windows::fs::symlink_dir(src, dst).or_else(|_| copy_dir_recursive(src, dst))
        } else {
            std::os::windows::fs::symlink_file(src, dst).or_else(|_| std::fs::copy(src, dst).map(|_| ()))
        }
    }

    fn set_permissions_executable(_path: &Path) -> io::Result<()> {
        // Executability is determined by the file extension on Windows.
        Ok(())
    }

    fn set_file_permissions_owner_only(path: &Path) -> io::Result<()> {
        fs::set_file_permissions_owner_only(path)
    }

    fn set_dir_permissions_owner_only(path: &Path) -> io::Result<()> {
        fs::set_dir_permissions_owner_only(path)
    }

    fn harden_credential_file_permissions(path: &Path) {
        fs::harden_credential_file_permissions(path);
    }

    fn harden_credential_dir_permissions(path: &Path) {
        fs::harden_credential_dir_permissions(path);
    }

    fn harden_dir_permissions(path: &Path) {
        fs::harden_dir_permissions(path);
    }

    fn is_private_dir(metadata: &Metadata) -> bool {
        fs::is_private_dir(metadata)
    }

    fn is_private_dir_owned_by(metadata: &Metadata, owner: &Metadata) -> bool {
        fs::is_private_dir_owned_by(metadata, owner)
    }

    fn is_private_file(metadata: &Metadata) -> bool {
        fs::is_private_file(metadata)
    }

    fn is_executable(path: &Path) -> bool {
        fs::is_executable(path)
    }

    fn no_follow_open_options() -> OpenOptions {
        fs::no_follow_open_options()
    }

    fn request_owner_only_mode(options: &mut OpenOptions) {
        fs::request_owner_only_mode(options);
    }

    fn create_private_dir(path: &Path) -> io::Result<()> {
        fs::create_private_dir(path)
    }

    fn verify_private_file(path: &Path, file: &File) -> io::Result<()> {
        fs::verify_private_file(path, file)
    }

    fn file_stamp(metadata: &Metadata) -> FileStamp {
        fs::file_stamp(metadata)
    }

    fn file_ids(metadata: &Metadata) -> FileIds {
        fs::file_ids(metadata)
    }

    fn mtime_ns(metadata: &Metadata) -> i128 {
        fs::mtime_ns(metadata)
    }

    fn rotate_backup(primary: &Path, backup: &Path) -> io::Result<()> {
        fs::rotate_backup(primary, backup)
    }

    fn sync_parent_directory(path: &Path) -> io::Result<()> {
        fs::sync_parent_directory(path)
    }

    fn isolate_process_group(_command: &mut Command) {
        // Windows has no process groups; job objects cover the same ground and
        // are not created here.
    }

    fn fd_diagnostic_snapshot() -> String {
        // There is no `/proc` to read another process's descriptor table from.
        format!("pid={} fd snapshot unsupported on this platform", std::process::id())
    }

    fn stdin_state(pid: u32) -> StdinState {
        term::stdin_state(pid)
    }

    fn process_tree_stdin_state(pid: u32) -> StdinState {
        term::process_tree_stdin_state(pid)
    }

    fn direct_children(pid: u32) -> Vec<u32> {
        term::direct_children(pid)
    }

    fn user_discriminator() -> String {
        let raw = std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "user".to_string());
        let sanitized: String = raw
            .chars()
            .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
            .take(64)
            .collect();
        if sanitized.is_empty() {
            "user".to_string()
        } else {
            sanitized
        }
    }

    fn user_runtime_dir() -> Option<PathBuf> {
        // The shared temporary directory is already per-user on Windows.
        None
    }

    fn user_config_home() -> Option<PathBuf> {
        // Roaming application data, or the same place under the profile when
        // the environment variable is missing.
        std::env::var_os("APPDATA").map(PathBuf::from).or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|home| PathBuf::from(home).join("AppData/Roaming"))
        })
    }

    fn atomic_replace(from: &Path, to: &Path) -> io::Result<()> {
        fs::atomic_replace(from, to)
    }

    fn symlink_file(src: &Path, dst: &Path) -> io::Result<()> {
        fs::symlink_file(src, dst)
    }

    fn terminate_child(child: &mut Child) {
        // `Child::kill` is `TerminateProcess`, and Windows has no zombie state,
        // so the child is gone as soon as it is terminated and waited on.
        if matches!(child.try_wait(), Ok(Some(_))) {
            return;
        }
        drop(child.kill());
        drop(child.wait());
    }

    fn describe_file_kind(metadata: &Metadata) -> &'static str {
        if metadata.is_dir() {
            "directory"
        } else {
            // Windows exposes no sockets, FIFOs or device nodes through the
            // metadata surface.
            "non-regular file"
        }
    }

    fn harden_credential_open(_options: &mut OpenOptions, path: &Path) -> io::Result<()> {
        // There is no `O_NOFOLLOW` here, so refuse a symlink at the leaf before
        // the open instead: following it would leave the private tree.
        if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "a credential file must not be a symlink",
            ));
        }
        Ok(())
    }

    fn set_permissions_owner_only_file(_file: &File) -> io::Result<()> {
        // Access comes from the inherited ACL, which the caller sets by path.
        Ok(())
    }

    fn verify_private_file_metadata(metadata: &Metadata) -> io::Result<()> {
        if metadata.is_file() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "a credential file must be a regular file",
            ))
        }
    }

    fn verify_private_dir_metadata(metadata: &Metadata) -> io::Result<()> {
        if metadata.is_dir() {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                "a credential directory must be a directory",
            ))
        }
    }

    fn clone_tree(_src: &Path, _dst: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this platform has no copy-on-write directory clone",
        ))
    }

    fn hide_console_window(cmd: &mut Command) {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

        // The flag is inherited, so a script that shells out stays invisible
        // too.
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    fn shell_command(command: &str, _temp_dir: Option<&Path>) -> Command {
        use std::os::windows::process::CommandExt;

        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/D", "/S", "/C"]);
        // `arg` re-escapes the string for CommandLineToArgvW, which rewrites the
        // quotes a command such as `gh issue create --title "a b"` relies on;
        // `raw_arg` passes the single `/C` argument exactly as written.
        cmd.raw_arg(format!("\"{command}\""));
        cmd
    }

    fn shell_command_escaped(command: &str, _temp_dir: Option<&Path>) -> Command {
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/D", "/S", "/C"]).arg(command);
        cmd
    }

    fn process_working_dirs() -> Vec<PathBuf> {
        // No process table lists working directories without extra privileges.
        Vec::new()
    }

    fn process_memory_facts() -> ProcessMemoryFacts {
        ProcessMemoryFacts::default()
    }

    fn set_process_name(_name: &str) {
        // The process name comes from the executable image.
    }

    fn browser_launcher_state() -> Option<bool> {
        // `start` ships with the operating system.
        Some(true)
    }

    fn is_container() -> bool {
        false
    }

    fn kill_process_group(_pid: u32) {
        // There is no group-wide signal; the caller terminates the child.
    }

    fn configure_system_allocator(_use_system_allocator: bool) {
        // The allocator knobs this tunes are glibc's; Windows' heap has its own
        // defaults and no equivalent switch.
    }

    fn main_thread_stack_size() -> Option<usize> {
        // Windows executables default to a much smaller main-thread stack than
        // the Unix environments where most development happens. The CLI and
        // provider setup can exceed that reserve before Tokio takes over,
        // producing an unrecoverable STATUS_STACK_OVERFLOW, so the entry point
        // runs on a deliberately sized thread instead.
        Some(8 * 1024 * 1024)
    }

    fn bind_process_lifetime_to_parent() -> bool {
        // Job objects can tie a child to its parent, but this adapter creates
        // none, and a silently armed kill-on-close would change how a detached
        // daemon outlives its client.
        false
    }

    fn request_graceful_stop(pid: u32) -> io::Result<StopRequest> {
        // There is no catchable termination signal; the tree is terminated at
        // once, so there is nothing left for an escalation step.
        Self::signal_detached_process_group(pid, 0)?;
        Ok(StopRequest::Terminated)
    }

    fn force_stop_process_group(pid: u32) -> io::Result<()> {
        Self::signal_detached_process_group(pid, 0)
    }

    fn spawn_with_ready_pipe(cmd: &mut Command, _ready_env_var: &str) -> io::Result<ReadyChild> {
        // A descriptor cannot be handed to a child through CreateProcess, so
        // the caller polls the child's own effect instead of waiting for a
        // readiness byte.
        Ok(ReadyChild {
            child: cmd.spawn()?,
            ready: None,
        })
    }

    fn signal_name(signal: i32) -> String {
        signal::signal_name(signal)
    }

    fn signal_crash_reason(signal: i32) -> String {
        signal::signal_crash_reason(signal)
    }

    fn spawn_termination_signal_watchers<F>(on_signal: F)
    where
        F: Fn(i32) + Send + Sync + 'static,
    {
        signal::spawn_termination_signal_watchers(on_signal);
    }
}

fn spawn_replacement_process(cmd: &mut Command) -> io::Result<Child> {
    cmd.spawn()
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

impl Adapter {
    /// Build a client configuration that also trusts `extra_roots`.
    ///
    /// Production code takes the cached configuration from
    /// [`PlatformAdapter::tls_client_config`]; this hook lets tests inject a
    /// private CA and prove it is accepted beside the real trust set.
    #[doc(hidden)]
    pub fn client_config_with_extra_roots(
        extra_roots: Vec<rustls::pki_types::CertificateDer<'static>>,
    ) -> Result<ClientConfig, rustls::Error> {
        tls::build(extra_roots)
    }
}
/// Inert sleep-prevention assertion: only macOS can keep the machine awake.
pub struct PowerAssertion;

impl SleepAssertion for PowerAssertion {
    fn prevent_user_idle_system_sleep(_reason: &str) -> Self {
        Self
    }

    fn is_active(&self) -> bool {
        false
    }

    fn release(&mut self) {}
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::path::PathBuf;

    use super::*;

    /// Write `contents` into a fresh temporary directory, sync it, and return
    /// the directory guard (which keeps the file alive) with its path.
    fn synced_file(contents: &[u8], mode: SyncMode) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("payload.txt");
        let mut file = File::create(&path).expect("create file");
        file.write_all(contents).expect("write bytes");
        file.flush().expect("flush bytes");
        Adapter::sync_file(&file, mode).expect("sync file");
        drop(file);
        (directory, path)
    }

    fn read_back(path: &std::path::Path) -> String {
        let mut contents = String::new();
        File::open(path)
            .expect("reopen")
            .read_to_string(&mut contents)
            .expect("read back");
        contents
    }

    #[test]
    fn sync_file_full_then_reopen_reads_bytes() {
        let (_directory, path) = synced_file(b"durable full mode\r\n", SyncMode::Full);
        assert_eq!(read_back(&path), "durable full mode\r\n");
    }

    #[test]
    fn sync_file_fast_then_reopen_reads_bytes() {
        let (_directory, path) = synced_file(b"durable fast mode\r\n", SyncMode::Fast);
        assert_eq!(read_back(&path), "durable fast mode\r\n");
    }

    #[test]
    fn sync_dir_is_a_no_op_on_a_directory() {
        let directory = tempfile::tempdir().expect("temporary directory");
        Adapter::sync_dir(directory.path(), SyncMode::Full).expect("directory sync");
        Adapter::sync_dir(directory.path(), SyncMode::Fast).expect("directory sync");
    }

    #[test]
    fn is_process_running_reports_exited_children_as_stopped() {
        use std::process::{Command, Stdio};
        use std::time::Duration;

        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/C", "ping -n 3 127.0.0.1 >NUL"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let mut child = cmd.spawn().expect("spawn child");
        let pid = child.id();
        assert!(Adapter::is_process_running(pid), "child should initially be running");

        let status = child.wait().expect("wait for child");
        assert!(status.success(), "child should exit successfully");
        std::thread::sleep(Duration::from_millis(100));

        assert!(
            !Adapter::is_process_running(pid),
            "exited child should not be reported as running"
        );
    }

    #[test]
    fn signal_detached_process_group_terminates_descendant_tree() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let temp = tempfile::tempdir().expect("temp dir");
        let ready_path = temp.path().join("child-ready.txt");
        let survived_path = temp.path().join("child-survived.txt");
        let child_script_path = temp.path().join("child.cmd");
        let parent_script_path = temp.path().join("parent.cmd");
        let child_script = concat!(
            "@echo off\r\n",
            "echo ready>\"%~dp0child-ready.txt\"\r\n",
            "ping -n 6 127.0.0.1 >NUL\r\n",
            "echo survived>\"%~dp0child-survived.txt\"\r\n"
        );
        let parent_script = concat!(
            "@echo off\r\n",
            "start \"\" /B cmd.exe /D /C \"\"%~dp0child.cmd\"\"\r\n",
            "ping -n 30 127.0.0.1 >NUL\r\n"
        );
        std::fs::write(&child_script_path, child_script).expect("write child command script");
        std::fs::write(&parent_script_path, parent_script).expect("write parent command script");
        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/D", "/C"])
            .arg(&parent_script_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let mut parent = Adapter::spawn_detached(&mut cmd).expect("spawn detached process tree");
        let parent_pid = parent.id();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready_path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(ready_path.exists(), "descendant should report ready");
        assert!(Adapter::is_process_running(parent_pid));

        Adapter::signal_detached_process_group(parent_pid, 0).expect("terminate process tree");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Adapter::is_process_running(parent_pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = parent.wait();

        assert!(!Adapter::is_process_running(parent_pid), "parent should stop");
        std::thread::sleep(Duration::from_secs(6));
        assert!(
            !survived_path.exists(),
            "descendant should not survive termination of the detached process tree"
        );
    }

    #[test]
    fn spawn_replacement_process_returns_without_waiting_for_child_exit() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let mut cmd = Command::new("cmd.exe");
        cmd.args(["/C", "ping -n 4 127.0.0.1 >NUL"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let start = Instant::now();
        let mut child = spawn_replacement_process(&mut cmd).expect("spawn replacement process should succeed");
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(1),
            "replacement spawn should not block, took {:?}",
            elapsed
        );
        assert!(
            child.try_wait().expect("poll child status").is_none(),
            "replacement child should still be running immediately after spawn"
        );

        child.kill().ok();
        let _ = child.wait();
    }
}
