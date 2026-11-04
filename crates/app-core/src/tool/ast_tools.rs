//! Structural search and rewrite over tree-sitter grammars.
//!
//! Both tools are opt-in (`[tools] ast_tools = true`). `ast_grep` answers
//! "where does this shape occur" without a regex over text; `ast_edit` applies
//! an ast-grep pattern rewrite and refuses to write unless the caller pins the
//! replacement count it expects. A preview is the default so a mistaken pattern
//! costs nothing.

use super::{ConcurrencyClass, Tool, ToolContext, ToolEffects, ToolOutput};
use crate::bus::{Bus, BusEvent, FileOp, FileTouch};
use anyhow::Result;
use async_trait::async_trait;
use horon_agent_ast::{Lang, Match as AstMatch};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Files one `ast_grep` call may parse, so a search from `/` cannot spin.
const MAX_FILES: usize = 1000;
/// Matches one `ast_grep` call returns by default.
const DEFAULT_LIMIT: usize = 100;

/// `[tools] ast_tools`, off by default.
pub(crate) fn enabled() -> bool {
    crate::config::config().tools.ast_tools
}

fn resolve_lang(explicit: Option<&str>, path: &Path) -> Result<Lang> {
    match explicit {
        Some(name) => Lang::from_name(name).ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown language '{name}'; supported: {}",
                horon_agent_ast::supported_names().join(", ")
            )
        }),
        None => Lang::from_path(path)
            .ok_or_else(|| anyhow::anyhow!("Unsupported file type for structural search: {}", path.display())),
    }
}

pub struct AstGrepTool;

impl AstGrepTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct AstGrepInput {
    pattern: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

fn files_under(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .follow_links(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build();
    for entry in walker {
        if files.len() >= MAX_FILES {
            break;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if Lang::from_path(entry.path()).is_some() {
            files.push(entry.path().to_path_buf());
        }
    }
    files
}

#[async_trait]
impl Tool for AstGrepTool {
    fn name(&self) -> &str {
        "ast_grep"
    }

    fn description(&self) -> &str {
        "Search code by syntax shape with an ast-grep pattern (`foo($A, $B)`). Reports the path and line span of each match; `$A` captures are reported when named."
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::read()
    }

    fn concurrency_class(&self) -> ConcurrencyClass {
        ConcurrencyClass::Concurrent
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["pattern"],
            "properties": {
                "intent": super::intent_schema_property(),
                "pattern": {
                    "type": "string",
                    "description": "ast-grep pattern, for example `foo($A, $B)` or `if ($COND) { $$$BODY }`. `$NAME` captures one node, `$$$NAME` captures many."
                },
                "path": {
                    "type": "string",
                    "description": "File or directory to search. Defaults to the working directory."
                },
                "lang": {
                    "type": "string",
                    "description": "Language override: rust, typescript, tsx, javascript, python, go. Inferred from each file's extension when omitted."
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum matches to report. Default 100."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: AstGrepInput = serde_json::from_value(input)?;
        let explicit_lang = params.lang.as_deref();
        let limit = params.limit.unwrap_or(DEFAULT_LIMIT).max(1);
        let root = ctx.resolve_path(Path::new(params.path.as_deref().unwrap_or(".")));
        if !root.exists() {
            anyhow::bail!("Path not found: {}", root.display());
        }
        let explicit = explicit_lang
            .map(|name| resolve_lang(Some(name), Path::new("")))
            .transpose()?;

        let files = if root.is_file() {
            vec![root.clone()]
        } else {
            files_under(&root)
        };
        let scan_root = if root.is_file() {
            root.parent().unwrap_or(&root)
        } else {
            &root
        };
        let mut grouped: BTreeMap<String, Vec<AstMatch>> = BTreeMap::new();
        let mut total = 0usize;

        for file in files {
            if total >= limit {
                break;
            }
            let lang = match explicit {
                Some(lang) => lang,
                None => match Lang::from_path(&file) {
                    Some(lang) => lang,
                    None => continue,
                },
            };
            let Ok(source) = tokio::fs::read_to_string(&file).await else {
                continue;
            };
            let found = horon_agent_ast::find(&source, lang, &params.pattern)?;
            if found.is_empty() {
                continue;
            }
            let display = file
                .strip_prefix(scan_root)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/");
            for found in found {
                if total >= limit {
                    break;
                }
                total += 1;
                grouped.entry(display.clone()).or_default().push(found);
            }
        }

        if total == 0 {
            return Ok(ToolOutput::new(format!("No matches for `{}`.", params.pattern)));
        }

        let mut body = String::new();
        let mut first_file = true;
        for (path, matches) in &grouped {
            if !first_file {
                body.push('\n');
            }
            first_file = false;
            for found in matches {
                let first_line = found.text.lines().next().unwrap_or("").trim();
                body.push_str(&format!(
                    "{}:{}-{}: {}\n",
                    path, found.start_line, found.end_line, first_line
                ));
            }
        }
        if total >= limit {
            body.push_str(&format!("\n(match limit {limit} reached)"));
        }
        Ok(ToolOutput::new(body))
    }
}

pub struct AstEditTool;

impl AstEditTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Deserialize)]
struct AstEditInput {
    #[serde(default)]
    intent: Option<String>,
    ops: Vec<AstEditOp>,
    paths: Vec<String>,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    expected_replacements: Option<usize>,
}

#[derive(Deserialize)]
struct AstEditOp {
    pattern: String,
    rewrite: String,
}

struct FilePlan {
    path: PathBuf,
    display: String,
    original: String,
    rewritten: String,
    replacements: usize,
}

#[async_trait]
impl Tool for AstEditTool {
    fn name(&self) -> &str {
        "ast_edit"
    }

    fn description(&self) -> &str {
        "Rewrite code by ast-grep pattern and template. Previews a unified diff unless `expected_replacements` is given; a count mismatch writes nothing."
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::write()
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["ops", "paths"],
            "properties": {
                "intent": super::intent_schema_property(),
                "ops": {
                    "type": "array",
                    "description": "Pattern rewrites applied in order to every file.",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "required": ["pattern", "rewrite"],
                        "properties": {
                            "pattern": {"type": "string", "description": "ast-grep pattern to match."},
                            "rewrite": {"type": "string", "description": "Replacement template; `$NAME` inserts the captured node."}
                        }
                    }
                },
                "paths": {
                    "type": "array",
                    "description": "Files to rewrite. Directories are not walked; name the files.",
                    "items": {"type": "string"},
                    "minItems": 1
                },
                "lang": {
                    "type": "string",
                    "description": "Language override: rust, typescript, tsx, javascript, python, go. Inferred per file when omitted."
                },
                "expected_replacements": {
                    "type": "integer",
                    "description": "Total replacements the caller expects. Omit to preview without writing; a mismatch refuses the call."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: AstEditInput = serde_json::from_value(input)?;
        if params.ops.is_empty() {
            anyhow::bail!("`ops` must contain at least one pattern rewrite.");
        }
        if params.paths.is_empty() {
            anyhow::bail!("`paths` must name at least one file.");
        }
        let explicit = params
            .lang
            .as_deref()
            .map(|name| resolve_lang(Some(name), Path::new("")))
            .transpose()?;

        let mut plans: Vec<FilePlan> = Vec::new();
        for authored in &params.paths {
            let path = ctx.resolve_path(Path::new(authored));
            if !path.exists() {
                anyhow::bail!("File not found: {authored}");
            }
            if !path.is_file() {
                anyhow::bail!("Not a file: {authored}; name the files to rewrite.");
            }
            super::path_policy::enforce("write", &json!({ "file_path": path.to_string_lossy() }), &ctx)?;
            let lang = match explicit {
                Some(lang) => lang,
                None => resolve_lang(None, &path)?,
            };
            let original = tokio::fs::read_to_string(&path)
                .await
                .map_err(|error| anyhow::anyhow!("cannot read {authored} as UTF-8 text: {error}"))?;
            let mut rewritten = original.clone();
            let mut replacements = 0usize;
            for op in &params.ops {
                let (next, count) = horon_agent_ast::rewrite(&rewritten, lang, &op.pattern, &op.rewrite)?;
                rewritten = next;
                replacements += count;
            }
            plans.push(FilePlan {
                path,
                display: authored.clone(),
                original,
                rewritten,
                replacements,
            });
        }

        let total: usize = plans.iter().map(|plan| plan.replacements).sum();
        let changed: Vec<&FilePlan> = plans.iter().filter(|plan| plan.original != plan.rewritten).collect();

        let Some(expected) = params.expected_replacements else {
            let mut body =
                format!("Preview only — no files changed. Re-run with expected_replacements={total} to apply.\n");
            for plan in &changed {
                let diff = super::file_diff::unified(&plan.display, &plan.display, &plan.original, &plan.rewritten);
                body.push('\n');
                body.push_str(diff.trim_end());
                body.push('\n');
            }
            if changed.is_empty() {
                body.push_str("\nNo file content would change.\n");
            }
            return Ok(ToolOutput::new(body));
        };

        if expected != total {
            anyhow::bail!("Expected {expected} replacements but the files now produce {total}. Nothing written.");
        }
        if changed.is_empty() {
            return Ok(ToolOutput::new(format!(
                "No changes: the pattern rewrites reproduce the current content of {}.",
                params.paths.join(", ")
            )));
        }

        for plan in &changed {
            if let Some(refusal) = super::generated_guard::refusal(&plan.path, &plan.display) {
                anyhow::bail!(refusal);
            }
        }

        let mut pending = Vec::with_capacity(changed.len());
        for plan in &changed {
            let _lock = super::file_lock::lock(&plan.path).await;
            let before = super::undo::capture(&plan.path).await;
            tokio::fs::write(&plan.path, &plan.rewritten).await?;
            pending.push(super::undo::PendingFile {
                path: plan.path.clone(),
                before,
                after: super::undo::bytes(plan.rewritten.as_bytes()),
            });
        }
        super::undo::record(&ctx, pending);
        for plan in &changed {
            super::edit_stats::record(&ctx, &plan.original, &plan.rewritten, false).await;
            let diff = super::file_diff::unified(&plan.display, &plan.display, &plan.original, &plan.rewritten);
            Bus::global().publish(BusEvent::FileTouch(FileTouch {
                session_id: ctx.session_id.clone(),
                path: plan.path.clone(),
                op: FileOp::Edit,
                intent: params.intent.clone().filter(|value| !value.trim().is_empty()),
                summary: Some(format!(
                    "ast-grep rewrite: {} replacement{}",
                    plan.replacements,
                    if plan.replacements == 1 { "" } else { "s" }
                )),
                detail: super::hashline_edit::build_file_touch_preview(&diff),
            }));
        }

        let mut body = format!(
            "Edited {} file{}: {total} replacement{}\n",
            changed.len(),
            if changed.len() == 1 { "" } else { "s" },
            if total == 1 { "" } else { "s" }
        );
        for plan in &changed {
            let diff = super::file_diff::unified(&plan.display, &plan.display, &plan.original, &plan.rewritten);
            body.push_str(&format!("\n{}\n", diff.trim_end()));
        }
        Ok(ToolOutput::new(body).with_title(params.paths.join(", ")))
    }
}

#[cfg(test)]
#[path = "ast_tools_tests.rs"]
mod tests;
