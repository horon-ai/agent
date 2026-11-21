use super::*;
use crate::tool::{Tool, ToolExecutionMode};
use serde_json::json;

#[test]
fn approved_direnv_exports_to_the_pty_and_blocks_unapproved_files() {
    if !horon_agent_platform::term::shell_is_posix() {
        return;
    }
    const TEST_CHILD: &str = "HORON_DIRENV_FIXTURE_WORKSPACE";
    if let Some(workspace) = std::env::var_os(TEST_CHILD) {
        let workspace = std::path::PathBuf::from(workspace);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(exercise_bash(&workspace));
        return;
    }
    if std::process::Command::new("direnv").arg("version").output().is_err() {
        return; // The Linux CI cohort explicitly installs direnv before running this test.
    }

    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path().join("workspace");
    let home = temp.path().join("horon");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("config.toml"), "[tools]\nbash_direnv = true\n").unwrap();
    std::fs::write(
        workspace.join(".envrc"),
        "printf 'ran' > rc-ran\nexport HORON_FIXTURE_SECRET=changed\nexport DIRENV_FIXTURE_SECRET=changed\nexport FIXTURE_EXISTING=changed\nexport FIXTURE_NEW=from-direnv\n",
    )
    .unwrap();
    let run = |trust: &str| {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tool::bash::direnv::tests::approved_direnv_exports_to_the_pty_and_blocks_unapproved_files",
                "--nocapture",
            ])
            .env(TEST_CHILD, &workspace)
            .env("HORON_HOME", &home)
            .env("HORON_AGENT_WORKSPACE_TRUST", trust)
            .env("DIRENV_CONFIG", temp.path().join("direnv-config"))
            .env("XDG_DATA_HOME", temp.path().join("direnv-data"))
            .env("FIXTURE_EXISTING", "original")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    };
    run("trust");
    assert!(workspace.join("rc-ran").is_file());
    std::fs::remove_file(workspace.join("rc-ran")).unwrap();
    // direnv still allows these bytes, but Horon independently denies the workspace.
    run("deny");
    assert!(!workspace.join("rc-ran").exists());
}

async fn exercise_bash(workspace: &Path) {
    let tool = super::super::BashTool::new();
    let run = || {
        tool.execute(
            json!({"command": "test -t 1 && printf 'PTY:%s:%s' \"$FIXTURE_NEW\" \"$FIXTURE_EXISTING\"", "pty": "always"}),
            crate::tool::ToolContext {
                session_id: "direnv-fixture".into(),
                message_id: String::new(),
                tool_call_id: "direnv-fixture-call".into(),
                working_dir: Some(workspace.to_path_buf()),
                stdin_request_tx: None,
                graceful_shutdown_signal: None,
                execution_mode: ToolExecutionMode::Direct,
            },
        )
    };
    let blocked = run().await.expect_err("unapproved file must stop the shell");
    assert!(!workspace.join("rc-ran").exists());
    if std::env::var("HORON_AGENT_WORKSPACE_TRUST").as_deref() == Ok("deny") {
        assert!(blocked.to_string().contains("Horon workspace trust"), "{blocked}");
        return;
    }
    assert!(blocked.to_string().contains("direnv allow"), "{blocked}");
    let approved = Command::new("direnv")
        .args(["allow", "."])
        .current_dir(workspace)
        .output()
        .await
        .unwrap();
    assert!(approved.status.success(), "fixture approval failed");
    let result = run().await.expect("approved file supplies the shell environment");
    assert!(result.output.contains("PTY:from-direnv:original"), "{}", result.output);
}
