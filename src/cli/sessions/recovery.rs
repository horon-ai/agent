//! `session recovery`: a read-only preview of what restoring one session would
//! touch.
//!
//! The preview reads the stored originals and nothing else. It never starts a
//! daemon, never opens a runtime journal and never writes, so it is safe to run
//! on a session another process is using. Resuming stays with the existing
//! `--resume` entry point; this command only says which work is eligible for it
//! and which work must wait for a human.
//!
//! One session, and the child sessions it owns (`metadata.parent_id`), fall into
//! one of four states:
//!
//! - `empty`: no messages, nothing to restore.
//! - `complete`: the last message is the assistant's and no tool call is open.
//! - `awaiting_reply`: the last message is the user's (or a tool result), so the
//!   turn stopped before the model answered. Nothing was started that the
//!   transcript does not show, so resuming it is eligible.
//! - `unresolved_tool_calls`: an assistant tool call has no result. Whether it
//!   ran is unknown and it may have side effects, so it is parked: never offered
//!   for an automatic resume, named for a decision.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::Path;

use crate::message::{ContentBlock, Role};
use crate::session::event_store_api as store;

/// Upper bound on stored sessions read while looking for children.
const CHILD_SCAN_LIMIT: usize = 5000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RecoveryState {
    Empty,
    Complete,
    AwaitingReply,
    UnresolvedToolCalls(Vec<(String, String)>),
}

impl RecoveryState {
    fn name(&self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Complete => "complete",
            Self::AwaitingReply => "awaiting_reply",
            Self::UnresolvedToolCalls(_) => "unresolved_tool_calls",
        }
    }

    fn eligible(&self) -> bool {
        matches!(self, Self::AwaitingReply)
    }

    fn parked(&self) -> &[(String, String)] {
        match self {
            Self::UnresolvedToolCalls(calls) => calls,
            _ => &[],
        }
    }
}

/// Classify a transcript. Each item is one message's role and content blocks, in
/// order. Tool results count wherever they appear, so a call answered in a later
/// message is resolved.
pub(super) fn classify<'a>(messages: impl Iterator<Item = (&'a Role, &'a [ContentBlock])>) -> RecoveryState {
    let mut calls: Vec<(String, String)> = Vec::new();
    let mut answered: HashSet<String> = HashSet::new();
    let mut last_is_user = None;
    for (role, content) in messages {
        last_is_user = Some(matches!(role, Role::User));
        for block in content {
            match block {
                ContentBlock::ToolUse { id, name, .. } => calls.push((id.clone(), name.clone())),
                ContentBlock::ToolResult { tool_use_id, .. } => {
                    answered.insert(tool_use_id.clone());
                }
                _ => {}
            }
        }
    }
    let unresolved: Vec<(String, String)> = calls.into_iter().filter(|(id, _)| !answered.contains(id)).collect();
    match last_is_user {
        None => RecoveryState::Empty,
        Some(_) if !unresolved.is_empty() => RecoveryState::UnresolvedToolCalls(unresolved),
        Some(true) => RecoveryState::AwaitingReply,
        Some(false) => RecoveryState::Complete,
    }
}

struct Read {
    parent_id: Option<String>,
    title: Option<String>,
    state: RecoveryState,
}

fn read_session(paths: &store::SessionPaths, id: &store::SessionId) -> Result<Read> {
    let graph = store::EventGraph::open(paths, id)?;
    let view = match graph.replay(None) {
        Ok(view) => view,
        Err(error) if error.code == store::StoreErrorCode::ConflictHeads => {
            graph.replay(Some(&graph.shared_head()?))?
        }
        Err(error) => return Err(error.into()),
    };
    let state = classify(
        view.messages()
            .iter()
            .map(|row| (&row.message.role, row.message.content.as_slice())),
    );
    let metadata = view.metadata();
    Ok(Read {
        parent_id: metadata.parent_id.clone(),
        title: metadata.custom_title.clone().or_else(|| metadata.title.clone()),
        state,
    })
}

fn state_json(session_id: &str, title: Option<&str>, state: &RecoveryState) -> Value {
    let parked: Vec<Value> = state
        .parked()
        .iter()
        .map(|(id, name)| json!({"tool_call_id": id, "tool": name}))
        .collect();
    json!({
        "session_id": session_id,
        "title": title,
        "state": state.name(),
        "eligible": state.eligible(),
        "parked_tool_calls": parked,
    })
}

/// Build the preview for `id` and the children it owns.
pub(super) fn build_report(paths: &store::SessionPaths, id: &store::SessionId) -> Result<Value> {
    let Some(layout) = store::reader::layout(paths, id)? else {
        bail!(
            "session {} has no original under {}",
            id.as_str(),
            paths.root().display()
        );
    };
    if layout != store::reader::SourceLayout::Canonical {
        bail!(
            "session {} is not in the current layout; run `session migrate` before previewing its recovery",
            id.as_str()
        );
    }
    let target = read_session(paths, id).with_context(|| format!("could not read session {}", id.as_str()))?;

    let mut children = Vec::new();
    let mut unreadable = Vec::new();
    let mut scanned = 0usize;
    let mut truncated = false;
    for source in store::reader::sources(paths)? {
        if source.layout != store::reader::SourceLayout::Canonical || source.session_id == *id {
            continue;
        }
        if scanned >= CHILD_SCAN_LIMIT {
            truncated = true;
            break;
        }
        scanned += 1;
        match read_session(paths, &source.session_id) {
            Ok(read) if read.parent_id.as_deref() == Some(id.as_str()) => {
                children.push(state_json(
                    source.session_id.as_str(),
                    read.title.as_deref(),
                    &read.state,
                ));
            }
            Ok(_) => {}
            Err(error) => {
                unreadable.push(json!({"session_id": source.session_id.as_str(), "error": error.to_string()}))
            }
        }
    }
    children.sort_by(|a, b| a["session_id"].as_str().cmp(&b["session_id"].as_str()));

    let mut report = state_json(id.as_str(), target.title.as_deref(), &target.state);
    let count = |key: &str, want: bool| children.iter().filter(|child| child[key] == json!(want)).count();
    let parked_children = children
        .iter()
        .filter(|child| {
            child["parked_tool_calls"]
                .as_array()
                .is_some_and(|calls| !calls.is_empty())
        })
        .count();
    report["children"] = Value::Array(children.clone());
    report["unreadable_sessions"] = json!(unreadable);
    report["child_scan_truncated"] = json!(truncated);
    report["summary"] = json!({
        "eligible_sessions": count("eligible", true) + usize::from(target.state.eligible()),
        "parked_sessions": parked_children + usize::from(!target.state.parked().is_empty()),
        "children": children.len(),
    });
    report["resume_command"] = json!(format!("horon-agent --resume {}", id.as_str()));
    Ok(report)
}

pub(super) fn run(home: &Path, session: &str, json_output: bool) -> Result<()> {
    let id = super::parse_id(session)?;
    let report = build_report(&super::store_paths(home), &id)?;
    if json_output {
        return super::emit(true, &report);
    }
    print_report(&report);
    Ok(())
}

fn print_report(report: &Value) {
    let line = |item: &Value| {
        let title = match item["title"].as_str() {
            Some(title) => format!(" ({title})"),
            None => String::new(),
        };
        format!(
            "{}{}: {}{}",
            item["session_id"].as_str().unwrap_or(""),
            title,
            item["state"].as_str().unwrap_or(""),
            if item["eligible"] == json!(true) {
                ", eligible to resume"
            } else {
                ""
            }
        )
    };
    println!("session {}", line(report));
    let parked = |item: &Value| {
        for call in item["parked_tool_calls"].as_array().into_iter().flatten() {
            println!(
                "  parked: tool call {} ({}) has no result and may have side effects; decide before resuming",
                call["tool_call_id"].as_str().unwrap_or(""),
                call["tool"].as_str().unwrap_or("")
            );
        }
    };
    parked(report);
    let children: &[Value] = report["children"].as_array().map_or(&[], Vec::as_slice);
    if children.is_empty() {
        println!("children: none");
    } else {
        println!("children:");
        for child in children {
            println!("  {}", line(child));
            parked(child);
        }
    }
    if report["child_scan_truncated"] == json!(true) {
        println!("note: child scan stopped after {CHILD_SCAN_LIMIT} stored sessions");
    }
    println!(
        "eligible: {}, parked: {}",
        report["summary"]["eligible_sessions"], report["summary"]["parked_sessions"]
    );
    println!(
        "This preview changed nothing. To resume: {}",
        report["resume_command"].as_str().unwrap_or("")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(text: &str) -> ContentBlock {
        ContentBlock::Text {
            text: text.to_owned(),
            cache_control: None,
        }
    }

    fn call(id: &str, name: &str) -> ContentBlock {
        ContentBlock::ToolUse {
            id: id.to_owned(),
            name: name.to_owned(),
            input: json!({}),
            thought_signature: None,
        }
    }

    fn result(id: &str) -> ContentBlock {
        ContentBlock::ToolResult {
            tool_use_id: id.to_owned(),
            content: "ok".to_owned(),
            is_error: None,
            verdict: None,
        }
    }

    fn run(rows: &[(Role, Vec<ContentBlock>)]) -> RecoveryState {
        classify(rows.iter().map(|(role, content)| (role, content.as_slice())))
    }

    #[test]
    fn an_unanswered_user_message_is_eligible() {
        let state = run(&[(Role::User, vec![text("hi")])]);
        assert_eq!(state, RecoveryState::AwaitingReply);
        assert!(state.eligible());
    }

    #[test]
    fn a_finished_exchange_has_nothing_to_restore() {
        let state = run(&[(Role::User, vec![text("hi")]), (Role::Assistant, vec![text("hello")])]);
        assert_eq!(state, RecoveryState::Complete);
        assert!(!state.eligible());
    }

    #[test]
    fn a_tool_call_without_a_result_is_parked_not_eligible() {
        let state = run(&[
            (Role::User, vec![text("run it")]),
            (Role::Assistant, vec![call("c1", "bash")]),
        ]);
        assert_eq!(
            state,
            RecoveryState::UnresolvedToolCalls(vec![("c1".to_owned(), "bash".to_owned())])
        );
        assert!(!state.eligible());
        assert_eq!(state.parked().len(), 1);
    }

    #[test]
    fn a_tool_call_answered_later_is_resolved_and_the_turn_awaits_the_model() {
        let state = run(&[
            (Role::User, vec![text("run it")]),
            (Role::Assistant, vec![call("c1", "bash")]),
            (Role::User, vec![result("c1")]),
        ]);
        assert_eq!(state, RecoveryState::AwaitingReply);
    }

    #[test]
    fn an_empty_transcript_is_empty() {
        assert_eq!(run(&[]), RecoveryState::Empty);
    }

    struct Home {
        previous: Option<std::ffi::OsString>,
        _dir: tempfile::TempDir,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Home {
        fn new() -> Self {
            let lock = crate::storage::lock_test_env();
            let dir = tempfile::tempdir().expect("temp home");
            let previous = std::env::var_os("HORON_HOME");
            crate::env::set_var("HORON_HOME", dir.path());
            crate::config::Config::invalidate_cache();
            Self {
                previous,
                _dir: dir,
                _lock: lock,
            }
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(previous) => crate::env::set_var("HORON_HOME", previous),
                None => crate::env::remove_var("HORON_HOME"),
            }
            crate::config::Config::invalidate_cache();
        }
    }

    fn store_session(id: &str, parent: Option<&str>, rows: Vec<(Role, Vec<ContentBlock>)>) -> Result<()> {
        let mut session = crate::session::Session::create_with_id(id.to_owned(), parent.map(str::to_owned), None);
        for (role, content) in rows {
            session.add_message(role, content);
        }
        session.save()?;
        Ok(())
    }

    fn snapshot(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
        let mut files = std::collections::BTreeMap::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read dir").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    files.insert(path.clone(), std::fs::read(&path).expect("read file"));
                }
            }
        }
        files
    }

    #[test]
    fn the_preview_offers_only_interrupted_children_parks_tool_calls_and_changes_nothing() -> Result<()> {
        let home = Home::new();
        let root = "session_recovery_parent";
        store_session(
            root,
            None,
            vec![(Role::User, vec![text("plan")]), (Role::Assistant, vec![text("ok")])],
        )?;
        store_session(
            "session_recovery_waiting",
            Some(root),
            vec![(Role::User, vec![text("go")])],
        )?;
        store_session(
            "session_recovery_done",
            Some(root),
            vec![(Role::User, vec![text("go")]), (Role::Assistant, vec![text("done")])],
        )?;
        store_session(
            "session_recovery_parked",
            Some(root),
            vec![
                (Role::User, vec![text("go")]),
                (Role::Assistant, vec![call("c9", "bash")]),
            ],
        )?;
        store_session(
            "session_recovery_stranger",
            None,
            vec![(Role::User, vec![text("unrelated")])],
        )?;

        let store_root = home._dir.path().canonicalize()?;
        let before = snapshot(&store_root);
        let paths = super::super::store_paths(&store_root);
        let id = store::SessionId::parse(root).map_err(|error| anyhow::anyhow!("{}", error.message))?;
        let report = build_report(&paths, &id)?;
        let after = snapshot(&store_root);
        assert_eq!(before, after, "the preview must not write anything");

        assert_eq!(report["state"], "complete");
        let children = report["children"].as_array().expect("children");
        let by_id = |id: &str| {
            children
                .iter()
                .find(|child| child["session_id"] == id)
                .expect("child present")
        };
        assert_eq!(children.len(), 3, "the unrelated session is not a child");
        assert_eq!(by_id("session_recovery_waiting")["eligible"], true);
        assert_eq!(by_id("session_recovery_done")["eligible"], false);
        let parked = by_id("session_recovery_parked");
        assert_eq!(parked["eligible"], false);
        assert_eq!(parked["parked_tool_calls"][0]["tool_call_id"], "c9");
        assert_eq!(report["summary"]["eligible_sessions"], 1);
        assert_eq!(report["summary"]["parked_sessions"], 1);
        Ok(())
    }
}
