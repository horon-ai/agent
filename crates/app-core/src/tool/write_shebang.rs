//! The execute bit a shebang earns on a newly created file.
//!
//! A script written without it fails the very next `bash ./script.sh`, and the
//! agent then spends a turn on `chmod`. Only a create is affected: an existing
//! file keeps the mode its owner chose, whether or not the new content has a
//! shebang.

use std::io;
use std::path::Path;

/// Whether the content the caller is about to write starts a script.
#[cfg(unix)]
fn has_shebang(content: &str) -> bool {
    content.starts_with("#!")
}

/// The mode a new script takes.
///
/// `write` creates files with `0o666 & !umask`, so the read and write bits
/// already carry the umask and the execute bit is derived from them: every class
/// that may read the file may run it. Deriving the bit instead of calling
/// `umask()` keeps this free of a process-wide change that would race every
/// other thread creating a file.
#[cfg(unix)]
fn script_mode(created: u32) -> u32 {
    let created = created & 0o777;
    created | ((created & 0o444) >> 2)
}

/// Make a just-created file executable when its content starts with `#!`.
///
/// Returns whether the file is executable because of this call, which is the
/// only case worth telling the caller about.
#[cfg(unix)]
pub(super) async fn make_executable(path: &Path, content: &str) -> io::Result<bool> {
    use std::os::unix::fs::PermissionsExt;

    if !has_shebang(content) {
        return Ok(false);
    }
    let created = tokio::fs::metadata(path).await?.permissions().mode() & 0o777;
    let target = script_mode(created);
    if target == created {
        return Ok(target & 0o111 != 0);
    }
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(target)).await?;
    Ok(true)
}

/// Windows has no execute bit, so a create leaves the file exactly as written.
#[cfg(not(unix))]
pub(super) async fn make_executable(_path: &Path, _content: &str) -> io::Result<bool> {
    Ok(false)
}

#[cfg(all(test, unix))]
mod tests {
    use super::super::WriteTool;
    use super::*;
    use crate::tool::{Tool, ToolContext};

    fn restore_home(previous: Option<std::ffi::OsString>) {
        if let Some(value) = previous {
            crate::env::set_var("HORON_HOME", value);
        } else {
            crate::env::remove_var("HORON_HOME");
        }
    }

    fn write_ctx(dir: &Path, session: &str) -> ToolContext {
        ToolContext {
            session_id: session.to_string(),
            message_id: "test-message".to_string(),
            tool_call_id: "test-call".to_string(),
            working_dir: Some(dir.to_path_buf()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::Direct,
        }
    }

    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;

        std::fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    #[tokio::test]
    async fn write_shebang_create_makes_the_file_executable() {
        let _lock = crate::storage::lock_test_env();
        let previous_home = std::env::var_os("HORON_HOME");
        let temp = tempfile::tempdir().expect("tempdir");
        let home = tempfile::tempdir().expect("home dir");
        crate::env::set_var("HORON_HOME", home.path());
        let output = WriteTool::new()
            .execute(
                serde_json::json!({"file_path": "run.sh", "content": "#!/bin/sh\necho hi\n"}),
                write_ctx(temp.path(), "write-shebang-create"),
            )
            .await
            .expect("write should succeed");

        let mode = mode_of(&temp.path().join("run.sh"));
        assert_eq!(mode & 0o100, 0o100, "the owner must be able to run it: {mode:o}");
        assert_eq!(mode & 0o600, 0o600, "the owner keeps read and write: {mode:o}");
        assert!(
            output.output.contains("Made the file executable"),
            "output={:?}",
            output.output
        );
        restore_home(previous_home);
    }

    #[tokio::test]
    async fn write_shebang_overwrite_keeps_the_existing_mode() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("run.sh");
        std::fs::write(&path, "echo old\n").expect("seed file");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("seed mode");

        let output = WriteTool::new()
            .execute(
                serde_json::json!({"file_path": "run.sh", "content": "#!/bin/sh\necho new\n"}),
                write_ctx(temp.path(), "write-shebang-overwrite"),
            )
            .await
            .expect("write should succeed");

        assert_eq!(mode_of(&path), 0o644);
        assert!(
            !output.output.contains("Made the file executable"),
            "an overwrite must not report a mode change: {:?}",
            output.output
        );
    }

    #[tokio::test]
    async fn write_shebang_new_file_without_one_keeps_the_default_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        let output = WriteTool::new()
            .execute(
                serde_json::json!({"file_path": "notes.txt", "content": "plain text\n"}),
                write_ctx(temp.path(), "write-shebang-plain"),
            )
            .await
            .expect("write should succeed");

        let mode = mode_of(&temp.path().join("notes.txt"));
        assert_eq!(mode & 0o111, 0, "no execute bit without a shebang: {mode:o}");
        assert!(
            !output.output.contains("Made the file executable"),
            "output={:?}",
            output.output
        );
    }

    #[tokio::test]
    async fn write_shebang_not_on_the_first_line_does_not_count() {
        let temp = tempfile::tempdir().expect("tempdir");
        WriteTool::new()
            .execute(
                serde_json::json!({"file_path": "late.sh", "content": "\n#!/bin/sh\n"}),
                write_ctx(temp.path(), "write-shebang-late"),
            )
            .await
            .expect("write should succeed");

        let mode = mode_of(&temp.path().join("late.sh"));
        assert_eq!(mode & 0o111, 0, "a shebang must open the file: {mode:o}");
    }

    #[test]
    fn write_shebang_script_mode_adds_the_classes_that_may_read() {
        assert_eq!(script_mode(0o644), 0o755);
        assert_eq!(script_mode(0o600), 0o700);
        assert_eq!(script_mode(0o666), 0o777);
        assert_eq!(script_mode(0o400), 0o500);
        assert_eq!(script_mode(0o000), 0o000);
    }

    #[test]
    fn write_shebang_detects_only_a_leading_shebang() {
        assert!(has_shebang("#!/bin/sh\n"));
        assert!(has_shebang("#!"));
        assert!(!has_shebang("# !/bin/sh\n"));
        assert!(!has_shebang(" #!/bin/sh\n"));
        assert!(!has_shebang(""));
    }
}
/// Windows has no execute bit, so the step must report nothing to do.
#[cfg(all(test, not(unix)))]
mod windows_tests {
    #[tokio::test]
    async fn write_shebang_without_an_execute_bit_is_a_noop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("run.sh");
        tokio::fs::write(&path, "#!/bin/sh\n").await.expect("seed file");

        assert!(
            !super::make_executable(&path, "#!/bin/sh\n")
                .await
                .expect("the no-op step cannot fail")
        );
    }
}
