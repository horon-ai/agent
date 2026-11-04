//! Injects ancestor `AGENTS.md` files into `read` results, so a session sees the
//! rules of directories it actually reads from. Purely additive: nothing here
//! parses, merges, or enforces file contents.

use super::ToolContext;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub(crate) const MAX_BYTES_PER_FILE: usize = 16 * 1024;
pub(crate) const MAX_BYTES_PER_READ: usize = 32 * 1024;
/// At or above this many bytes of base output no injection is appended. The cap is
/// a conservative budget choice, not where the tool-result guard begins withholding:
/// between the two a result keeps its body but carries no contracts, and nothing says so.
pub(crate) const MAX_BASE_OUTPUT_BYTES: usize = 32 * 1024;
pub(crate) const MAX_TRACKED_SESSIONS: usize = 64;
/// Held back out of `MAX_BYTES_PER_READ` so the note naming skipped ancestors
/// fits whenever the budget runs out, instead of the reader losing that news to
/// the same ceiling that caused it.
const SKIPPED_MARKER_RESERVE: usize = 256;

const AGENTS_MD: &str = "AGENTS.md";

static INJECTED: OnceLock<Mutex<InjectedSessions>> = OnceLock::new();

#[derive(Default)]
struct InjectedSessions {
    sessions: HashMap<String, HashSet<PathBuf>>,
    order: VecDeque<String>,
}

impl InjectedSessions {
    fn ensure_session(&mut self, session_id: &str) {
        if self.sessions.contains_key(session_id) {
            return;
        }
        self.sessions.insert(session_id.to_string(), HashSet::new());
        self.order.push_back(session_id.to_string());
        while self.order.len() > MAX_TRACKED_SESSIONS {
            if let Some(oldest) = self.order.pop_front() {
                self.sessions.remove(&oldest);
            }
        }
    }

    fn already_injected(&self, session_id: &str, path: &Path) -> bool {
        self.sessions.get(session_id).is_some_and(|paths| paths.contains(path))
    }

    fn mark_injected(&mut self, session_id: &str, path: PathBuf) {
        if let Some(paths) = self.sessions.get_mut(session_id) {
            paths.insert(path);
        }
    }
}

fn cache() -> &'static Mutex<InjectedSessions> {
    INJECTED.get_or_init(|| Mutex::new(InjectedSessions::default()))
}

#[cfg(test)]
pub(crate) fn clear_session_cache_for_test() {
    let mut guard = match cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.sessions.clear();
    guard.order.clear();
}

pub(crate) fn inject_for_read(file_path: &Path, base_output_bytes: usize, ctx: &ToolContext) -> Option<String> {
    if !crate::config::config().tools.nested_agents_md {
        return None;
    }
    let Some(working_dir) = ctx.working_dir.as_deref() else {
        crate::logging::info("nested_agents: no working_dir; skipping ancestor AGENTS.md injection");
        return None;
    };
    let (file, root) = contain(file_path, working_dir)?;
    let candidates = find_agents_md_up(&file, &root);
    if candidates.is_empty() {
        return None;
    }
    if base_output_bytes >= MAX_BASE_OUTPUT_BYTES {
        return None;
    }

    let mut guard = match cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.ensure_session(&ctx.session_id);

    let mut out = String::new();
    let mut budget = MAX_BYTES_PER_READ;
    let mut skipped_ancestors = 0;
    for (index, path) in candidates.iter().enumerate() {
        if guard.already_injected(&ctx.session_id, path) {
            continue;
        }
        // The caps cover the bytes this read appends, not just the file body, so
        // the header and the truncation note are measured first and the content
        // is sized to what they leave. Blocks also leave
        // `SKIPPED_MARKER_RESERVE` of the budget untouched, so the note naming
        // skipped ancestors always has room when the budget runs out.
        let usable = budget.saturating_sub(SKIPPED_MARKER_RESERVE);
        let header = block_header(path);
        let room = MAX_BYTES_PER_FILE.min(usable).saturating_sub(header.len());
        if room == 0 {
            // Not even the header fits, and no later ancestor has a shorter one.
            skipped_ancestors = not_yet_injected(&candidates[index..], &guard, &ctx.session_id);
            break;
        }
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(err) => {
                crate::logging::warn(&format!("nested_agents: failed to read {}: {err}", path.display()));
                continue;
            }
        };
        let note = truncated_note(path);
        let limit = if content.len() <= room {
            content.len()
        } else {
            room.saturating_sub(note.len())
        };
        if limit == 0 && !content.is_empty() {
            // A cut body has to say so, and its note does not fit: stop here.
            skipped_ancestors = not_yet_injected(&candidates[index..], &guard, &ctx.session_id);
            break;
        }
        let (text, truncated) = truncate_utf8(&content, limit);
        let mut chunk = header;
        chunk.push_str(&text);
        if truncated {
            chunk.push_str(&note);
        }
        budget = budget.saturating_sub(chunk.len());
        out.push_str(&chunk);
        guard.mark_injected(&ctx.session_id, path.clone());
    }

    if skipped_ancestors > 0 {
        // Naming what was left out matters, but the read cap is the harder
        // promise: a marker that does not fit is dropped rather than pushing the
        // appended text past `MAX_BYTES_PER_READ`. The reserve above keeps room
        // for it in every read that fits a block at all.
        let marker = skipped_note(skipped_ancestors);
        if marker.len() <= budget {
            out.push_str(&marker);
        }
    }

    (!out.is_empty()).then_some(out)
}

fn not_yet_injected(paths: &[PathBuf], injected: &InjectedSessions, session_id: &str) -> usize {
    paths
        .iter()
        .filter(|path| !injected.already_injected(session_id, path))
        .count()
}

/// Per-session dedupe for glob-triggered foreign rule injection, with the same
/// bounded lifetime model as the AGENTS.md cache above. Keys are the rule
/// paths as the workspace reports them, so a rule read twice in one session is
/// injected once.
static RULE_INJECTED: std::sync::LazyLock<Mutex<RuleInjectedSessions>> =
    std::sync::LazyLock::new(|| Mutex::new(RuleInjectedSessions::default()));

#[derive(Default)]
struct RuleInjectedSessions {
    sessions: HashMap<String, HashSet<String>>,
    order: VecDeque<String>,
}

impl RuleInjectedSessions {
    fn ensure_session(&mut self, session_id: &str) {
        if self.sessions.contains_key(session_id) {
            return;
        }
        self.sessions.insert(session_id.to_string(), HashSet::new());
        self.order.push_back(session_id.to_string());
        while self.order.len() > MAX_TRACKED_SESSIONS {
            if let Some(oldest) = self.order.pop_front() {
                self.sessions.remove(&oldest);
            }
        }
    }

    fn already_injected(&self, session_id: &str, path: &str) -> bool {
        self.sessions.get(session_id).is_some_and(|paths| paths.contains(path))
    }

    fn mark_injected(&mut self, session_id: &str, path: String) {
        if let Some(paths) = self.sessions.get_mut(session_id) {
            paths.insert(path);
        }
    }
}

fn rule_cache() -> &'static Mutex<RuleInjectedSessions> {
    &RULE_INJECTED
}

#[cfg(test)]
pub(crate) fn clear_rule_session_cache_for_test() {
    let mut guard = match rule_cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.sessions.clear();
    guard.order.clear();
}

/// Append the bodies of foreign rules whose globs match the file a session just
/// touched.
///
/// Only rulebook mode injects: inline mode already put every body in the system
/// prompt, so a second copy here would duplicate context. Each rule is injected
/// at most once per session, and the appended text is bounded by the same
/// per-read budget the AGENTS.md injection uses.
pub(crate) fn inject_rules_for_path(file_path: &Path, ctx: &ToolContext) -> Option<String> {
    let config = crate::config::config();
    if !config.features.foreign_rules || config.features.foreign_rules_mode != crate::config::ForeignRulesMode::Rulebook
    {
        return None;
    }
    let working_dir = ctx.working_dir.as_deref()?;
    let relative = relative_to_working_dir(file_path, working_dir)?;
    let rules = horon_agent_base::foreign_rules::discover(working_dir);
    if rules.is_empty() {
        return None;
    }

    let mut guard = match rule_cache().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.ensure_session(&ctx.session_id);

    let mut out = String::new();
    for rule in &rules {
        if out.len() >= MAX_BYTES_PER_READ {
            break;
        }
        if rule.scope == "always" || rule.globs.is_empty() {
            continue;
        }
        if !rule.globs.iter().any(|pattern| glob_matches(pattern, &relative)) {
            continue;
        }
        if guard.already_injected(&ctx.session_id, &rule.path) {
            continue;
        }
        let room = MAX_BYTES_PER_FILE.min(MAX_BYTES_PER_READ.saturating_sub(out.len()));
        let (body, truncated) = truncate_utf8(&rule.body, room.saturating_sub(64));
        out.push_str(&format!(
            "\n\n<project-rule path=\"{}\">\n{}\n</project-rule>",
            rule.path, body
        ));
        if truncated {
            out.push_str(&format!(
                "\n[Note: rule body truncated to save context window; read the rule directly for the rest: {}]",
                rule.path
            ));
        }
        guard.mark_injected(&ctx.session_id, rule.path.clone());
    }

    (!out.is_empty()).then_some(out)
}

/// The touched file's path relative to the working directory, with `/`
/// separators. An absolute file outside the working directory never matches a
/// project glob.
fn relative_to_working_dir(file_path: &Path, working_dir: &Path) -> Option<String> {
    let relative = if file_path.is_absolute() {
        file_path.strip_prefix(working_dir).ok()?
    } else {
        file_path
    };
    Some(
        relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
    )
}

fn glob_matches(pattern: &str, relative: &str) -> bool {
    glob::Pattern::new(pattern).is_ok_and(|compiled| compiled.matches(relative))
}

fn block_header(path: &Path) -> String {
    format!("\n\n[Directory Context: {}]\n", path.display())
}

fn truncated_note(path: &Path) -> String {
    format!(
        "\n\n[Note: content truncated to save context window; read the file directly for the rest: {}]",
        path.display()
    )
}

fn skipped_note(count: usize) -> String {
    let (noun, pronoun) = if count == 1 { ("file", "it") } else { ("files", "them") };
    format!(
        "\n\n[Note: {count} more ancestor AGENTS.md {noun} skipped: this read's context budget is full; read {pronoun} directly]"
    )
}

/// Canonicalizes both sides, so a symlink that escapes the root is rejected, and
/// the root itself does not count as inside it.
pub(crate) fn contain(file_path: &Path, root: &Path) -> Option<(PathBuf, PathBuf)> {
    let file = match std::fs::canonicalize(file_path) {
        Ok(path) => path,
        Err(_) => return None,
    };
    let root = match std::fs::canonicalize(root) {
        Ok(path) => path,
        Err(_) => return None,
    };
    if file == root || !file.starts_with(&root) {
        return None;
    }
    Some((file, root))
}

/// Walks from the directory of `start` up to but excluding `root`, and returns
/// the result root-to-leaf. `start` must already be canonical.
///
/// Candidates are canonicalized before they are compared, so a symlinked alias
/// of an `AGENTS.md` already on the list cannot inject the same file twice, and
/// a candidate whose canonical path leaves `root` is dropped: a committed
/// `AGENTS.md` symlink pointing outside the working directory would otherwise
/// read a file the session is not allowed to read and inject it as directory
/// context. `root` is canonical already, so both sides of the comparison are.
pub(crate) fn find_agents_md_up(start: &Path, root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    let mut current = start.parent();
    while let Some(dir) = current {
        if dir == root || !dir.starts_with(root) {
            break;
        }
        let candidate = dir.join(AGENTS_MD);
        if candidate.is_file()
            && let Ok(canonical) = std::fs::canonicalize(&candidate)
            && canonical.starts_with(root)
            && canonical != start
            && !found.contains(&canonical)
        {
            found.push(canonical);
        }
        current = dir.parent();
    }
    found.reverse();
    found
}

pub(crate) fn truncate_utf8(content: &str, max_bytes: usize) -> (String, bool) {
    let truncated = content.len() > max_bytes;
    (crate::util::truncate_str(content, max_bytes).to_string(), truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolExecutionMode;

    fn cache_test_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn ctx_for(session_id: &str, root: &Path) -> ToolContext {
        ToolContext {
            session_id: session_id.to_string(),
            message_id: "test-message".to_string(),
            tool_call_id: "test-call".to_string(),
            working_dir: Some(root.to_path_buf()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        }
    }

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, content).expect("write fixture");
    }

    fn tree() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize root");
        write(&root.join("AGENTS.md"), "root rules");
        write(&root.join("a/AGENTS.md"), "level a rules");
        write(&root.join("a/b/AGENTS.md"), "level b rules");
        write(&root.join("a/b/file.txt"), "file body");
        (temp, root)
    }

    #[test]
    fn injects_ancestors_root_to_leaf_and_skips_root_file() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let (_temp, root) = tree();
        let ctx = ctx_for("t1-happy", &root);
        let file = root.join("a/b/file.txt");

        let injected = inject_for_read(&file, 0, &ctx).expect("injection");
        println!("--- injected text for manual QA ---{injected}\n--- end ---");

        let a_header = format!("[Directory Context: {}]", root.join("a/AGENTS.md").display());
        let b_header = format!("[Directory Context: {}]", root.join("a/b/AGENTS.md").display());
        let root_header = format!("[Directory Context: {}]", root.join("AGENTS.md").display());

        let a_at = injected.find(&a_header).expect("level a header");
        let b_at = injected.find(&b_header).expect("level b header");
        assert!(a_at < b_at, "root-to-leaf order: {injected}");
        assert!(!injected.contains(&root_header), "root AGENTS.md: {injected}");
        assert!(injected.contains("level a rules"));
        assert!(injected.contains("level b rules"));
        assert!(!injected.contains("root rules"));
    }

    #[cfg(unix)]
    #[test]
    fn find_agents_md_up_dedupes_symlinked_aliases() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let temp = tempfile::TempDir::new().expect("temp dir");
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize root");
        write(&root.join("a/AGENTS.md"), "level a rules");
        write(&root.join("a/b/file.txt"), "file body");
        std::os::unix::fs::symlink(root.join("a/AGENTS.md"), root.join("a/b/AGENTS.md")).expect("symlink alias");

        let found = find_agents_md_up(&root.join("a/b/file.txt"), &root);
        println!("--- manual QA: deduped AGENTS.md candidates = {found:?} ---");

        assert_eq!(
            found,
            vec![root.join("a/AGENTS.md")],
            "the alias must not add a second entry"
        );
    }

    #[cfg(unix)]
    #[test]
    fn find_agents_md_up_rejects_a_symlinked_agents_md_that_escapes_root() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let temp = tempfile::TempDir::new().expect("temp dir");
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize root");
        write(&root.join("a/file.txt"), "file body");
        write(&root.join("a/b/AGENTS.md"), "in-root rules\n");
        write(&root.join("a/b/c/file.txt"), "file body");

        let outside = tempfile::TempDir::new().expect("outside dir");
        let secret = outside.path().join("credentials");
        write(&secret, "secret marker: OUTSIDE-SECRET-4c1d\n");
        let secret = std::fs::canonicalize(&secret).expect("canonicalize secret");
        std::os::unix::fs::symlink(&secret, root.join("a/AGENTS.md")).expect("symlink escape");
        std::os::unix::fs::symlink(&secret, root.join("a/b/c/AGENTS.md")).expect("symlink escape");

        let escaping_only = find_agents_md_up(&root.join("a/file.txt"), &root);
        println!(
            "--- manual QA: candidates under root/a = {escaping_only:?}; escaping target = {} ---",
            secret.display()
        );
        assert!(
            !escaping_only.contains(&secret),
            "a candidate that resolves outside root must be rejected: {escaping_only:?}"
        );
        assert!(
            escaping_only.is_empty(),
            "the only candidate here escapes root, so nothing is left: {escaping_only:?}"
        );

        let with_in_root = find_agents_md_up(&root.join("a/b/c/file.txt"), &root);
        println!("--- manual QA: candidates under root/a/b/c = {with_in_root:?} ---");
        assert_eq!(
            with_in_root,
            vec![root.join("a/b/AGENTS.md")],
            "only the genuine in-root contract may survive"
        );

        let ctx = ctx_for("t15-symlink-escape", &root);
        let injected = inject_for_read(&root.join("a/file.txt"), 0, &ctx);
        println!("--- manual QA: inject_for_read with only an escaping symlink = {injected:?} ---");
        assert!(
            injected.is_none(),
            "an escaping symlink leaves nothing to inject: {injected:?}"
        );

        let ctx = ctx_for("t15-symlink-escape-in-root", &root);
        let injected = inject_for_read(&root.join("a/b/c/file.txt"), 0, &ctx).expect("injection");
        println!("--- manual QA: inject_for_read with an in-root contract ---\n{injected}\n--- end ---");
        assert!(injected.contains("in-root rules"), "output={injected:?}");
        assert!(
            !injected.contains("OUTSIDE-SECRET-4c1d"),
            "the secret outside root must never reach the model: {injected:?}"
        );
        assert!(
            !injected.contains(&secret.display().to_string()),
            "the escaping path must not appear either: {injected:?}"
        );
    }

    #[test]
    fn contain_rejects_path_outside_root() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let (_temp, root) = tree();
        let outside = tempfile::TempDir::new().expect("outside dir");
        let outside_file = outside.path().join("other.txt");
        write(&outside_file, "outside body");

        assert!(contain(&outside_file, &root).is_none());
        assert!(contain(&root.join("a/b/file.txt"), &root).is_some());
        assert!(contain(&root, &root).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn contain_rejects_symlink_escaping_root() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let (_temp, root) = tree();
        let outside = tempfile::TempDir::new().expect("outside dir");
        let outside_file = outside.path().join("secret.txt");
        write(&outside_file, "outside body");
        std::os::unix::fs::symlink(outside.path(), root.join("escape")).expect("symlink");

        let escaped = root.join("escape/secret.txt");
        assert!(escaped.is_file(), "fixture resolves");
        assert!(contain(&escaped, &root).is_none());
        let ctx = ctx_for("t3-symlink", &root);
        assert!(inject_for_read(&escaped, 0, &ctx).is_none());
    }

    #[test]
    fn oversized_agents_md_is_truncated_and_keeps_utf8() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let temp = tempfile::TempDir::new().expect("temp dir");
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize root");
        let big = "가".repeat(20_000);
        write(&root.join("a/AGENTS.md"), &big);
        write(&root.join("a/file.txt"), "file body");
        let ctx = ctx_for("t4-truncate", &root);

        let injected = inject_for_read(&root.join("a/file.txt"), 0, &ctx).expect("injection");
        assert!(
            injected.contains("[Note:"),
            "note marker: {}",
            &injected[..200.min(injected.len())]
        );
        assert!(!injected.contains('\u{FFFD}'));
        assert!(injected.len() <= MAX_BYTES_PER_FILE);
        let bytes = injected.clone().into_bytes();
        assert!(String::from_utf8(bytes).is_ok(), "valid utf-8");

        let (text, truncated) = truncate_utf8(&big, 1000);
        assert!(truncated);
        assert!(text.len() <= 1000);
        assert!(big.starts_with(&text));
        let (whole, not_truncated) = truncate_utf8("short", 1000);
        assert_eq!(whole, "short");
        assert!(!not_truncated);
    }

    #[test]
    fn second_read_of_same_directory_injects_nothing() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let (_temp, root) = tree();
        let ctx = ctx_for("t5-repeat", &root);
        let file = root.join("a/b/file.txt");

        assert!(inject_for_read(&file, 0, &ctx).is_some());
        assert!(inject_for_read(&file, 0, &ctx).is_none());
    }

    #[test]
    fn reading_agents_md_does_not_inject_itself() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let (_temp, root) = tree();
        let ctx = ctx_for("t6-self", &root);
        let file = root.join("a/b/AGENTS.md");

        let injected = inject_for_read(&file, 0, &ctx).expect("parent injection");
        let self_header = format!("[Directory Context: {}]", file.display());
        let parent_header = format!("[Directory Context: {}]", root.join("a/AGENTS.md").display());
        assert!(!injected.contains(&self_header), "self header: {injected}");
        assert!(!injected.contains("level b rules"));
        assert!(injected.contains(&parent_header));
        assert!(injected.contains("level a rules"));
    }

    #[test]
    fn large_base_output_skips_injection() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let (_temp, root) = tree();
        let ctx = ctx_for("t7-large", &root);
        let file = root.join("a/b/file.txt");

        assert!(inject_for_read(&file, MAX_BASE_OUTPUT_BYTES, &ctx).is_none());
        assert!(inject_for_read(&file, MAX_BASE_OUTPUT_BYTES + 1, &ctx).is_none());
        assert!(inject_for_read(&file, MAX_BASE_OUTPUT_BYTES - 1, &ctx).is_some());
    }

    #[test]
    fn missing_working_dir_skips_injection() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let (_temp, root) = tree();
        let mut ctx = ctx_for("t8-no-working-dir", &root);
        ctx.working_dir = None;

        assert!(inject_for_read(&root.join("a/b/file.txt"), 0, &ctx).is_none());
    }

    #[test]
    fn disabled_flag_skips_injection() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let previous = std::env::var_os("HORON_AGENT_NESTED_AGENTS_MD");
        crate::env::set_var("HORON_AGENT_NESTED_AGENTS_MD", "0");
        crate::config::invalidate_config_cache();

        let (_temp, root) = tree();
        let ctx = ctx_for("t10-disabled", &root);
        let injected = inject_for_read(&root.join("a/b/file.txt"), 0, &ctx);

        match previous {
            Some(value) => crate::env::set_var("HORON_AGENT_NESTED_AGENTS_MD", value),
            None => crate::env::remove_var("HORON_AGENT_NESTED_AGENTS_MD"),
        }
        crate::config::invalidate_config_cache();

        assert!(injected.is_none(), "flag off must inject nothing: {injected:?}");
    }

    #[test]
    fn inserting_more_than_max_sessions_evicts_the_oldest() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let (_temp, root) = tree();
        let file = root.join("a/b/file.txt");

        for index in 0..=MAX_TRACKED_SESSIONS {
            let ctx = ctx_for(&format!("t9-session-{index}"), &root);
            assert!(inject_for_read(&file, 0, &ctx).is_some(), "session {index}");
        }

        {
            let guard = cache().lock().expect("cache lock");
            assert_eq!(guard.sessions.len(), MAX_TRACKED_SESSIONS);
            assert!(!guard.sessions.contains_key("t9-session-0"));
            assert!(guard.sessions.contains_key("t9-session-64"));
        }

        let survivor = ctx_for("t9-session-1", &root);
        assert!(inject_for_read(&file, 0, &survivor).is_none(), "survivor kept");

        let oldest = ctx_for("t9-session-0", &root);
        assert!(
            inject_for_read(&file, 0, &oldest).is_some(),
            "evicted session injects again"
        );
    }

    /// Four ancestors, each far larger than the per-file cap, so the read cap has
    /// to stop injection partway through the chain.
    fn large_tree(fill: &str) -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::TempDir::new().expect("temp dir");
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize root");
        let body = fill.repeat(MAX_BYTES_PER_FILE * 2);
        for level in ["a", "a/b", "a/b/c", "a/b/c/d"] {
            write(&root.join(level).join(AGENTS_MD), &body);
        }
        write(&root.join("a/b/c/d/file.txt"), "file body");
        (temp, root)
    }

    #[test]
    fn one_read_never_exceeds_max_bytes_per_read() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let (_temp, root) = large_tree("r");
        let ctx = ctx_for("t11-read-cap", &root);

        let injected = inject_for_read(&root.join("a/b/c/d/file.txt"), 0, &ctx).expect("injection");
        println!(
            "--- manual QA: injected bytes = {} (cap {MAX_BYTES_PER_READ}) ---",
            injected.len()
        );

        assert!(
            injected.len() <= MAX_BYTES_PER_READ,
            "one read injected {} bytes, cap is {MAX_BYTES_PER_READ}",
            injected.len()
        );
        assert!(
            injected.len() > MAX_BYTES_PER_READ - MAX_BYTES_PER_FILE,
            "the read cap must actually bind here: {} bytes",
            injected.len()
        );
    }

    #[test]
    fn one_file_never_exceeds_max_bytes_per_file() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let temp = tempfile::TempDir::new().expect("temp dir");
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize root");
        let agents = root.join("a").join(AGENTS_MD);
        write(&agents, &"b".repeat(MAX_BYTES_PER_FILE * 3));
        write(&root.join("a/file.txt"), "file body");
        let ctx = ctx_for("t12-file-cap", &root);

        let injected = inject_for_read(&root.join("a/file.txt"), 0, &ctx).expect("injection");
        let header = block_header(&agents);
        let note = truncated_note(&agents);
        let body = &injected[injected.find(&header).expect("header")..];
        let block = body
            .strip_suffix(&note)
            .expect("the oversized file must announce its truncation");
        println!(
            "--- manual QA: per-file block bytes = {} (cap {MAX_BYTES_PER_FILE}), header = {} ---",
            block.len(),
            header.len()
        );

        assert!(
            block.len() <= MAX_BYTES_PER_FILE,
            "one file injected {} bytes, cap is {MAX_BYTES_PER_FILE}",
            block.len()
        );
    }

    #[test]
    fn skipped_ancestors_are_named_when_the_read_budget_runs_out() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let (_temp, root) = large_tree("s");
        let ctx = ctx_for("t13-skip", &root);

        let injected = inject_for_read(&root.join("a/b/c/d/file.txt"), 0, &ctx).expect("injection");
        let marker_at = injected
            .find("[Note: 2 more ancestor AGENTS.md files skipped")
            .expect("the two ancestors that did not fit must be named");
        println!("--- manual QA: skip marker = {:?} ---", &injected[marker_at..]);

        let skipped_leaf = block_header(&root.join("a/b/c/d").join(AGENTS_MD));
        assert!(
            !injected.contains(&skipped_leaf),
            "a skipped ancestor must not leave a half block behind"
        );
        assert!(
            injected.len() <= MAX_BYTES_PER_READ,
            "the marker must not blow the read cap: {} bytes",
            injected.len()
        );
    }

    #[test]
    fn utf8_boundaries_survive_the_cap() {
        let _lock = cache_test_lock();
        let _env = crate::storage::lock_test_env();
        clear_session_cache_for_test();
        let temp = tempfile::TempDir::new().expect("temp dir");
        let root = std::fs::canonicalize(temp.path()).expect("canonicalize root");
        let agents = root.join("a").join(AGENTS_MD);
        write(&agents, &"가".repeat(MAX_BYTES_PER_FILE));
        write(&root.join("a/file.txt"), "file body");
        let ctx = ctx_for("t14-utf8-cap", &root);

        let injected = inject_for_read(&root.join("a/file.txt"), 0, &ctx).expect("injection");
        let header = block_header(&agents);
        let note = truncated_note(&agents);
        let block = &injected[injected.find(&header).expect("header")..];
        let content = block
            .strip_suffix(&note)
            .expect("the oversized file must announce its truncation");
        println!(
            "--- manual QA: content bytes = {}, last char = {:?}, block = {} ---",
            content.len() - header.len(),
            content.chars().next_back(),
            content.len()
        );

        assert!(
            content.ends_with('가'),
            "the cut must land on a character boundary, not mid-character"
        );
        assert_eq!(
            (content.len() - header.len()) % 3,
            0,
            "every kept character is three bytes, so no partial character survives"
        );
        assert!(
            content.len() <= MAX_BYTES_PER_FILE,
            "header plus content must fit the per-file cap: {}",
            content.len()
        );
        assert!(!injected.contains('\u{FFFD}'), "no replacement character");
        assert!(std::str::from_utf8(injected.as_bytes()).is_ok(), "valid utf-8");
    }
}
