#![cfg_attr(test, allow(clippy::await_holding_lock))]

use crate::agent::Agent;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::server::SwarmState;
use crate::server::comm_session::spawn_swarm_agent;
use crate::server::comm_worktree::{self, IsoApply};
use crate::tool::Registry;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use tokio::sync::{Mutex, RwLock, broadcast};

struct MockProvider;

#[async_trait]
impl Provider for MockProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Err(anyhow::anyhow!("mock provider should not be called"))
    }

    fn name(&self) -> &str {
        "mock"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(MockProvider)
    }
}

struct ScratchRepo {
    _temp: tempfile::TempDir,
    path: String,
}

impl ScratchRepo {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().canonicalize().unwrap().join("project");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.to_str().unwrap().to_string();
        git(&path, &["init", "-b", "main"]);
        git(&path, &["config", "user.name", "Worktree Test"]);
        git(&path, &["config", "user.email", "worktree-test@example.invalid"]);
        std::fs::write(Path::new(&path).join("shared.txt"), "one\ntwo\nthree\n").unwrap();
        git(&path, &["add", "shared.txt"]);
        git(&path, &["commit", "-m", "initial"]);
        Self { _temp: temp, path }
    }

    fn read(&self, file: &str) -> String {
        std::fs::read_to_string(Path::new(&self.path).join(file)).unwrap()
    }

    fn write(&self, dir: &str, file: &str, content: &str) {
        std::fs::write(Path::new(dir).join(file), content).unwrap();
    }
}

fn git(dir: &str, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn branch_exists(dir: &str, branch: &str) -> bool {
    std::process::Command::new("git")
        .current_dir(dir)
        .args(["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .status()
        .expect("git runs")
        .success()
}

#[tokio::test]
async fn swarm_worktree_two_children_apply() {
    let repo = ScratchRepo::new();
    let first = tokio::task::spawn_blocking({
        let path = repo.path.clone();
        move || {
            comm_worktree::prepare(
                &path,
                "session_first",
                comm_worktree::Isolation::Worktree,
                IsoApply::Apply,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let second = tokio::task::spawn_blocking({
        let path = repo.path.clone();
        move || {
            comm_worktree::prepare(
                &path,
                "session_second",
                comm_worktree::Isolation::Worktree,
                IsoApply::Apply,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    assert_ne!(first.path, second.path);
    assert_ne!(first.branch, second.branch);

    repo.write(&first.path, "shared.txt", "ONE\ntwo\nthree\n");
    repo.write(&second.path, "shared.txt", "one\ntwo\nTHREE\n");

    let first_report = tokio::task::spawn_blocking(move || comm_worktree::finish(&first, "swarm: first"))
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(first_report, comm_worktree::WorktreeReport::Applied { .. }),
        "{first_report:?}"
    );
    let second_report = tokio::task::spawn_blocking(move || comm_worktree::finish(&second, "swarm: second"))
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(second_report, comm_worktree::WorktreeReport::Applied { .. }),
        "{second_report:?}"
    );

    assert_eq!(repo.read("shared.txt"), "ONE\ntwo\nTHREE\n");
    assert!(!branch_exists(&repo.path, "swarm/session_first"));
    assert!(!branch_exists(&repo.path, "swarm/session_second"));
    let status = std::process::Command::new("git")
        .current_dir(&repo.path)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(status.stdout.is_empty(), "parent worktree must be clean");
}

#[tokio::test]
async fn swarm_worktree_conflict_is_kept() {
    let repo = ScratchRepo::new();
    let first = tokio::task::spawn_blocking({
        let path = repo.path.clone();
        move || {
            comm_worktree::prepare(
                &path,
                "session_conflict_a",
                comm_worktree::Isolation::Worktree,
                IsoApply::Apply,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    let second = tokio::task::spawn_blocking({
        let path = repo.path.clone();
        move || {
            comm_worktree::prepare(
                &path,
                "session_conflict_b",
                comm_worktree::Isolation::Worktree,
                IsoApply::Apply,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();

    repo.write(&first.path, "shared.txt", "one\nFIRST\nthree\n");
    repo.write(&second.path, "shared.txt", "one\nSECOND\nthree\n");

    let first_report = tokio::task::spawn_blocking(move || comm_worktree::finish(&first, "swarm: first"))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(first_report, comm_worktree::WorktreeReport::Applied { .. }));

    let kept_path = second.path.clone();
    let conflict = tokio::task::spawn_blocking(move || comm_worktree::finish(&second, "swarm: second"))
        .await
        .unwrap()
        .unwrap();
    let comm_worktree::WorktreeReport::Conflict { files, message, .. } = &conflict else {
        panic!("expected a conflict, got {conflict:?}");
    };
    assert_eq!(files, &vec!["shared.txt".to_string()]);
    assert!(!message.is_empty());
    assert!(
        conflict.describe_with("Worktree isolation").contains(&kept_path),
        "the report must name the kept worktree: {}",
        conflict.describe_with("Worktree isolation")
    );
    assert_eq!(repo.read("shared.txt"), "one\nFIRST\nthree\n");
    assert!(Path::new(&kept_path).exists());
    assert_eq!(
        std::fs::read_to_string(Path::new(&kept_path).join("shared.txt")).unwrap(),
        "one\nSECOND\nthree\n"
    );
    assert!(branch_exists(&repo.path, "swarm/session_conflict_b"));
}

#[tokio::test]
async fn swarm_worktree_drop_refuses_dirty_worktree() {
    let repo = ScratchRepo::new();
    let child = tokio::task::spawn_blocking({
        let path = repo.path.clone();
        move || {
            comm_worktree::prepare(
                &path,
                "session_dirty",
                comm_worktree::Isolation::Worktree,
                IsoApply::Drop,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    repo.write(&child.path, "shared.txt", "uncommitted\n");
    let path = child.path.clone();
    let report = tokio::task::spawn_blocking(move || comm_worktree::finish(&child, "swarm: dirty"))
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(report, comm_worktree::WorktreeReport::Dirty { .. }),
        "{report:?}"
    );
    assert!(Path::new(&path).exists());
    assert_eq!(
        std::fs::read_to_string(Path::new(&path).join("shared.txt")).unwrap(),
        "uncommitted\n"
    );
    assert_eq!(repo.read("shared.txt"), "one\ntwo\nthree\n");
}

#[tokio::test]
async fn swarm_worktree_cancelled_child_leaves_no_half_merge() {
    let repo = ScratchRepo::new();
    let child = tokio::task::spawn_blocking({
        let path = repo.path.clone();
        move || {
            comm_worktree::prepare(
                &path,
                "session_cancelled",
                comm_worktree::Isolation::Worktree,
                IsoApply::Apply,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    repo.write(&child.path, "shared.txt", "alpha\nHALF-WRITTEN\ngamma\n");
    let path = child.path.clone();
    let branch = child.branch.clone();
    let report = comm_worktree::failure_report(&child, Some("partial work".to_string()));
    assert!(report.starts_with("partial work"));
    assert!(report.contains(&path), "{report}");
    assert!(report.contains(&branch), "{report}");
    assert_eq!(repo.read("shared.txt"), "one\ntwo\nthree\n");
    assert!(branch_exists(&repo.path, "swarm/session_cancelled"));
    assert_eq!(
        std::fs::read_to_string(Path::new(&path).join("shared.txt")).unwrap(),
        "alpha\nHALF-WRITTEN\ngamma\n"
    );
}

#[tokio::test]
async fn swarm_default_isolation_unchanged() {
    let _env = crate::storage::lock_test_env();
    let temp = tempfile::TempDir::new().unwrap();
    let previous = std::env::var_os("HORON_AGENT_HOME");
    crate::env::set_var("HORON_AGENT_HOME", temp.path());

    let repo = ScratchRepo::new();
    let provider: Arc<dyn Provider> = Arc::new(MockProvider);
    let registry = Registry::new(provider.clone()).await;
    let mut session = crate::session::Session::create_with_id("isolation_coord".to_string(), None, None);
    session.model = Some("mock".to_string());
    session.working_dir = Some(repo.path.clone());
    let agent = Agent::new_with_session(provider.clone(), registry, session, None);
    let sessions = Arc::new(RwLock::new(HashMap::from([(
        "isolation_coord".to_string(),
        Arc::new(Mutex::new(agent)),
    )])));
    let state = SwarmState::new(HashMap::new(), HashMap::new(), HashMap::new(), HashMap::new());
    let mcp = Arc::new(crate::mcp::SharedMcpPool::new(crate::mcp::McpConfig::default()));

    let child_id = spawn_swarm_agent(
        "isolation_coord",
        "isolation-swarm",
        Some(repo.path.clone()),
        None,
        Some(crate::config::SwarmSpawnMode::Headless),
        None,
        None,
        Some("isolation default".into()),
        crate::server::comm_worktree::Isolation::None,
        IsoApply::Apply,
        None,
        &sessions,
        &Arc::new(RwLock::new(String::new())),
        &provider,
        &state.members,
        &state.swarms_by_id,
        &state.coordinators,
        &state.plans,
        &Arc::new(RwLock::new(std::collections::VecDeque::new())),
        &Arc::new(AtomicU64::new(0)),
        &broadcast::channel(16).0,
        &mcp,
        &Arc::new(RwLock::new(HashMap::new())),
        &Arc::new(RwLock::new(HashMap::new())),
    )
    .await
    .expect("default spawn succeeds");

    let child = sessions.read().await.get(&child_id).cloned().unwrap();
    let working_dir = child.lock().await.working_dir().map(str::to_string);
    assert_eq!(working_dir.as_deref(), Some(repo.path.as_str()));
    assert!(!temp.path().join("project-worktrees").exists());

    if let Some(previous) = previous {
        crate::env::set_var("HORON_AGENT_HOME", previous);
    } else {
        crate::env::remove_var("HORON_AGENT_HOME");
    }
}

/// A PID that is certainly not running: a child that has already exited.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .expect("sh runs");
    let pid = child.id();
    child.wait().unwrap();
    pid
}

fn overwrite_marker_pid(checkout: &str, pid: u32) {
    let mut marker = crate::git_clone_isolation::read_owner_marker(Path::new(checkout)).expect("marker exists");
    marker.pid = pid;
    std::fs::write(
        format!("{checkout}.owner.json"),
        serde_json::to_string(&marker).unwrap(),
    )
    .unwrap();
}

#[tokio::test]
async fn swarm_clone_child_apply_merges_and_removes_the_clone() {
    let repo = ScratchRepo::new();
    let child = tokio::task::spawn_blocking({
        let path = repo.path.clone();
        move || comm_worktree::prepare(&path, "session_clone", comm_worktree::Isolation::Clone, IsoApply::Apply)
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(child.isolation, comm_worktree::Isolation::Clone);
    assert!(
        child.checkout.contains("-clones/"),
        "clone lives under the sibling clones root: {}",
        child.checkout
    );
    assert!(Path::new(&child.checkout).join(".git").is_dir());
    // The clone's branch is private until settle fetches it back.
    assert!(!branch_exists(&repo.path, "swarm/session_clone"));
    repo.write(&child.path, "shared.txt", "CLONE\ntwo\nthree\n");

    let clone_root = child.checkout.clone();
    let report = tokio::task::spawn_blocking(move || comm_worktree::settle(&child, "swarm: clone", None))
        .await
        .unwrap();
    assert!(report.starts_with("Clone isolation: merged branch"), "{report}");
    assert_eq!(repo.read("shared.txt"), "CLONE\ntwo\nthree\n");
    assert!(!Path::new(&clone_root).exists(), "clone removed after apply");
    assert!(
        !Path::new(&format!("{clone_root}.owner.json")).exists(),
        "owner marker removed with the clone"
    );
    assert!(!branch_exists(&repo.path, "swarm/session_clone"));
}

#[tokio::test]
async fn swarm_clone_child_keep_reports_the_diffstat() {
    let repo = ScratchRepo::new();
    let child = tokio::task::spawn_blocking({
        let path = repo.path.clone();
        move || {
            comm_worktree::prepare(
                &path,
                "session_clone_keep",
                comm_worktree::Isolation::Clone,
                IsoApply::Keep,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    repo.write(&child.path, "shared.txt", "ONE\ntwo\nthree\n");
    let clone_root = child.checkout.clone();
    let report = tokio::task::spawn_blocking(move || comm_worktree::settle(&child, "swarm: keep", None))
        .await
        .unwrap();
    assert!(report.starts_with("Clone isolation: left branch"), "{report}");
    assert!(report.contains(&clone_root), "{report}");
    assert!(
        report.contains("shared.txt"),
        "diffstat names the changed file: {report}"
    );
    assert_eq!(repo.read("shared.txt"), "one\ntwo\nthree\n");
    assert!(Path::new(&clone_root).exists(), "kept clone stays for inspection");
}

#[tokio::test]
async fn swarm_prepare_reaps_a_dead_clean_clone() {
    let repo = ScratchRepo::new();
    let child = tokio::task::spawn_blocking({
        let path = repo.path.clone();
        move || {
            comm_worktree::prepare(
                &path,
                "session_orphan_clone",
                comm_worktree::Isolation::Clone,
                IsoApply::Keep,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();
    assert!(Path::new(&child.checkout).exists());
    overwrite_marker_pid(&child.checkout, dead_pid());

    let next = tokio::task::spawn_blocking({
        let path = repo.path.clone();
        move || {
            comm_worktree::prepare(
                &path,
                "session_after_sweep",
                comm_worktree::Isolation::Worktree,
                IsoApply::Apply,
            )
        }
    })
    .await
    .unwrap()
    .unwrap();

    assert!(
        !Path::new(&child.checkout).exists(),
        "prepare sweeps a dead, clean clone"
    );
    assert!(Path::new(&next.path).exists());
}
