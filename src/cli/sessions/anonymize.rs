//! `sessions anonymize`: a structure-preserving export for diagnostics.
//!
//! The export keeps what a bug report needs, the order of messages, their roles,
//! the kinds and sizes of their content blocks, which tool result answers which
//! tool call, and which session is a child of which. Every identifier is
//! replaced by a sequence number, so the same store always gives the same
//! export. Titles, paths, system prompts, tool inputs and results, reasoning and
//! attachment bytes are always removed. Message text, tool names and timestamps
//! are removed unless the caller keeps them, and kept text only has recognizable
//! secrets and known paths masked: free text cannot be de-identified reliably,
//! so keep it only for data you may share.
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::Path;

use crate::message::ContentBlock;
use crate::session::event_store_api as store;

pub(super) struct Options {
    pub with_children: bool,
    pub keep_text: bool,
    pub keep_tool_names: bool,
    pub keep_timestamps: bool,
}

struct Exported {
    id: store::SessionId,
    parent: Option<String>,
    created_at: String,
    working_dir: Option<String>,
    messages: Vec<crate::session::StoredMessage>,
}

fn load(paths: &store::SessionPaths, id: &store::SessionId) -> Result<Exported> {
    let graph = store::EventGraph::open(paths, id)?;
    let view = match graph.replay(None) {
        Ok(view) => view,
        Err(error) if error.code == store::StoreErrorCode::ConflictHeads => {
            graph.replay(Some(&graph.shared_head()?))?
        }
        Err(error) => return Err(error.into()),
    };
    let metadata = view.metadata();
    Ok(Exported {
        id: id.clone(),
        parent: metadata.parent_id.clone(),
        created_at: metadata.created_at.to_string(),
        working_dir: view.metadata().working_dir.clone(),
        messages: view.messages().iter().map(|row| row.message.clone()).collect(),
    })
}

/// The root session and, when asked, every descendant, in a stable order.
fn collect(paths: &store::SessionPaths, root: &store::SessionId, with_children: bool) -> Result<Vec<Exported>> {
    let mut found = vec![load(paths, root)?];
    if !with_children {
        return Ok(found);
    }
    let mut children: BTreeMap<String, Vec<Exported>> = BTreeMap::new();
    // List the store by reading its directory: the catalog helpers refresh a
    // derived cache on disk, and this command must not write to the store.
    for entry in std::fs::read_dir(paths.root())? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(id) = store::SessionId::parse(&name) else {
            continue;
        };
        if id == *root || !entry.file_type()?.is_dir() {
            continue;
        }
        // A session whose log cannot be replayed is not part of any tree here.
        if let Ok(session) = load(paths, &id)
            && let Some(parent) = session.parent.clone()
        {
            children.entry(parent).or_default().push(session);
        }
    }
    let mut seen = BTreeSet::from([root.as_str().to_string()]);
    let mut queue = VecDeque::from([root.as_str().to_string()]);
    while let Some(parent) = queue.pop_front() {
        let mut direct = children.remove(&parent).unwrap_or_default();
        direct.sort_by(|a, b| (&a.created_at, a.id.as_str()).cmp(&(&b.created_at, b.id.as_str())));
        for child in direct {
            if seen.insert(child.id.as_str().to_string()) {
                queue.push_back(child.id.as_str().to_string());
                found.push(child);
            }
        }
    }
    Ok(found)
}

struct Scrub {
    needles: Vec<String>,
}

impl Scrub {
    fn text(&self, text: &str) -> String {
        let mut masked = crate::message::redact_secrets(text);
        for needle in &self.needles {
            masked = masked.replace(needle.as_str(), "<path>");
        }
        masked
    }
}

fn block(value: &Value, options: &Options, scrub: &Scrub, tools: &mut BTreeMap<String, String>) -> Value {
    let kind = value.get("type").and_then(Value::as_str).unwrap_or("unknown");
    let mut tool_id = |raw: Option<&str>| {
        let next = tools.len() + 1;
        raw.map(|raw| {
            tools
                .entry(raw.to_string())
                .or_insert_with(|| format!("t{next}"))
                .clone()
        })
    };
    let size = |field: &str| value.get(field).map_or(0, |inner| inner.to_string().len());
    match kind {
        "text" => {
            let text = value.get("text").and_then(Value::as_str).unwrap_or_default();
            let mut out = json!({"type": "text", "chars": text.chars().count()});
            if options.keep_text {
                out["text"] = json!(scrub.text(text));
            }
            out
        }
        "tool_use" => json!({
            "type": "tool_use",
            "id": tool_id(value.get("id").and_then(Value::as_str)),
            "name": if options.keep_tool_names { value.get("name").cloned().unwrap_or(Value::Null) } else { json!("tool") },
            "input_bytes": size("input"),
        }),
        "tool_result" => json!({
            "type": "tool_result",
            "tool_use_id": tool_id(value.get("tool_use_id").and_then(Value::as_str)),
            "is_error": value.get("is_error").cloned().unwrap_or(Value::Null),
            "content_bytes": size("content"),
        }),
        // Reasoning and every attachment: only the kind and the size survive.
        other => json!({"type": other, "bytes": value.to_string().len()}),
    }
}

fn anonymize(sessions: &[Exported], options: &Options) -> Result<Value> {
    let names: BTreeMap<&str, String> = sessions
        .iter()
        .enumerate()
        .map(|(index, session)| (session.id.as_str(), format!("s{}", index + 1)))
        .collect();
    let mut needles: Vec<String> = sessions
        .iter()
        .filter_map(|session| session.working_dir.clone())
        .collect();
    needles.extend(dirs::home_dir().map(|home| home.display().to_string()));
    needles.retain(|needle| needle.len() > 1);
    needles.sort_by_key(|needle| std::cmp::Reverse(needle.len()));
    let scrub = Scrub { needles };

    let mut tools = BTreeMap::new();
    let mut out = Vec::new();
    for (index, session) in sessions.iter().enumerate() {
        let name = format!("s{}", index + 1);
        let mut messages = Vec::new();
        for (position, message) in session.messages.iter().enumerate() {
            let blocks: Vec<Value> = message
                .content
                .iter()
                .map(|content: &ContentBlock| {
                    block(
                        &serde_json::to_value(content).unwrap_or(Value::Null),
                        options,
                        &scrub,
                        &mut tools,
                    )
                })
                .collect();
            let mut row = json!({
                "id": format!("{name}.m{}", position + 1),
                "role": serde_json::to_value(&message.role)?,
                "blocks": blocks,
            });
            if options.keep_timestamps {
                row["timestamp"] = json!(message.timestamp.map(|stamp| stamp.to_string()));
            }
            messages.push(row);
        }
        let parent = match session.parent.as_deref() {
            None => Value::Null,
            Some(parent) => json!(names.get(parent).cloned().unwrap_or_else(|| "external".into())),
        };
        let mut row = json!({"id": name, "parent": parent, "message_count": messages.len(), "messages": messages});
        if options.keep_timestamps {
            row["created_at"] = json!(session.created_at);
        }
        out.push(row);
    }
    Ok(json!({"sessions": out}))
}

fn classes(options: &Options) -> (Vec<&'static str>, Vec<&'static str>) {
    let mut removed = vec![
        "session and message identifiers (replaced by sequence numbers)",
        "titles",
        "working directories and other paths",
        "system prompts",
        "provider session identifiers",
        "tool inputs and tool results",
        "reasoning",
        "attachment bytes (images, audio, documents)",
    ];
    let mut kept = vec![
        "message order and roles",
        "content block kinds and sizes",
        "which tool result answers which tool call",
        "parent and child session links",
    ];
    for (keep, removed_label, kept_label) in [
        (
            options.keep_text,
            "message text",
            "message text (secrets and known paths masked)",
        ),
        (options.keep_tool_names, "tool names", "tool names"),
        (options.keep_timestamps, "timestamps", "timestamps"),
    ] {
        if keep {
            kept.push(kept_label);
        } else {
            removed.push(removed_label);
        }
    }
    (removed, kept)
}

pub(super) fn run(
    paths: &store::SessionPaths,
    root: &store::SessionId,
    output: Option<&Path>,
    options: &Options,
    json_output: bool,
) -> Result<()> {
    if let Some(destination) = output
        && destination.symlink_metadata().is_ok()
    {
        bail!(
            "output path already exists; refusing to overwrite {}",
            destination.display()
        );
    }
    let sessions = collect(paths, root, options.with_children)?;
    let (removed, kept) = classes(options);
    let messages: usize = sessions.iter().map(|session| session.messages.len()).sum();
    let mut report = json!({
        "kind": "horon-agent-anonymized-session",
        "version": 1,
        "sessions": sessions.len(),
        "messages": messages,
        "removed": removed,
        "kept": kept,
        "written": false,
    });
    if let Some(destination) = output {
        let body = anonymize(&sessions, options)?;
        let mut document = report.clone();
        document["sessions"] = body["sessions"].clone();
        super::create_private_dir(destination)?;
        let file = destination.join("anonymized.json");
        super::write_file(&file, &serde_json::to_vec_pretty(&document)?)?;
        report["written"] = json!(file.display().to_string());
    }
    if json_output {
        return super::emit(true, &report);
    }
    println!("sessions: {}, messages: {}", sessions.len(), messages);
    println!("removed:");
    for class in &removed {
        println!("  - {class}");
    }
    println!("kept:");
    for class in &kept {
        println!("  - {class}");
    }
    match report["written"].as_str() {
        Some(file) => println!("wrote {file}"),
        None => println!("preview only; add --output <new directory> to write the export"),
    }
    if options.keep_text {
        println!("note: free text cannot be reliably de-identified; share it only if you may share the original");
    }
    Ok(())
}
