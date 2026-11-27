use super::{Tool, ToolContext, ToolOutput};
use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::cmp::Reverse;
use std::collections::HashSet;

include!(concat!(env!("OUT_DIR"), "/horon_agent_docs.rs"));

/// The address scheme `read` resolves against the bundled documents.
pub(crate) const SCHEME: &str = "horon://";

const DEFAULT_LIMIT: usize = 5;
const MAX_LIMIT: usize = 10;
const MAX_SECTION_CHARS: usize = 4_000;

pub struct HoronAgentDocsTool;

impl HoronAgentDocsTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct HoronAgentDocsInput {
    #[serde(default = "default_action")]
    action: String,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

fn default_action() -> String {
    "search".to_string()
}

#[derive(Debug)]
struct Section<'a> {
    path: &'a str,
    heading: String,
    body: String,
}

#[async_trait]
impl Tool for HoronAgentDocsTool {
    fn name(&self) -> &str {
        "horon"
    }

    fn description(&self) -> &str {
        "Search bundled Horon docs; read one as horon://<path>."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["search", "read", "list"],
                    "description": "Search documentation (default), read one document, or list bundled documents."
                },
                "query": {
                    "type": "string",
                    "description": "Words or question to search for. Required for search."
                },
                "path": {
                    "type": "string",
                    "description": "Bundled path or horon:// address from search/list. Required for read."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_LIMIT,
                    "description": "Maximum search results. Defaults to 5."
                }
            }
        })
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let params: HoronAgentDocsInput = serde_json::from_value(input)?;
        let output = match params.action.as_str() {
            "search" => search(
                params
                    .query
                    .as_deref()
                    .ok_or_else(|| anyhow!("query is required for search"))?,
                params.limit,
            ),
            "read" => {
                let path = params
                    .path
                    .as_deref()
                    .ok_or_else(|| anyhow!("path is required for read"))?;
                let reference = path
                    .get(..SCHEME.len())
                    .filter(|head| head.eq_ignore_ascii_case(SCHEME))
                    .map_or(path, |_| &path[SCHEME.len()..]);
                read_doc(reference)?
            }
            "list" => list_docs(),
            other => {
                return Err(anyhow!("unknown action {other:?}; use search, read, or list"));
            }
        };
        Ok(ToolOutput::new(output).with_title(format!("horon docs {}", params.action)))
    }
}

pub(crate) fn list_docs() -> String {
    let mut output = format!(
        "Bundled Horon documentation ({} files); read one with read {SCHEME}<path>:\n",
        HORON_AGENT_DOCS.len()
    );
    for (path, body) in HORON_AGENT_DOCS {
        let title = body.lines().find_map(|line| line.strip_prefix("# ")).unwrap_or(path);
        output.push_str(&format!("- {SCHEME}{}: {title}\n", address(path)));
    }
    output
}

/// Prefix the root `README.md` is addressed under, since `horon://README.md`
/// is the docs index (`docs/README.md`).
const PROJECT_PREFIX: &str = "project/";

/// The `horon://` address of a bundled path: `docs/` is the root of the
/// scheme, so `docs/runtime/TOOLS.md` is `runtime/TOOLS.md`, and the project's
/// own `README.md` is `project/README.md`.
fn address(path: &str) -> String {
    match path.strip_prefix("docs/") {
        Some(rest) => rest.to_string(),
        None => format!("{PROJECT_PREFIX}{path}"),
    }
}

/// The bundled document a `horon://` reference names, with its address, or
/// `None` for the bare scheme, which lists them.
///
/// `horon://runtime/TOOLS.md` names `docs/runtime/TOOLS.md`. `.md` may be left
/// out and letter case does not matter (`horon://runtime/tools`); a reference
/// that still spells `docs/` resolves too.
pub(crate) fn document(reference: &str) -> Result<Option<(String, &'static str)>> {
    let reference = reference.trim().trim_matches('/');
    if reference.is_empty() {
        return Ok(None);
    }
    let path = match reference.get(..PROJECT_PREFIX.len()) {
        Some(head) if head.eq_ignore_ascii_case(PROJECT_PREFIX) => reference[PROJECT_PREFIX.len()..].to_string(),
        _ if reference
            .get(..5)
            .is_some_and(|head| head.eq_ignore_ascii_case("docs/")) =>
        {
            reference.to_string()
        }
        _ => format!("docs/{reference}"),
    };
    let candidates = [path.clone(), format!("{path}.md")];
    let found = candidates.iter().find_map(|candidate| {
        HORON_AGENT_DOCS
            .iter()
            .find(|(path, _)| *path == candidate.as_str())
            .or_else(|| {
                HORON_AGENT_DOCS
                    .iter()
                    .find(|(path, _)| path.eq_ignore_ascii_case(candidate))
            })
    });
    match found {
        Some((path, body)) => Ok(Some((address(path), body))),
        None => Err(anyhow!(
            "{SCHEME}{reference} names no bundled document. Read {SCHEME} to list them, or search with the horon tool."
        )),
    }
}

fn read_doc(reference: &str) -> Result<String> {
    let (path, body) =
        document(reference)?.ok_or_else(|| anyhow!("path is required for read; action=list names the documents"))?;
    Ok(format!(
        "Source: {SCHEME}{path} (bundled with this Horon build)\n\n{body}"
    ))
}

fn search(query: &str, limit: Option<usize>) -> String {
    let terms = terms(query);
    if terms.is_empty() {
        return "Search query must contain at least one word.".to_string();
    }
    let mut matches = sections()
        .into_iter()
        .filter_map(|section| {
            let heading = section.heading.to_lowercase();
            let body = section.body.to_lowercase();
            let path = section.path.to_lowercase();
            let matched = terms
                .iter()
                .filter(|term| heading.contains(*term) || body.contains(*term) || path.contains(*term))
                .count();
            if matched == 0 {
                return None;
            }
            let occurrences = terms
                .iter()
                .map(|term| body.matches(term.as_str()).count().min(10))
                .sum::<usize>();
            let score = matched * 100
                + terms.iter().filter(|term| heading.contains(*term)).count() * 40
                + terms.iter().filter(|term| path.contains(*term)).count() * 20
                + occurrences;
            Some((score, section))
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|(score, section)| (Reverse(*score), section.path, section.heading.clone()));
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut output = format!("Horon docs results for {query:?} (bundled with this Horon build):\n");
    for (index, (_, section)) in matches.into_iter().take(limit).enumerate() {
        let excerpt = relevant_excerpt(&section.body, &terms);
        output.push_str(&format!(
            "\n{}. {SCHEME}{} > {}\n{}\n",
            index + 1,
            address(section.path),
            section.heading,
            excerpt
        ));
    }
    if output.lines().count() == 1 {
        output.push_str("\nNo matching documentation. Try fewer or broader terms.\n");
    }
    output
}

fn terms(query: &str) -> Vec<String> {
    let stop: HashSet<&str> = [
        "a",
        "an",
        "and",
        "about",
        "does",
        "for",
        "how",
        "i",
        "in",
        "is",
        "horon-agent",
        "of",
        "on",
        "the",
        "to",
        "what",
        "with",
    ]
    .into_iter()
    .collect();
    query
        .split(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
        .map(str::to_lowercase)
        .filter(|term| term.len() > 1 && !stop.contains(term.as_str()))
        .collect()
}

fn sections() -> Vec<Section<'static>> {
    let mut result = Vec::new();
    for (path, document) in HORON_AGENT_DOCS {
        let mut heading = "Overview".to_string();
        let mut body = String::new();
        for line in document.lines() {
            if line.starts_with('#') {
                if !body.trim().is_empty() {
                    result.push(Section {
                        path,
                        heading,
                        body: std::mem::take(&mut body),
                    });
                }
                heading = line.trim_start_matches('#').trim().to_string();
            } else {
                body.push_str(line);
                body.push('\n');
            }
        }
        if !body.trim().is_empty() {
            result.push(Section { path, heading, body });
        }
    }
    result
}

fn relevant_excerpt(body: &str, terms: &[String]) -> String {
    let paragraphs = body
        .split("\n\n")
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>();
    let best = paragraphs
        .iter()
        .max_by_key(|part| {
            let lower = part.to_lowercase();
            terms.iter().filter(|term| lower.contains(*term)).count()
        })
        .copied()
        .unwrap_or(body)
        .trim();
    if best.chars().count() <= MAX_SECTION_CHARS {
        best.to_string()
    } else {
        format!("{}…", best.chars().take(MAX_SECTION_CHARS).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_includes_current_docs_but_not_plans() {
        assert!(HORON_AGENT_DOCS.iter().any(|(path, _)| *path == "README.md"));
        assert!(HORON_AGENT_DOCS.iter().any(|(path, _)| *path == "docs/README.md"));
        assert!(!HORON_AGENT_DOCS.iter().any(|(path, _)| path.starts_with("docs/plans/")));
    }

    #[test]
    fn search_finds_relevant_version_matched_documentation() {
        let output = search("How does swarm task graph work?", Some(3));
        assert!(output.contains("horon://runtime/SWARM_TASK_GRAPH.md"), "{output}");
        assert!(output.contains("bundled with this Horon build"));
    }

    #[test]
    fn exact_document_can_be_read() {
        let output = read_doc("docs/README.md").unwrap();
        assert!(output.contains("# horon agent documentation"));
    }

    /// `read horon://<path>` returns a bundled document whole, a selector pages
    /// it, the bare scheme lists the documents, and an unknown path says how to
    /// find a real one.
    #[tokio::test]
    async fn read_resolves_horon_addresses() {
        use crate::tool::{Tool, ToolContext, ToolExecutionMode};
        let workspace = tempfile::tempdir().expect("workspace");
        let read = |path: &str| {
            let input = json!({ "file_path": path });
            let context = ToolContext {
                session_id: "horon-url-session".to_string(),
                message_id: "horon-url-message".to_string(),
                tool_call_id: "horon-url-call".to_string(),
                working_dir: Some(workspace.path().to_path_buf()),
                stdin_request_tx: None,
                graceful_shutdown_signal: None,
                execution_mode: ToolExecutionMode::Direct,
            };
            async move { crate::tool::read::ReadTool::new().execute(input, context).await }
        };

        // `docs/` is the root of the scheme.
        let whole = read("horon://README.md").await.expect("bundled doc").output;
        assert!(
            whole.starts_with(
                "Source: horon://README.md (bundled with this Horon build)\n\n# horon agent documentation"
            ),
            "{whole}"
        );
        let tools = read("horon://runtime/TOOLS.md").await.expect("doc under docs/").output;
        assert!(tools.starts_with("Source: horon://runtime/TOOLS.md "), "{tools}");
        // `.md`, letter case and a spelled-out `docs/` are forgiven.
        for loose in ["horon://runtime/tools", "horon://docs/runtime/TOOLS.md"] {
            let output = read(loose).await.expect("loose address").output;
            assert!(
                output.starts_with("Source: horon://runtime/TOOLS.md "),
                "{loose}: {output}"
            );
        }
        // The project's own README sits under `project/`.
        let project = read("horon://project/README.md").await.expect("root README").output;
        assert!(project.starts_with("Source: horon://project/README.md "), "{project}");
        let first_line = read("horon://README.md:1").await.expect("selector").output;
        assert!(first_line.contains("# horon agent documentation"), "{first_line}");
        assert!(
            !first_line.contains("\n## "),
            "a selector reads only its lines: {first_line}"
        );

        let list = read("horon://").await.expect("list").output;
        assert!(list.contains("- horon://runtime/TOOLS.md: "), "{list}");
        assert!(list.contains("- horon://project/README.md: "), "{list}");

        let missing = read("horon://nope").await.expect_err("unknown doc").to_string();
        assert!(missing.contains("horon://nope names no bundled document"), "{missing}");
    }
}
