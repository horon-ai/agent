//! Windows platform adapter: durable file sync and the system certificate
//! verifier for TLS.
//!
//! The crate is compiled as an empty library on other targets so a
//! `--workspace` build never has to compile Windows-only syscalls; only
//! `horon-agent-platform` selects it, and only on Windows.
#![cfg(windows)]

mod tls;

use std::fs::File;
use std::io;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::Duration;

use horon_agent_platform_api::{PlatformAdapter, SleepAssertion, SyncMode};
use rustls::ClientConfig;

/// Windows implementation of [`PlatformAdapter`].
#[derive(Clone, Copy, Debug, Default)]
pub struct Adapter;

impl PlatformAdapter for Adapter {
    type PowerAssertion = PowerAssertion;

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
