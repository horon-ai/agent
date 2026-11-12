//! Deny-only filesystem path policy for the tools that touch files.
//!
//! A session may register one predicate that is consulted before `read`,
//! `write`, `edit`, `replace`, `apply_patch`, `ls`, or `agentgrep` runs. The
//! predicate can only refuse: there is no way to force a call through, and a
//! session that registers nothing behaves exactly as it did before this hook
//! existed.
//!
//! Two limits are deliberate and must not be papered over:
//!
//! - `bash` reaches every path through the shell, so it is outside this hook.
//! - A symbolic link can name an allowed path and resolve to a denied one.
//!
//! This is a guard against mistakes, not a sandbox.

use super::ToolContext;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock as StdRwLock};

/// Decides whether one resolved path is refused.
///
/// `Some(reason)` denies the call and the reason is what the caller sees;
/// `None` leaves the path to the tool's own checks. A predicate cannot allow a
/// path the tool would otherwise refuse, which is what "deny-wins" means here.
pub type PathDenyPredicate = Arc<dyn Fn(&Path) -> Option<String> + Send + Sync>;

struct SessionPathPolicy {
    deny: PathDenyPredicate,
    owner: Option<u64>,
}

static SESSION_PATH_POLICIES: LazyLock<StdRwLock<HashMap<String, SessionPathPolicy>>> =
    LazyLock::new(|| StdRwLock::new(HashMap::new()));
static NEXT_SESSION_PATH_POLICY_OWNER: AtomicU64 = AtomicU64::new(1);

/// Removes a session's policy when the owner that registered it leaves memory.
///
/// The owner token keeps a stale owner from removing the policy a successor
/// installed for the same persisted session ID.
pub struct SessionPathPolicyRegistration {
    session_id: String,
    owner: u64,
}

impl Drop for SessionPathPolicyRegistration {
    fn drop(&mut self) {
        let mut policies = SESSION_PATH_POLICIES
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if policies
            .get(&self.session_id)
            .is_some_and(|policy| policy.owner == Some(self.owner))
        {
            policies.remove(&self.session_id);
        }
    }
}

/// Install the deny predicate for `session_id`, replacing any earlier one.
pub fn register_session_path_policy(session_id: &str, deny: PathDenyPredicate) -> SessionPathPolicyRegistration {
    let owner = NEXT_SESSION_PATH_POLICY_OWNER.fetch_add(1, Ordering::Relaxed);
    let mut policies = SESSION_PATH_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    policies.insert(
        session_id.to_string(),
        SessionPathPolicy {
            deny,
            owner: Some(owner),
        },
    );
    SessionPathPolicyRegistration {
        session_id: session_id.to_string(),
        owner,
    }
}

#[cfg(test)]
pub(crate) fn clear_session_path_policy(session_id: &str) {
    let mut policies = SESSION_PATH_POLICIES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    policies.remove(session_id);
}

fn session_path_policy(session_id: &str) -> Option<PathDenyPredicate> {
    SESSION_PATH_POLICIES
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id)
        .map(|policy| Arc::clone(&policy.deny))
}

/// The refusal `apply_patch` reports for a deletion of a catastrophic path.
///
/// The check lives here rather than inside `apply_patch` so every denial the
/// file tools emit comes from one module, and the wording the tool already
/// published stays byte-identical.
pub(crate) fn catastrophic_delete_refusal(path: &Path, display: &str, ctx: &ToolContext) -> Option<String> {
    let risk_ctx = horon_agent_command_risk::RiskContext::from_env(ctx.working_dir.clone());
    horon_agent_command_risk::is_catastrophic_target(path, &risk_ctx)
        .then(|| format!("✗ {display}: refused, this path is protected and must never be deleted by an agent"))
}

/// Refuse `tool` when the session's policy denies any path the call names.
///
/// Called by each tool before it reads or writes anything, so a refusal costs
/// nothing and leaves the filesystem untouched.
pub(crate) fn enforce(tool: &str, input: &Value, ctx: &ToolContext) -> anyhow::Result<()> {
    let Some(deny) = session_path_policy(&ctx.session_id) else {
        return Ok(());
    };
    for path in resolved_paths(tool, input, ctx) {
        if let Some(reason) = deny(&path) {
            return Err(anyhow::anyhow!("Tool call blocked by path policy: {reason}"));
        }
    }
    Ok(())
}

/// Paths a call names, resolved against the session working directory.
///
/// Each tool reads its paths from a different key. `agentgrep` also accepts
/// `file`/`file_path` for a single-file search; its `pattern` key is the search
/// query, not a path, so it is not consulted here. `apply_patch` names its paths
/// inside the patch text, including move destinations, and `hashline_edit` names
/// a move destination in `move_to`.
fn resolved_paths(tool: &str, input: &Value, ctx: &ToolContext) -> Vec<PathBuf> {
    named_paths(tool, input, ctx.working_dir.as_deref())
}

/// The text a call gives for each path it names, before any resolution.
///
/// The approval gate matches `[tools] restricted_paths` against this text as
/// well as the resolved path: a glob can name what the user typed, and a path
/// that does not resolve (a file that does not exist yet) still has to be
/// matchable.
pub(crate) fn named_path_texts<'a>(tool: &str, input: &'a Value) -> Vec<&'a str> {
    let keys: &[&str] = match tool {
        "read" | "write" | "edit" => &["file_path"],
        "replace" => &["path", "file_path"],
        "ls" => &["path"],
        "agentgrep" => &["path", "file", "file_path"],
        "lsp" => &["file"],
        "hashline_edit" => &["file_path", "move_to"],
        "apply_patch" => &["file_path", "path"],
        _ => &[],
    };
    let Some(object) = input.as_object() else {
        return Vec::new();
    };

    let mut named: Vec<&str> = Vec::new();
    for key in keys {
        if let Some(path) = object.get(*key).and_then(Value::as_str) {
            // A URI-shaped reference names content its reader resolves
            // (`agent://`, `artifact://`, `history://`, `memory://`, `skill://`), not a file
            // in the working directory; a deny rule on the literal text would
            // reject the whole read. The `skill://` reader checks the real file.
            if is_local_uri(path) {
                continue;
            }
            named.push(path);
        }
    }
    if tool == "apply_patch"
        && let Some(patch) = object.get("patch_text").and_then(Value::as_str)
    {
        named.extend(patch_paths(patch));
    }

    named.retain(|path| !is_local_uri(path));
    named
}

/// The paths `tool` names, resolved against `working_dir`.
///
/// Shared with the approval gate so a grant is written for exactly the path the
/// call will touch instead of a second, drifting key table.
pub(crate) fn named_paths(tool: &str, input: &Value, working_dir: Option<&Path>) -> Vec<PathBuf> {
    let named = named_path_texts(tool, input);
    if named.is_empty() {
        return Vec::new();
    }
    named
        .into_iter()
        .map(|path| match path {
            path if Path::new(path).is_absolute() => PathBuf::from(path),
            path => match working_dir {
                Some(base) => base.join(path),
                None => PathBuf::from(path),
            },
        })
        .collect()
}

/// Paths a tool call names in its input, labelled by the input key they
/// came from, resolved against the session working directory. Covers the
/// common path keys, `paths` arrays (first 8), and `apply_patch` file
/// headers (Codex envelope and unified diff).
pub(crate) fn touched_paths(input: &Value, ctx: &ToolContext) -> Vec<(&'static str, std::path::PathBuf)> {
    let mut touched = Vec::new();
    let Some(object) = input.as_object() else {
        return touched;
    };
    let resolve = |path: &str| ctx.resolve_path(std::path::Path::new(path));
    for key in ["file_path", "path", "target", "target_path", "old_path", "new_path"] {
        if let Some(path) = object.get(key).and_then(Value::as_str) {
            touched.push((key, resolve(path)));
        }
    }
    if let Some(paths) = object.get("paths").and_then(Value::as_array) {
        for path in paths.iter().filter_map(Value::as_str).take(8) {
            touched.push(("paths", resolve(path)));
        }
    }
    if let Some(patch) = object.get("patch_text").and_then(Value::as_str) {
        for path in patch_paths(patch) {
            if !touched.iter().any(|(_, existing)| *existing == resolve(path)) {
                touched.push(("patch", resolve(path)));
            }
        }
    }
    touched
}

/// File paths named in patch headers. Deletions are included because the
/// path was still touched; `/dev/null` sides of unified diffs are not.
pub(crate) fn patch_paths(patch: &str) -> Vec<&str> {
    let mut paths = Vec::new();
    for line in patch.lines() {
        let path = [
            "*** Add File: ",
            "*** Update File: ",
            "*** Delete File: ",
            "*** Move to: ",
        ]
        .iter()
        .find_map(|prefix| line.strip_prefix(prefix))
        .or_else(|| {
            let header = line.strip_prefix("+++ ").or_else(|| line.strip_prefix("--- "))?;
            let header = header.split('\t').next().unwrap_or(header).trim();
            if header == "/dev/null" {
                return None;
            }
            Some(
                header
                    .strip_prefix("a/")
                    .or_else(|| header.strip_prefix("b/"))
                    .unwrap_or(header),
            )
        });
        if let Some(path) = path.map(str::trim).filter(|path| !path.is_empty())
            && !paths.contains(&path)
        {
            paths.push(path);
        }
    }
    paths
}

/// Paths that name local data rather than a filesystem location. The deny
/// policy cannot reason about them, and resolving them would invent a
/// nonsense path in the working directory.
fn is_local_uri(path: &str) -> bool {
    ["agent://", "artifact://", "history://", "memory://", "skill://"]
        .iter()
        .any(|prefix| {
            path.get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        })
}

#[cfg(test)]
mod path_policy_tests {
    use super::*;
    use crate::tool::agentgrep::AgentGrepTool;
    use crate::tool::{
        Tool, ToolExecutionMode, apply_patch::ApplyPatchTool, edit::EditTool, ls::LsTool, read::ReadTool,
        replace::ReplaceTool, write::WriteTool,
    };
    use serde_json::json;

    fn context(session: &str, working_dir: &Path) -> ToolContext {
        ToolContext {
            session_id: session.to_string(),
            message_id: "message".to_string(),
            tool_call_id: "call".to_string(),
            working_dir: Some(working_dir.to_path_buf()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        }
    }

    fn deny_under(root: &Path) -> PathDenyPredicate {
        let root = root.to_path_buf();
        Arc::new(move |path: &Path| {
            path.starts_with(root.as_path())
                .then(|| format!("{} is outside the allowed workspace", path.display()))
        })
    }

    fn workspace() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("create workspace");
        (temp, workspace)
    }

    fn assert_blocked(error: &anyhow::Error) {
        assert!(
            error.to_string().starts_with("Tool call blocked by path policy:"),
            "a denial must use the path policy error, got: {error}"
        );
    }

    #[tokio::test]
    async fn path_policy_denies_write() {
        let (temp, workspace) = workspace();
        let session = "path-policy-denies-write";
        let _guard = register_session_path_policy(session, deny_under(&workspace));
        let target = workspace.join("denied.txt");

        let error = WriteTool::new()
            .execute(
                json!({"file_path": target, "content": "secret"}),
                context(session, temp.path()),
            )
            .await
            .expect_err("a denied write must not run");

        assert_blocked(&error);
        assert!(!target.exists(), "a denied write must leave no file behind");
    }

    #[tokio::test]
    async fn path_policy_denies_read() {
        let (temp, workspace) = workspace();
        let session = "path-policy-denies-read";
        let target = workspace.join("kept.txt");
        std::fs::write(&target, "content").expect("seed file");
        let _guard = register_session_path_policy(session, deny_under(&workspace));

        let error = ReadTool::new()
            .execute(json!({"file_path": target}), context(session, temp.path()))
            .await
            .expect_err("a denied read must not run");

        assert_blocked(&error);
    }

    #[tokio::test]
    async fn path_policy_denies_edit() {
        let (temp, workspace) = workspace();
        let session = "path-policy-denies-edit";
        let target = workspace.join("kept.txt");
        std::fs::write(&target, "before").expect("seed file");
        let _guard = register_session_path_policy(session, deny_under(&workspace));

        let error = EditTool::new()
            .execute(
                json!({"file_path": target, "old_string": "before", "new_string": "after"}),
                context(session, temp.path()),
            )
            .await
            .expect_err("a denied edit must not run");

        assert_blocked(&error);
        assert_eq!(
            std::fs::read_to_string(&target).expect("read back"),
            "before",
            "a denied edit must leave the file untouched"
        );
    }

    #[tokio::test]
    async fn path_policy_denies_replace() {
        let (temp, workspace) = workspace();
        let session = "path-policy-denies-replace";
        let target = workspace.join("kept.txt");
        std::fs::write(&target, "before").expect("seed file");
        let _guard = register_session_path_policy(session, deny_under(&workspace));

        let error = ReplaceTool::new()
            .execute(
                json!({"pattern": "before", "replacement": "after", "path": workspace}),
                context(session, temp.path()),
            )
            .await
            .expect_err("a denied replace must not run");

        assert_blocked(&error);
        assert_eq!(
            std::fs::read_to_string(&target).expect("read back"),
            "before",
            "a denied replace must leave the file untouched"
        );
    }

    #[tokio::test]
    async fn path_policy_denies_apply_patch() {
        let (temp, workspace) = workspace();
        let session = "path-policy-denies-apply-patch";
        let target = workspace.join("added.txt");
        let _guard = register_session_path_policy(session, deny_under(&workspace));
        let patch = format!(
            "*** Begin Patch\n*** Add File: {}\n+content\n*** End Patch\n",
            target.display()
        );

        let error = ApplyPatchTool::new()
            .execute(json!({"patch_text": patch}), context(session, temp.path()))
            .await
            .expect_err("a denied patch must not run");

        assert_blocked(&error);
        assert!(!target.exists(), "a denied patch must not create its file");
    }

    #[tokio::test]
    async fn path_policy_denies_ls() {
        let (temp, workspace) = workspace();
        let session = "path-policy-denies-ls";
        let _guard = register_session_path_policy(session, deny_under(&workspace));

        let error = LsTool::new()
            .execute(json!({"path": workspace}), context(session, temp.path()))
            .await
            .expect_err("a denied listing must not run");

        assert_blocked(&error);
    }

    #[tokio::test]
    async fn path_policy_denies_agentgrep() {
        let (temp, workspace) = workspace();
        let session = "path-policy-denies-agentgrep";
        std::fs::write(workspace.join("kept.txt"), "needle").expect("seed file");
        let _guard = register_session_path_policy(session, deny_under(&workspace));

        let error = AgentGrepTool::new()
            .execute(
                json!({"query": "needle", "path": workspace}),
                context(session, temp.path()),
            )
            .await
            .expect_err("a denied search must not run");

        assert_blocked(&error);
    }

    #[tokio::test]
    async fn path_policy_empty_allows() {
        let (temp, workspace) = workspace();
        let session = "path-policy-empty";
        clear_session_path_policy(session);
        let target = workspace.join("allowed.txt");

        let output = WriteTool::new()
            .execute(
                json!({"file_path": target, "content": "content"}),
                context(session, temp.path()),
            )
            .await
            .expect("an unregistered session keeps the old behavior");

        assert!(output.output.contains("allowed.txt"));
        assert!(target.exists(), "an unregistered session still writes");
    }
}
