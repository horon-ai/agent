//! Intercepts shell idioms another tool handles better.
//!
//! `cat`/`head`/`tail`, `grep`/`rg`, `find`/`fd`, in-place `sed`/`perl`/`awk`,
//! shell redirection, `nohup`, and a trailing `&` all work as shell commands,
//! but each has a dedicated tool that fails loudly, is reviewable, and does not
//! depend on quoting. The interceptor names that tool so a model can switch on
//! the next call.
//!
//! Two properties matter and are easy to get wrong:
//!
//! 1. A rule never fires when its target tool is not reachable under the active
//!    allow-list. A `core`-profile session has no `agentgrep`-adjacent gaps, but
//!    a `none` profile has no tools at all, and nudging toward an uncallable
//!    tool is worse than silence.
//! 2. Candidates exclude a segment whose stdin is a pipe. `ls | cat` is a
//!    pager-shaped use of `cat` that `read` cannot replace, and blocking it
//!    would teach the model nothing.

use std::collections::HashSet;
use std::sync::LazyLock;

/// What one matching rule wants the call to use instead.
pub(crate) struct Interception {
    /// The note the caller shows (or the reason it refuses).
    pub(crate) message: &'static str,
}

/// One compiled rule: `pattern`, the tool it names, and its note.
pub(crate) struct Rule {
    pattern: fancy_regex::Regex,
    pub(crate) tool: &'static str,
    pub(crate) message: &'static str,
}

/// `(pattern, target tool, note)`, in evaluation order.
const RAW_RULES: &[(&str, &str, &str)] = &[
    (
        r"^\s*(?:(?:head|tail|less|more)\s+|cat\s+(?!\s|(?:[02-9]>{1,2}\|?\s*[^\s>|]\S*\s+)*(?:<<|1?>(?!&)|&>)))",
        "read",
        "Use the `read` tool instead of cat/head/tail; it supports line selectors like path:10-40 and path:-50.",
    ),
    (
        r"^\s*(?:grep|rg|ripgrep|ag|ack)\s+",
        "agentgrep",
        "Use `agentgrep` instead of grep/rg.",
    ),
    (
        r"^\s*(?:find|fd|locate)\s+.*(?:-name|-iname|-type|--type|-glob)",
        "agentgrep",
        "Use `agentgrep` with mode=find, or `ls`, instead of find/fd.",
    ),
    (
        r"^\s*(?:sed\s+(?:-i|--in-place)|perl\s+.*-[pn]?i|awk\s+.*-i\s+inplace)",
        "edit",
        "Use `edit` instead of in-place sed/perl/awk.",
    ),
    (
        r"^\s*(?:(?:echo|printf)\s.*(?<![0-9&>])>{1,2}\s*[^\s&>|]|cat\s*(?:<<|>))",
        "write",
        "Use `write` to create or replace files instead of shell redirection.",
    ),
    (
        r"^\s*nohup\s+|(?<!&)&\s*$",
        "bash",
        "Use `bash` with `name` (and optionally `ready`) instead of nohup or a trailing &.",
    ),
];

fn compiled_rules() -> &'static [Rule] {
    static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
        RAW_RULES
            .iter()
            .filter_map(|(pattern, tool, message)| {
                fancy_regex::Regex::new(pattern)
                    .ok()
                    .map(|pattern| Rule { pattern, tool, message })
            })
            .collect()
    });
    &RULES
}

/// The first rule that applies to `command`, using the active config.
pub(crate) fn intercept(command: &str, has_name: bool) -> Option<Interception> {
    let allowed = crate::config::config().tools.allowed_tools();
    let rule = match_rule(command, has_name, allowed.as_ref())?;
    Some(Interception { message: rule.message })
}

/// The first rule that applies, or `None`.
///
/// `allowed` is the active allow-list; `None` means every tool is reachable.
pub(crate) fn match_rule(command: &str, has_name: bool, allowed: Option<&HashSet<String>>) -> Option<&'static Rule> {
    let (whole, segments) = candidates(command);
    // The whole command is tested first across every rule. A tokenized segment
    // loses operators (`cat <<EOF > f` becomes `cat EOF f`), so a rule must not
    // see a rewrite of the line before the line itself has had its say.
    first_match(compiled_rules(), &whole, has_name, allowed)
        .or_else(|| first_match(compiled_rules(), &segments, has_name, allowed))
}

fn first_match<'a>(
    rules: &'a [Rule],
    candidates: &[String],
    has_name: bool,
    allowed: Option<&HashSet<String>>,
) -> Option<&'a Rule> {
    rules.iter().find(|rule| {
        // A named background service is exactly what the `nohup`/`&` rule asks
        // for, so a command that already carries a name is not nudged.
        !(rule.tool == "bash" && has_name)
            && tool_allowed(allowed, rule.tool)
            && candidates
                .iter()
                .any(|candidate| rule.pattern.is_match(candidate).unwrap_or(false))
    })
}

fn tool_allowed(allowed: Option<&HashSet<String>>, tool: &str) -> bool {
    match allowed {
        None => true,
        Some(allowed) => allowed.contains(tool) || allowed.contains("*") || allowed.contains("all"),
    }
}

/// The whole command, and the segments that start a command of their own.
fn candidates(command: &str) -> (Vec<String>, Vec<String>) {
    let mut whole = Vec::new();
    push_candidate(&mut whole, command);
    let mut segments = Vec::new();
    for segment in horon_agent_command_risk::split_segments(command) {
        if segment.iter().any(|token| token.receives_pipe) {
            continue;
        }
        let text = segment
            .iter()
            .map(|token| token.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        push_candidate(&mut segments, &text);
    }
    (whole, segments)
}

fn push_candidate(candidates: &mut Vec<String>, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    push_unique(candidates, &normalized);
    if let Some(stripped) = strip_env_assignments(&normalized) {
        push_unique(candidates, stripped);
    }
}

fn push_unique(candidates: &mut Vec<String>, text: &str) {
    if !candidates.iter().any(|candidate| candidate == text) {
        candidates.push(text.to_string());
    }
}

/// Drop leading `NAME=value` words, as the shell does for a command prefix.
fn strip_env_assignments(text: &str) -> Option<&str> {
    let mut rest = text;
    loop {
        let (word, tail) = match rest.find(' ') {
            Some(index) => (&rest[..index], &rest[index + 1..]),
            None => (rest, ""),
        };
        if !is_env_assignment(word) {
            break;
        }
        if tail.is_empty() {
            return None;
        }
        rest = tail;
    }
    (rest != text).then_some(rest)
}

fn is_env_assignment(word: &str) -> bool {
    let Some((name, _value)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_')
        && name.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

#[cfg(all(test, not(windows)))]
#[path = "bash_interceptor_tests.rs"]
mod tests;
