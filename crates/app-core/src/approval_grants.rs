//! Scoped approval grants and always-ask paths.
//!
//! An approval answer can name a scope: "allow this file" or "allow this
//! folder". That answer is a grant, stored in `<agent home>/approvals.json` with
//! mode 0600 and an expiry, so the next call that names the same place runs
//! without asking. `[tools] restricted_paths` is the opposite: a set of globs
//! that asks in every mode, which no grant can cover.
//!
//! Three limits are deliberate:
//!
//! - A grant belongs to a workspace root. A grant written in one checkout
//!   covers nothing in another, even when both name the same absolute path.
//! - A grant covers only the effect it was issued for. "Allow this folder"
//!   answered for an append covers appends; a later write asks again.
//! - Paths are compared after canonicalization, so `..` and a symlink cannot
//!   walk out of the place the user allowed. A path that cannot be
//!   canonicalized gets no grant: it asks, which is the safe side of the
//!   question.

use crate::config::config;
use crate::safety::PermissionRequest;
use crate::tool::ToolEffects;
use crate::tool::path_policy::{named_path_texts, named_paths};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// Which effect a grant was issued for.
///
/// A write grant answers an append call, because replacing a file's contents is
/// a stronger decision than adding to them; the reverse is not true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum GrantCapability {
    Append,
    Write,
}

impl GrantCapability {
    /// The capability a call needs, or `None` when it mutates nothing and a
    /// grant would decide nothing.
    pub(crate) fn from_effects(effects: ToolEffects) -> Option<Self> {
        if effects.writes() {
            Some(Self::Write)
        } else if effects.appends() {
            Some(Self::Append)
        } else {
            None
        }
    }

    /// The label carried in the request context and in the grant file.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Append => "append",
            Self::Write => "write",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "append" => Some(Self::Append),
            "write" => Some(Self::Write),
            _ => None,
        }
    }

    /// Whether a grant of `self` answers a call that needs `needed`.
    fn covers(self, needed: Self) -> bool {
        match self {
            Self::Write => true,
            Self::Append => needed == Self::Append,
        }
    }
}

/// What a grant covers: one path, or everything under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum GrantScope {
    Path,
    Folder,
}

/// One stored answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Grant {
    /// The canonical workspace root the answer was given in.
    pub(crate) workspace: PathBuf,
    /// The canonical path the answer covers.
    pub(crate) path: PathBuf,
    pub(crate) scope: GrantScope,
    pub(crate) capability: GrantCapability,
    pub(crate) granted_at: DateTime<Utc>,
    pub(crate) expires_at: DateTime<Utc>,
}

impl Grant {
    fn covers_path(&self, target: &Path) -> bool {
        match self.scope {
            GrantScope::Path => target == self.path,
            GrantScope::Folder => target.starts_with(&self.path),
        }
    }
}

/// The on-disk file. `version` lets a later change read an older file instead of
/// throwing it away.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct GrantFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    grants: Vec<Grant>,
}

const GRANT_FILE_VERSION: u32 = 1;

static WARNED_MALFORMED: AtomicBool = AtomicBool::new(false);
static WARNED_RESTRICTED_PATTERN: AtomicBool = AtomicBool::new(false);

fn grant_path() -> anyhow::Result<PathBuf> {
    Ok(crate::storage::horon_agent_dir()?.join("approvals.json"))
}

fn grant_lock_path() -> anyhow::Result<PathBuf> {
    // The `approvals/` directory already holds the decision locks; the grant
    // lock lives beside them so one directory owns every approval file.
    Ok(crate::storage::horon_agent_dir()?
        .join("approvals")
        .join(".grants.lock"))
}

/// The stored grants.
///
/// A file that cannot be parsed is ignored with one warning per process rather
/// than deleted: the user's answers are theirs, and an unreadable file must ask
/// again instead of taking a grant nobody can see.
fn load_grants() -> Vec<Grant> {
    match grant_path() {
        Ok(path) => load_grants_from(&path),
        Err(_) => Vec::new(),
    }
}

fn load_grants_from(path: &Path) -> Vec<Grant> {
    if !path.exists() {
        return Vec::new();
    }
    match crate::storage::read_json::<GrantFile>(path) {
        Ok(file) => file.grants,
        Err(error) => {
            if !WARNED_MALFORMED.swap(true, Ordering::Relaxed) {
                crate::logging::warn(&format!(
                    "[approval-grants] ignoring unreadable {}: {error:#}; calls will ask again",
                    path.display()
                ));
            }
            Vec::new()
        }
    }
}

/// The canonical form of a path a grant could name.
///
/// The path itself is canonicalized when it exists. When it does not (a file
/// about to be created), the deepest existing ancestor is canonicalized and the
/// missing names are appended, so the answer covers the file the call will
/// create. A component that cannot be resolved yields `None`: such a path is
/// never grant-eligible and therefore asks.
pub(crate) fn canonical_target(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut existing: PathBuf = path
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect();
    if existing.as_os_str().is_empty() {
        return None;
    }
    let mut tail: Vec<OsString> = Vec::new();
    loop {
        if let Ok(resolved) = existing.canonicalize() {
            let mut resolved = resolved;
            for name in tail.iter().rev() {
                resolved.push(name);
            }
            return Some(resolved);
        }
        // `..` is resolved only by an existing directory, so an unresolved one
        // cannot be compared safely.
        if matches!(
            existing.components().next_back(),
            Some(Component::ParentDir | Component::RootDir | Component::Prefix(_))
        ) {
            return None;
        }
        tail.push(existing.file_name()?.to_os_string());
        existing.pop();
        if existing.as_os_str().is_empty() {
            return None;
        }
    }
}

/// The canonical workspace root a call runs in.
fn workspace_root(working_dir: Option<&Path>) -> Option<PathBuf> {
    canonical_target(working_dir?)
}

/// Whether an unexpired grant answers every path this call names.
///
/// Every named path must be covered: a call that writes two files, one allowed
/// and one not, still asks.
pub(crate) fn covered(tool: &str, input: &serde_json::Value, working_dir: Option<&Path>, effects: ToolEffects) -> bool {
    // A zero TTL turns grants off completely, including the read below.
    if config().tools.approval_grant_ttl_days == 0 {
        return false;
    }
    let Some(needed) = GrantCapability::from_effects(effects) else {
        return false;
    };
    let Some(workspace) = workspace_root(working_dir) else {
        return false;
    };
    let Some(targets) = canonical_targets(tool, input, working_dir) else {
        return false;
    };
    covered_by(&load_grants(), &workspace, &targets, needed, Utc::now())
}

fn covered_by(
    grants: &[Grant],
    workspace: &Path,
    targets: &[PathBuf],
    needed: GrantCapability,
    now: DateTime<Utc>,
) -> bool {
    if targets.is_empty() {
        return false;
    }
    targets.iter().all(|target| {
        grants.iter().any(|grant| {
            grant.workspace == workspace
                && grant.capability.covers(needed)
                && grant.covers_path(target)
                && now < grant.expires_at
        })
    })
}

/// The canonical targets of a call, or `None` when any of them cannot be
/// resolved.
///
/// One unresolvable path makes the whole call ineligible: a grant must never
/// answer a call that also reaches somewhere the comparison could not see.
fn canonical_targets(tool: &str, input: &serde_json::Value, working_dir: Option<&Path>) -> Option<Vec<PathBuf>> {
    named_paths(tool, input, working_dir)
        .iter()
        .map(|path| canonical_target(path))
        .collect()
}

/// Whether this call names exactly one filesystem path.
///
/// The ACP client offers "allow this file" and "allow this folder" only for this
/// shape, because a scoped answer has to cover every path the call touches and
/// the user was shown one question. The daemon repeats the check when it stores
/// the grant, so a client that offered the options anyway still cannot widen
/// what its answer covers.
pub fn names_single_path(tool: &str, input: &serde_json::Value) -> bool {
    named_path_texts(tool, input).len() == 1
}

/// The grant a scoped answer authorises, built from the request the user
/// answered.
///
/// Only a call that names exactly one path can be granted: "allow this folder"
/// on a patch that touches three files would silently decide two questions the
/// user was never shown. Such a call is approved once and asks next time.
pub(crate) fn grant_from_request(request: &PermissionRequest, scope: GrantScope) -> Option<Grant> {
    grant_from_request_at(request, scope, Utc::now(), config().tools.approval_grant_ttl_days)
}

fn grant_from_request_at(
    request: &PermissionRequest,
    scope: GrantScope,
    now: DateTime<Utc>,
    ttl_days: u64,
) -> Option<Grant> {
    if ttl_days == 0 {
        return None;
    }
    let context = request.context.as_ref()?;
    let capability = context
        .get("grant_capability")
        .and_then(serde_json::Value::as_str)
        .and_then(GrantCapability::parse)?;
    let input = context.get("input")?;
    let working_dir = context
        .get("working_dir")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from);
    let workspace = workspace_root(working_dir.as_deref())?;
    if named_path_texts(&request.action, input).len() != 1 {
        return None;
    }
    let target = canonical_target(&named_paths(&request.action, input, working_dir.as_deref())[0])?;
    // A folder grant covers the directory the file lives in, not the file: the
    // user answered "stop asking about this folder", and storing the file path
    // would make `starts_with` cover that one file and nothing beside it.
    let target = match scope {
        GrantScope::Path => target,
        GrantScope::Folder => target.parent()?.to_path_buf(),
    };
    Some(Grant {
        workspace,
        path: target,
        scope,
        capability,
        granted_at: now,
        expires_at: now + Duration::days(i64::try_from(ttl_days).unwrap_or(i64::MAX)),
    })
}

/// Store one grant, replacing an identical earlier one.
///
/// The read-modify-write runs under `<agent home>/approvals/.grants.lock`, the
/// directory the decision locks already live in, so two answers arriving at once
/// cannot drop one of them.
pub(crate) fn store(grant: Grant) -> anyhow::Result<()> {
    let _lock = acquire_grant_lock(100, std::time::Duration::from_millis(2))?;
    let now = Utc::now();
    let mut file = GrantFile {
        version: GRANT_FILE_VERSION,
        grants: load_grants(),
    };
    // Drop what has already expired so the file does not accumulate answers
    // nobody can use, then replace this exact answer.
    file.grants.retain(|existing| existing.expires_at > now);
    file.grants.retain(|existing| {
        !(existing.workspace == grant.workspace
            && existing.path == grant.path
            && existing.scope == grant.scope
            && existing.capability == grant.capability)
    });
    file.grants.push(grant);
    crate::storage::write_json_secret(&grant_path()?, &file)
}

/// Write the grant an answer of `scope` authorises, if the request supports one.
///
/// Returns whether a grant was stored. The call itself is approved either way;
/// a grant that cannot be written only means the next call asks again.
pub(crate) fn record_answer(request: &PermissionRequest, scope: GrantScope) -> bool {
    let Some(grant) = grant_from_request(request, scope) else {
        return false;
    };
    match store(grant) {
        Ok(()) => true,
        Err(error) => {
            crate::logging::warn(&format!("[approval-grants] could not store grant: {error:#}"));
            false
        }
    }
}

struct GrantLock {
    file: std::fs::File,
}

impl Drop for GrantLock {
    fn drop(&mut self) {
        if let Err(error) = self.file.unlock() {
            crate::logging::debug(&format!("[approval-grants] could not release the grant lock: {error}"));
        }
    }
}

fn acquire_grant_lock(attempts: u32, delay: std::time::Duration) -> anyhow::Result<GrantLock> {
    let path = grant_lock_path()?;
    if let Some(parent) = path.parent() {
        crate::storage::ensure_dir(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)?;
    let mut attempt = 0;
    loop {
        attempt += 1;
        match file.try_lock() {
            Ok(()) => return Ok(GrantLock { file }),
            Err(std::fs::TryLockError::WouldBlock) if attempt < attempts => std::thread::sleep(delay),
            Err(std::fs::TryLockError::WouldBlock) => {
                anyhow::bail!("the grant file stayed locked after {attempts} attempts; another writer is updating it")
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

// ---------------------------------------------------------------------------
// Restricted paths
// ---------------------------------------------------------------------------

/// The `[tools] restricted_paths` pattern that makes this call ask, if any.
///
/// Each named path is matched in the form the model wrote, its resolved form,
/// and its canonical form, so a pattern can name either the file as the user
/// thinks of it or the place it actually is. A pattern without a path separator
/// also matches the file name, so `secrets.env` covers `/srv/app/secrets.env`.
/// `bash` names no paths here: `[[tools.bash_patterns]]` is its equivalent.
pub(crate) fn restricted_match(tool: &str, input: &serde_json::Value, working_dir: Option<&Path>) -> Option<String> {
    // The default configuration has no list, so the common path allocates
    // nothing and touches no filesystem.
    let configured = &config().tools.restricted_paths;
    if configured.is_empty() {
        return None;
    }
    restricted_matched(configured, tool, input, working_dir)
}

fn restricted_matched(
    configured: &[String],
    tool: &str,
    input: &serde_json::Value,
    working_dir: Option<&Path>,
) -> Option<String> {
    let texts = named_path_texts(tool, input);
    if texts.is_empty() {
        return None;
    }
    let patterns = match compile_restricted(configured) {
        Ok(patterns) => patterns,
        // A pattern nobody can parse must ask about everything, not nothing:
        // the alternative is a typo silently switching the restriction off.
        Err(index) => return Some(configured[index].clone()),
    };
    let mut candidates: Vec<String> = texts.iter().map(|text| (*text).to_string()).collect();
    for path in named_paths(tool, input, working_dir) {
        candidates.push(path.to_string_lossy().into_owned());
        if let Some(target) = canonical_target(&path) {
            candidates.push(target.to_string_lossy().into_owned());
        }
    }
    patterns
        .iter()
        .enumerate()
        .find_map(|(index, pattern)| matches_any(pattern, &candidates).then(|| configured[index].clone()))
}

/// Whether `pattern` matches one of `candidates`.
///
/// A pattern with no path separator also matches a candidate's file name, so
/// `secrets.env` covers `/srv/app/secrets.env` the way a reader of the config
/// would expect. A pattern with a separator is matched against the whole path
/// only, so `notes/*` cannot reach a nested `notes/a/b.txt` by accident.
fn matches_any(pattern: &glob::Pattern, candidates: &[String]) -> bool {
    let names_only = !has_separator(pattern.as_str());
    candidates.iter().any(|candidate| {
        if pattern.matches(candidate) || pattern.matches_path(Path::new(candidate)) {
            return true;
        }
        names_only
            && Path::new(candidate)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| pattern.matches(name))
    })
}

fn has_separator(pattern: &str) -> bool {
    pattern.contains('/') || pattern.contains(std::path::MAIN_SEPARATOR)
}

/// Compile the configured patterns, reporting the first that cannot be parsed.
///
/// The caller turns an error into "restrict everything": an unreadable
/// restriction must never mean "no restriction".
fn compile_restricted(configured: &[String]) -> Result<Vec<glob::Pattern>, usize> {
    let mut patterns = Vec::with_capacity(configured.len());
    for (index, pattern) in configured.iter().enumerate() {
        match glob::Pattern::new(pattern) {
            Ok(compiled) => patterns.push(compiled),
            Err(error) => {
                if !WARNED_RESTRICTED_PATTERN.swap(true, Ordering::Relaxed) {
                    crate::logging::warn(&format!(
                        "[approval-grants] restricted path pattern {pattern:?} is invalid ({error}); \
                         every path will ask"
                    ));
                }
                return Err(index);
            }
        }
    }
    Ok(patterns)
}

#[cfg(test)]
#[path = "approval_grants_tests.rs"]
mod tests;
