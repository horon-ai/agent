//! The non-interactive environment every shell tool child receives.
//!
//! The fixed table lives here as constants so a tool call borrows it instead of
//! rebuilding it, and the two lookups that depend on the process (`PATH` for
//! `false`, the platform locale fixes) are resolved once and reused.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::sync::{Arc, LazyLock, Mutex};

/// Pager variables that become `cat`, so no command stops at a pager prompt.
const PAGERS: [&str; 11] = [
    "PAGER",
    "GIT_PAGER",
    "MANPAGER",
    "SYSTEMD_PAGER",
    "BAT_PAGER",
    "DELTA_PAGER",
    "GH_PAGER",
    "GLAB_PAGER",
    "PSQL_PAGER",
    "MYSQL_PAGER",
    "HOMEBREW_PAGER",
];

/// Editor variables that become `true`, so an editor invocation returns at once.
const EDITORS: [&str; 4] = ["GIT_EDITOR", "VISUAL", "EDITOR", "GIT_SEQUENCE_EDITOR"];

/// Package-manager and CLI switches that auto-confirm or silence prompts.
const PROMPT_FREE_TOOLS: [(&str, &str); 18] = [
    ("npm_config_yes", "true"),
    ("npm_config_update_notifier", "false"),
    ("npm_config_fund", "false"),
    ("npm_config_audit", "false"),
    ("npm_config_progress", "false"),
    ("PNPM_DISABLE_SELF_UPDATE_CHECK", "true"),
    ("PNPM_UPDATE_NOTIFIER", "false"),
    ("YARN_ENABLE_TELEMETRY", "0"),
    ("YARN_ENABLE_PROGRESS_BARS", "0"),
    ("CARGO_TERM_PROGRESS_WHEN", "never"),
    ("DEBIAN_FRONTEND", "noninteractive"),
    ("PIP_NO_INPUT", "1"),
    ("PIP_DISABLE_PIP_VERSION_CHECK", "1"),
    ("TF_INPUT", "0"),
    ("TF_IN_AUTOMATION", "1"),
    ("GH_PROMPT_DISABLED", "1"),
    ("COMPOSER_NO_INTERACTION", "1"),
    ("CLOUDSDK_CORE_DISABLE_PROMPTS", "1"),
];

/// Console code-page and locale fixes where the platform shell needs them.
///
/// Resolved once per process: on Windows the locale probe walks the whole
/// process environment, and nothing in this process sets `LANG` or `LC_*`
/// after start, so the answer cannot change between tool calls.
static PLATFORM_SHELL_ENV: LazyLock<Vec<(&'static str, String)>> =
    LazyLock::new(horon_agent_platform::term::shell_env_overrides);

/// Variables every child command sees so nothing can stop and wait for a
/// terminal answer that will never come: pagers and editors become no-ops,
/// Git and SSH cannot prompt, and package managers auto-confirm.
///
/// Order matters only in that later entries win; a caller that sets
/// `HORON_AGENT_SESSION_ID`/`HORON_AGENT_CWD_FILE` after these still keeps the
/// last word. Every value except `SSH_ASKPASS` is borrowed from a table that
/// lives for the process, so a tool call no longer rebuilds it.
pub(super) fn non_interactive_env(pty: bool) -> Vec<(&'static str, Cow<'static, str>)> {
    let platform: &'static [(&'static str, String)] = &PLATFORM_SHELL_ENV;
    let mut env = Vec::with_capacity(PAGERS.len() + EDITORS.len() + PROMPT_FREE_TOOLS.len() + platform.len() + 11);
    env.extend(PAGERS.iter().map(|&name| (name, Cow::Borrowed("cat"))));
    env.push(("AWS_PAGER", Cow::Borrowed("")));
    env.push(("LESS", Cow::Borrowed("FRX")));
    env.push(("TERM", Cow::Borrowed(if pty { "xterm-256color" } else { "dumb" })));
    if !pty {
        env.push(("NO_COLOR", Cow::Borrowed("1")));
    }
    env.push(("PYTHONUNBUFFERED", Cow::Borrowed("1")));
    env.extend(EDITORS.iter().map(|&name| (name, Cow::Borrowed("true"))));
    env.push(("GIT_TERMINAL_PROMPT", Cow::Borrowed("0")));
    env.push(("GCM_INTERACTIVE", Cow::Borrowed("never")));
    env.push(("SSH_ASKPASS", Cow::Owned(absolute_false_command().to_string())));
    if !pty && !ci_suppressed() {
        env.push(("CI", Cow::Borrowed("true")));
    }
    env.push(("AGENT", Cow::Borrowed("1")));
    env.extend(
        PROMPT_FREE_TOOLS
            .iter()
            .map(|&(name, value)| (name, Cow::Borrowed(value))),
    );
    env.extend(
        platform
            .iter()
            .map(|(name, value)| (*name, Cow::Borrowed(value.as_str()))),
    );
    env
}

/// The last `PATH` value `SSH_ASKPASS` was resolved against, with its result.
type FalseCommandCache = Mutex<Option<(Option<OsString>, Arc<str>)>>;

static FALSE_COMMAND: FalseCommandCache = Mutex::new(None);

/// The absolute path of the `false` program, so `SSH_ASKPASS` fails closed
/// instead of falling back to a GUI prompt.
///
/// The `PATH` walk runs once per distinct `PATH` value rather than once per
/// tool call; on Windows nothing provides `false`, so every call used to probe
/// every `PATH` directory.
fn absolute_false_command() -> Arc<str> {
    cached_false_command(&FALSE_COMMAND, std::env::var_os("PATH"))
}

fn cached_false_command(cache: &FalseCommandCache, path: Option<OsString>) -> Arc<str> {
    let mut cached = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((seen, value)) = cached.as_ref()
        && *seen == path
    {
        return Arc::clone(value);
    }
    let value: Arc<str> = resolve_false_command(path.as_deref()).into();
    *cached = Some((path, Arc::clone(&value)));
    value
}

/// Walk `path` for a `false` program. A bare `false` is the documented
/// fallback when nothing on `PATH` provides it.
fn resolve_false_command(path: Option<&OsStr>) -> String {
    let Some(path) = path else {
        return "false".to_string();
    };
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join("false");
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    "false".to_string()
}

/// `HORON_AGENT_BASH_NO_CI=1` lets a caller keep a `CI` value of its own (for a
/// command that behaves differently under CI) while the rest of the
/// non-interactive environment stays on.
fn ci_suppressed() -> bool {
    std::env::var("HORON_AGENT_BASH_NO_CI")
        .map(|value| {
            let value = value.trim();
            value == "1" || value.eq_ignore_ascii_case("true")
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn non_interactive_env_disables_pagers_and_prompts() {
        let env = super::non_interactive_env(false);
        let find = |name: &str| env.iter().find(|(key, _)| *key == name).map(|(_, value)| &**value);
        assert_eq!(find("PAGER"), Some("cat"));
        assert_eq!(find("HOMEBREW_PAGER"), Some("cat"));
        assert_eq!(find("AWS_PAGER"), Some(""));
        assert_eq!(find("LESS"), Some("FRX"));
        assert_eq!(find("TERM"), Some("dumb"));
        assert_eq!(find("NO_COLOR"), Some("1"));
        assert_eq!(find("GIT_EDITOR"), Some("true"));
        assert_eq!(find("EDITOR"), Some("true"));
        assert_eq!(find("GIT_TERMINAL_PROMPT"), Some("0"));
        assert_eq!(find("AGENT"), Some("1"));
        assert_eq!(find("npm_config_yes"), Some("true"));
        let askpass = find("SSH_ASKPASS").expect("SSH_ASKPASS is set");
        assert!(askpass == "false" || std::path::Path::new(askpass).is_absolute());

        // A terminal keeps colors, its own TERM, and no forced CI marker.
        let env = super::non_interactive_env(true);
        let find = |name: &str| env.iter().find(|(key, _)| *key == name).map(|(_, value)| &**value);
        assert_eq!(find("TERM"), Some("xterm-256color"));
        assert_eq!(find("NO_COLOR"), None);
        assert_eq!(find("CI"), None);
    }

    #[test]
    fn false_command_walks_path_once_per_distinct_value() {
        let with_false = tempfile::tempdir().expect("temp dir");
        let without_false = tempfile::tempdir().expect("temp dir");
        let program = with_false.path().join("false");
        std::fs::write(&program, b"").expect("write false");
        let cache = super::FalseCommandCache::new(None);

        // No PATH falls back to a bare `false`, exactly as before.
        assert_eq!(&*super::cached_false_command(&cache, None), "false");

        // A PATH without `false` keeps the bare fallback, and the second call
        // with the same value is served from the cache, not a new walk.
        let empty_path = std::env::join_paths([without_false.path()]).expect("join");
        let first = super::cached_false_command(&cache, Some(empty_path.clone()));
        let second = super::cached_false_command(&cache, Some(empty_path.clone()));
        assert_eq!(&*first, "false");
        assert!(std::sync::Arc::ptr_eq(&first, &second));

        // A changed PATH is resolved again and finds the new program.
        let found_path = std::env::join_paths([without_false.path(), with_false.path()]).expect("join");
        let found = super::cached_false_command(&cache, Some(found_path));
        assert_eq!(&*found, program.to_string_lossy());
        assert_eq!(super::resolve_false_command(Some(&empty_path)), "false");
    }

    #[test]
    fn non_interactive_env_keeps_the_same_child_environment() {
        // The table moved to constants; the child must still receive every
        // entry, in the same order, with the platform fixes last.
        let env = super::non_interactive_env(false);
        let names: Vec<&str> = env.iter().map(|(name, _)| *name).collect();
        let platform: Vec<&str> = horon_agent_platform::term::shell_env_overrides()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let mut expected = vec![
            "PAGER",
            "GIT_PAGER",
            "MANPAGER",
            "SYSTEMD_PAGER",
            "BAT_PAGER",
            "DELTA_PAGER",
            "GH_PAGER",
            "GLAB_PAGER",
            "PSQL_PAGER",
            "MYSQL_PAGER",
            "HOMEBREW_PAGER",
            "AWS_PAGER",
            "LESS",
            "TERM",
            "NO_COLOR",
            "PYTHONUNBUFFERED",
            "GIT_EDITOR",
            "VISUAL",
            "EDITOR",
            "GIT_SEQUENCE_EDITOR",
            "GIT_TERMINAL_PROMPT",
            "GCM_INTERACTIVE",
            "SSH_ASKPASS",
        ];
        if !super::ci_suppressed() {
            expected.push("CI");
        }
        expected.push("AGENT");
        expected.extend(super::PROMPT_FREE_TOOLS.iter().map(|(name, _)| *name));
        expected.extend(platform);
        assert_eq!(names, expected);
        assert_eq!(super::PROMPT_FREE_TOOLS[0], ("npm_config_yes", "true"));
        assert_eq!(super::PROMPT_FREE_TOOLS[17], ("CLOUDSDK_CORE_DISABLE_PROMPTS", "1"));
    }
}
