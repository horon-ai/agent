//! Wire-only pruning of tool results that no longer earn their tokens.
//!
//! The plan is computed from the transcript and applied only to the provider
//! request copy: stored history always keeps the original tool output, so a
//! resumed session or a rewind can still read what was elided. Nothing here
//! mutates a message; callers rewrite their own copy using [`PRUNED_SUPERSEDED_TEXT`]
//! and [`PRUNED_USELESS_TEXT`].

use crate::message_token_count;
use chrono::{DateTime, Utc};
use horon_agent_message_types::{ContentBlock, Message, Role};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Why a tool result is replaced on the provider request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PruneKind {
    /// A later read of the same file made this one redundant.
    Superseded,
    /// The result told the agent nothing (no output, empty grep).
    Useless,
}

/// Replacement text for [`PruneKind::Superseded`] results.
pub const PRUNED_SUPERSEDED_TEXT: &str = "[Superseded by a newer read of this file]";
/// Replacement text for [`PruneKind::Useless`] results.
pub const PRUNED_USELESS_TEXT: &str = "[Uneventful result elided]";

/// Tokens allowed after a prune candidate before the prompt cache is assumed
/// to have been rewritten anyway.
pub const CACHE_GUARD_TOKENS: usize = 8_000;
/// Age at which the newest message makes the prompt cache cold regardless of
/// how many tokens follow a candidate.
pub const CACHE_GUARD_AGE_SECS: i64 = 30 * 60;
/// How many newer messages a useless result must have before it is pruned.
pub const USELESS_MIN_NEWER_MESSAGES: usize = 6;

/// Mirrors the bash tool's empty-success text
/// (`crates/app-core/src/tool/bash.rs`), which compaction-core cannot import.
const NO_OUTPUT_RESULT: &str = "Command completed successfully (no output)";

/// Shape of the `read` call that produced a result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ReadForm {
    /// Whole file, no paging.
    Whole,
    /// Any line selector or start/end/offset/limit paging.
    Ranged,
    /// `:raw` output: no line numbers, no line-length cut.
    Raw,
}

/// Identity of a read within the transcript: the same file read the same way.
type ReadKey = (String, ReadForm);

/// A non-error `read` result and what it covers.
#[derive(Debug)]
struct ReadObservation<'a> {
    index: usize,
    tool_use_id: &'a str,
    key: ReadKey,
    complete: bool,
}

/// Newly prunable tool-use ids, in transcript order.
///
/// `already` is the persisted ledger: an id in it is never planned again (the
/// caller still applies the ledger when rewriting, so this only reports new
/// work). Results are only returned for messages old enough that rewriting
/// their text does not throw away the prompt cache for the newest turn.
pub fn plan_prunes(
    messages: &[Message],
    already: &BTreeMap<String, PruneKind>,
    now: DateTime<Utc>,
) -> Vec<(String, PruneKind)> {
    if messages.is_empty() {
        return Vec::new();
    }

    let uses = tool_uses(messages);
    let reads = read_observations(messages, &uses);
    let mut newest_by_key: HashMap<&ReadKey, usize> = HashMap::new();
    let mut newest_complete_by_path: HashMap<&str, usize> = HashMap::new();
    for observation in &reads {
        let slot = newest_by_key.entry(&observation.key).or_insert(observation.index);
        *slot = (*slot).max(observation.index);
        if observation.complete {
            let slot = newest_complete_by_path
                .entry(observation.key.0.as_str())
                .or_insert(observation.index);
            *slot = (*slot).max(observation.index);
        }
    }

    let mut tokens_after = vec![0usize; messages.len() + 1];
    for index in (0..messages.len()).rev() {
        tokens_after[index] = tokens_after[index + 1].saturating_add(message_token_count(&messages[index]));
    }
    let aged = messages
        .iter()
        .rev()
        .find_map(|message| message.timestamp)
        .is_some_and(|timestamp| now.signed_duration_since(timestamp).num_seconds() >= CACHE_GUARD_AGE_SECS);

    let mut planned = Vec::new();
    let mut seen = HashSet::new();
    for (index, message) in messages.iter().enumerate() {
        for block in &message.content {
            let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            } = block
            else {
                continue;
            };
            if matches!(is_error, Some(true)) || already.contains_key(tool_use_id) || !seen.insert(tool_use_id.clone())
            {
                continue;
            }
            let Some((name, _)) = uses.get(tool_use_id.as_str()) else {
                continue;
            };
            let Some(kind) = prune_kind(
                index,
                messages.len(),
                name,
                tool_use_id,
                content,
                &reads,
                &newest_by_key,
                &newest_complete_by_path,
            ) else {
                continue;
            };
            if !aged && tokens_after[index + 1] > CACHE_GUARD_TOKENS {
                continue;
            }
            planned.push((tool_use_id.clone(), kind));
        }
    }
    planned
}

fn tool_uses(messages: &[Message]) -> HashMap<&str, (&str, &serde_json::Value)> {
    let mut uses = HashMap::new();
    for message in messages {
        for block in &message.content {
            if let ContentBlock::ToolUse { id, name, input, .. } = block {
                uses.insert(id.as_str(), (name.as_str(), input));
            }
        }
    }
    uses
}

fn read_observations<'a>(
    messages: &'a [Message],
    uses: &HashMap<&'a str, (&'a str, &'a serde_json::Value)>,
) -> Vec<ReadObservation<'a>> {
    let mut reads = Vec::new();
    for (index, message) in messages.iter().enumerate() {
        for block in &message.content {
            let ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            } = block
            else {
                continue;
            };
            if matches!(is_error, Some(true)) {
                continue;
            }
            let Some((name, input)) = uses.get(tool_use_id.as_str()) else {
                continue;
            };
            if *name != "read" {
                continue;
            }
            let key = read_key(input);
            let complete = key.1 == ReadForm::Whole && read_result_is_complete(content);
            reads.push(ReadObservation {
                index,
                tool_use_id: tool_use_id.as_str(),
                key,
                complete,
            });
        }
    }
    reads
}

#[allow(clippy::too_many_arguments)]
fn prune_kind(
    index: usize,
    message_count: usize,
    name: &str,
    tool_use_id: &str,
    content: &str,
    reads: &[ReadObservation<'_>],
    newest_by_key: &HashMap<&ReadKey, usize>,
    newest_complete_by_path: &HashMap<&str, usize>,
) -> Option<PruneKind> {
    if name == "read" {
        let observation = reads
            .iter()
            .find(|observation| observation.tool_use_id == tool_use_id)?;
        let newer_same_key = newest_by_key
            .get(&observation.key)
            .is_some_and(|latest| *latest > index);
        let newer_complete_same_path = observation.key.1 != ReadForm::Raw
            && newest_complete_by_path
                .get(observation.key.0.as_str())
                .is_some_and(|latest| *latest > index);
        if newer_same_key || newer_complete_same_path {
            return Some(PruneKind::Superseded);
        }
        return None;
    }
    if message_count.saturating_sub(index + 1) < USELESS_MIN_NEWER_MESSAGES {
        return None;
    }
    if content.trim() == NO_OUTPUT_RESULT || (name == "agentgrep" && is_agentgrep_zero_match(content)) {
        return Some(PruneKind::Useless);
    }
    None
}

/// File path and call form of a `read` tool input.
fn read_key(input: &serde_json::Value) -> ReadKey {
    let file_path = input
        .get("file_path")
        .or_else(|| input.get("path"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let (path, form) = split_selector(file_path);
    let paged = input.get("start_line").is_some_and(|value| !value.is_null())
        || input.get("end_line").is_some_and(|value| !value.is_null())
        || input.get("offset").is_some_and(|value| !value.is_null())
        || input.get("limit").is_some_and(|value| !value.is_null());
    let form = if paged && form == ReadForm::Whole {
        ReadForm::Ranged
    } else {
        form
    };
    (path.to_string(), form)
}

/// Split a `:selector` suffix off a path, mirroring `read`'s selector grammar.
///
/// Only the text after the last colon is inspected, so `f.rs:raw:50-100` keys
/// as path `f.rs:raw` with a range form. That is still a distinct key from the
/// plain read, which is all pruning needs.
fn split_selector(file_path: &str) -> (&str, ReadForm) {
    if file_path.contains("://") {
        return (file_path, ReadForm::Whole);
    }
    let Some(colon) = file_path.rfind(':') else {
        return (file_path, ReadForm::Whole);
    };
    let path = &file_path[..colon];
    let selector = &file_path[colon + 1..];
    if path.is_empty() {
        return (file_path, ReadForm::Whole);
    }
    match parse_selector(selector) {
        Some(form) => (path, form),
        None => (file_path, ReadForm::Whole),
    }
}

fn parse_selector(selector: &str) -> Option<ReadForm> {
    if selector == "raw" {
        return Some(ReadForm::Raw);
    }
    if selector.is_empty() {
        return None;
    }
    if selector.split(',').all(is_range_spec) {
        return Some(ReadForm::Ranged);
    }
    None
}

/// `N`, `N-M`, `N+K`, `N-`, or `-N` (all 1-based).
fn is_range_spec(spec: &str) -> bool {
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    if digits(spec) {
        return true;
    }
    if let Some(rest) = spec.strip_prefix('-') {
        return digits(rest);
    }
    if let Some((start, end)) = spec.split_once('-') {
        return digits(start) && (end.is_empty() || digits(end));
    }
    if let Some((start, count)) = spec.split_once('+') {
        return digits(start) && digits(count);
    }
    false
}

fn read_result_is_complete(content: &str) -> bool {
    !content.contains("more lines (use") && !content.contains("more lines in file. Use :")
}

/// Whether an `agentgrep` result reported no matches at all.
///
/// The rendering lives in the `agentgrep` crate; these are the three
/// zero-result shapes it produces.
fn is_agentgrep_zero_match(content: &str) -> bool {
    let lines: Vec<&str> = content.trim_end().lines().map(str::trim_end).collect();
    let two_line = |second: &str| lines.len() == 2 && lines[0].starts_with("query: ") && lines[1] == second;
    if two_line("matches: 0 in 0 files") || two_line("top files: 0") {
        return true;
    }
    lines.last() == Some(&"top results: 0 files, 0 regions")
}

/// Whether `message` opens a turn the provider can start its context on.
pub fn is_turn_start(message: &Message) -> bool {
    matches!(message.role, Role::User)
        && message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Text { .. }))
        && !message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(text: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.into(),
                cache_control: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    fn assistant_use(id: &str, name: &str, input: serde_json::Value) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: id.into(),
                name: name.into(),
                input,
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    fn tool_result(id: &str, content: &str) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: id.into(),
                content: content.into(),
                is_error: None,
                verdict: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    fn read_call(id: &str, path: &str) -> Message {
        assistant_use(id, "read", json!({ "file_path": path }))
    }

    #[test]
    fn older_complete_reads_are_superseded_by_the_newest_one() {
        let messages = vec![
            user("read a.rs"),
            read_call("r1", "a.rs"),
            tool_result("r1", "1\tfn a() {}\n"),
            read_call("r2", "a.rs"),
            tool_result("r2", "1\tfn a() {}\n"),
            read_call("r3", "a.rs"),
            tool_result("r3", "1\tfn a() {}\n"),
        ];
        let planned = plan_prunes(&messages, &BTreeMap::new(), Utc::now());
        assert_eq!(
            planned,
            vec![
                ("r1".to_string(), PruneKind::Superseded),
                ("r2".to_string(), PruneKind::Superseded)
            ]
        );
    }

    #[test]
    fn a_partial_read_is_superseded_by_a_later_complete_read_of_the_same_path() {
        let messages = vec![
            user("read a.rs"),
            read_call("r1", "a.rs:10-20"),
            tool_result("r1", "10\tfn a() {}\n"),
            read_call("r2", "a.rs"),
            tool_result("r2", "1\tfn a() {}\n"),
        ];
        let planned = plan_prunes(&messages, &BTreeMap::new(), Utc::now());
        assert_eq!(planned, vec![("r1".to_string(), PruneKind::Superseded)]);
    }

    #[test]
    fn a_raw_read_is_only_superseded_by_a_raw_read() {
        let messages = vec![
            user("read a.rs raw"),
            read_call("r1", "a.rs:raw"),
            tool_result("r1", "fn a() {}\n"),
            read_call("r2", "a.rs"),
            tool_result("r2", "1\tfn a() {}\n"),
        ];
        assert!(plan_prunes(&messages, &BTreeMap::new(), Utc::now()).is_empty());
    }

    #[test]
    fn ranged_reads_of_the_same_file_share_a_key_and_supersede_each_other() {
        let messages = vec![
            user("read twice"),
            read_call("r1", "a.rs:2-3"),
            tool_result("r1", "2\tb\n"),
            read_call("r2", "a.rs:8-9"),
            tool_result("r2", "8\th\n"),
        ];
        let planned = plan_prunes(&messages, &BTreeMap::new(), Utc::now());
        assert_eq!(planned, vec![("r1".to_string(), PruneKind::Superseded)]);
    }

    #[test]
    fn candidates_with_more_than_the_cache_guard_behind_them_are_held_back() {
        let mut messages = vec![
            user("read a.rs"),
            read_call("r1", "a.rs"),
            tool_result("r1", "1\tfn a() {}\n"),
            read_call("r2", "a.rs"),
            tool_result("r2", "1\tfn a() {}\n"),
        ];
        let filler = "x".repeat(40_000);
        for _ in 0..2 {
            messages.push(user(&filler));
        }
        assert!(plan_prunes(&messages, &BTreeMap::new(), Utc::now()).is_empty());

        let recent = Utc::now() - chrono::Duration::minutes(31);
        messages[4].timestamp = Some(recent);
        messages[3].timestamp = Some(recent);
        messages[0].timestamp = Some(recent);
        let planned = plan_prunes(&messages, &BTreeMap::new(), Utc::now());
        assert_eq!(planned, vec![("r1".to_string(), PruneKind::Superseded)]);
    }

    #[test]
    fn useless_results_need_six_newer_messages() {
        let mut messages = vec![
            user("run"),
            assistant_use("b1", "bash", json!({ "command": "true" })),
            tool_result("b1", NO_OUTPUT_RESULT),
        ];
        for index in 0..5 {
            messages.push(user(&format!("note {index}")));
        }
        assert!(plan_prunes(&messages, &BTreeMap::new(), Utc::now()).is_empty());
        messages.push(user("one more"));
        let planned = plan_prunes(&messages, &BTreeMap::new(), Utc::now());
        assert_eq!(planned, vec![("b1".to_string(), PruneKind::Useless)]);
    }

    #[test]
    fn empty_grep_results_are_useless_but_error_results_never_are() {
        let mut messages = vec![
            user("grep"),
            assistant_use("g1", "agentgrep", json!({ "query": "needle" })),
            tool_result("g1", "query: needle\nmatches: 0 in 0 files"),
            assistant_use("r1", "read", json!({ "file_path": "a.rs" })),
            tool_result("r1", "1\tfn a() {}\n"),
        ];
        for index in 0..6 {
            messages.push(user(&format!("note {index}")));
        }
        let planned = plan_prunes(&messages, &BTreeMap::new(), Utc::now());
        assert_eq!(planned, vec![("g1".to_string(), PruneKind::Useless)]);

        let mut errored = messages.clone();
        errored[2].content = vec![ContentBlock::ToolResult {
            tool_use_id: "g1".into(),
            content: "query: needle\nmatches: 0 in 0 files".into(),
            is_error: Some(true),
            verdict: None,
        }];
        assert!(plan_prunes(&errored, &BTreeMap::new(), Utc::now()).is_empty());
    }

    #[test]
    fn already_pruned_ids_are_not_planned_again() {
        let messages = vec![
            user("read a.rs"),
            read_call("r1", "a.rs"),
            tool_result("r1", "1\tfn a() {}\n"),
            read_call("r2", "a.rs"),
            tool_result("r2", "1\tfn a() {}\n"),
        ];
        let mut already = BTreeMap::new();
        already.insert("r1".to_string(), PruneKind::Superseded);
        assert!(plan_prunes(&messages, &already, Utc::now()).is_empty());
    }

    #[test]
    fn selector_suffixes_are_recognised_but_literal_colons_are_kept() {
        assert_eq!(split_selector("a.rs"), ("a.rs", ReadForm::Whole));
        assert_eq!(split_selector("a.rs:2-3"), ("a.rs", ReadForm::Ranged));
        assert_eq!(split_selector("a.rs:-2"), ("a.rs", ReadForm::Ranged));
        assert_eq!(split_selector("a.rs:5+10"), ("a.rs", ReadForm::Ranged));
        assert_eq!(split_selector("a.rs:7-"), ("a.rs", ReadForm::Ranged));
        assert_eq!(split_selector("a.rs:1,4-6"), ("a.rs", ReadForm::Ranged));
        assert_eq!(split_selector("a.rs:raw"), ("a.rs", ReadForm::Raw));
        assert_eq!(split_selector("a:1"), ("a", ReadForm::Ranged));
        assert_eq!(split_selector("agent://child"), ("agent://child", ReadForm::Whole));
    }

    #[test]
    fn error_reads_do_not_supersede_anything() {
        let messages = vec![
            user("read"),
            read_call("r1", "a.rs"),
            tool_result("r1", "1\tfn a() {}\n"),
            read_call("r2", "a.rs"),
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "r2".into(),
                    content: "not found".into(),
                    is_error: Some(true),
                    verdict: None,
                }],
                timestamp: None,
                tool_duration_ms: None,
            },
        ];
        assert!(plan_prunes(&messages, &BTreeMap::new(), Utc::now()).is_empty());
    }
}
