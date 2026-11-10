//! `lsp`: ask the project's language server what the compiler knows.
//!
//! Everything here is a thin formatting layer over [`horon_agent_lsp`]: the
//! tool resolves the file and the workspace root, decides the 1-based position,
//! and renders the answer as text. Diagnostics need a server that is already
//! indexing; a definition request waits for it instead of failing fast.

#[path = "lsp_edit.rs"]
mod lsp_edit;

use super::{Tool, ToolContext, ToolEffects, ToolOutput};
use anyhow::{Result, anyhow, bail};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

const DEFAULT_TIMEOUT_SECS: f64 = 20.0;
const MIN_TIMEOUT_SECS: f64 = 5.0;
const MAX_TIMEOUT_SECS: f64 = 300.0;
/// Longest source line echoed with a definition or reference.
const MAX_LINE_CHARS: usize = 200;

pub(crate) fn enabled() -> bool {
    crate::config::config().tools.lsp
}

pub struct LspTool;

impl LspTool {
    pub fn new() -> Self {
        Self
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Action {
    Diagnostics,
    Definition,
    References,
    Hover,
    Symbols,
    Status,
    Rename,
    #[serde(rename = "code_actions")]
    CodeActions,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Self::Diagnostics => "diagnostics",
            Self::Definition => "definition",
            Self::References => "references",
            Self::Hover => "hover",
            Self::Symbols => "symbols",
            Self::Status => "status",
            Self::Rename => "rename",
            Self::CodeActions => "code_actions",
        }
    }
}

#[derive(Deserialize)]
struct LspInput {
    action: Action,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    line: Option<u32>,
    #[serde(default)]
    character: Option<u32>,
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    new_name: Option<String>,
    #[serde(default)]
    apply: bool,
    #[serde(default)]
    action_id: Option<String>,
    #[serde(default)]
    end_line: Option<u32>,
    #[serde(default)]
    end_character: Option<u32>,
    #[serde(default)]
    timeout_secs: Option<f64>,
}

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str {
        "lsp"
    }

    fn description(&self) -> &str {
        "Ask the project's language server for diagnostics, definitions, references, hover text, symbols, a rename, or code actions."
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::read().union(ToolEffects::process())
    }

    fn effects_for_input(&self, input: &Value) -> ToolEffects {
        let applying = match input.get("action").and_then(Value::as_str) {
            Some("rename") => input.get("apply").and_then(Value::as_bool).unwrap_or(false),
            Some("code_actions") => input
                .get("action_id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.trim().is_empty()),
            _ => false,
        };
        if applying {
            self.effects().union(ToolEffects::write())
        } else {
            self.effects()
        }
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": [
                        "diagnostics",
                        "definition",
                        "references",
                        "hover",
                        "symbols",
                        "status",
                        "rename",
                        "code_actions"
                    ],
                    "description": "What to ask the language server. `status` lists running servers and needs no file."
                },
                "file": {
                    "type": "string",
                    "description": "File path, resolved against the working directory. Required for every action except status."
                },
                "line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based line number. Required for definition, references, and hover."
                },
                "character": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "1-based column. When omitted, the first occurrence of `symbol` on the line is used."
                },
                "symbol": {
                    "type": "string",
                    "description": "Symbol to locate on `line` when `character` is omitted."
                },
                "query": {
                    "type": "string",
                    "description": "Workspace symbol query. With `symbols`, a query searches the workspace instead of outlining one file."
                },
                "new_name": {
                    "type": "string",
                    "description": "New name for `rename`."
                },
                "apply": {
                    "type": "boolean",
                    "description": "With `rename`, apply the server's edit. Default false, which only lists the planned edits."
                },
                "action_id": {
                    "type": "string",
                    "description": "With `code_actions`, the id of the action to apply. Omit to list the actions offered at the position."
                },
                "end_line": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Last line of a `code_actions` range. Default: the start line."
                },
                "end_character": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Last column of a `code_actions` range. Default: the start column."
                },
                "timeout_secs": {
                    "type": "number",
                    "minimum": 5,
                    "maximum": 300,
                    "description": "Seconds to wait for the server. Default 20."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        super::path_policy::enforce("lsp", &input, &ctx)?;
        let params: LspInput = serde_json::from_value(input)?;
        let base = working_dir(&ctx);

        if params.action == Action::Status {
            return Ok(ToolOutput::new(status_output(&base).await));
        }
        // Refused before a server is started, so a missing argument costs no
        // process and no indexing.
        if params.action == Action::Rename
            && params
                .new_name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .is_none()
        {
            bail!("new_name is required for action 'rename'.");
        }

        let file = params
            .file
            .as_deref()
            .ok_or_else(|| anyhow!("file is required for action '{}'.", params.action.as_str()))?;
        let path = ctx.resolve_path(Path::new(file));
        if !path.exists() {
            bail!("File not found: {file}");
        }
        let path = std::fs::canonicalize(&path).unwrap_or(path);

        let spec = horon_agent_lsp::server_for_path(&path).ok_or_else(|| {
            anyhow!(
                "No language server is configured for {file}; supported: Rust (.rs), TypeScript/JavaScript (.ts, .tsx, .js, .jsx, .mjs, .cjs), Go (.go), and Python (.py)."
            )
        })?;
        if !horon_agent_lsp::is_installed(spec) {
            bail!(
                "{} is not installed or not on PATH; install it or use bash to run the compiler.",
                spec.program
            );
        }

        let timeout = effective_timeout(params.timeout_secs);
        let root = horon_agent_lsp::workspace_root(&path, spec, &base);
        let client = horon_agent_lsp::Client::get(spec, root).await?;

        match params.action {
            Action::Diagnostics => {
                let diagnostics = client.diagnostics(&path, timeout).await?;
                if diagnostics.is_empty() {
                    return Ok(ToolOutput::new("No diagnostics."));
                }
                let mut lines: Vec<String> = diagnostics
                    .into_iter()
                    .map(|diagnostic| {
                        format!(
                            "{}:{}:{}: {}: {}",
                            display_path(&diagnostic.path, &base),
                            diagnostic.line,
                            diagnostic.column,
                            diagnostic.severity.as_str(),
                            one_line(&diagnostic.message),
                        )
                    })
                    .collect();
                lines.sort();
                lines.dedup();
                Ok(ToolOutput::new(lines.join("\n")))
            }
            Action::Definition => {
                let (line, column) = resolve_position(&path, params.line, params.character, params.symbol.as_deref())?;
                let locations = client.definition(&path, line, column, timeout).await?;
                if locations.is_empty() {
                    return Ok(ToolOutput::new("No definition found."));
                }
                Ok(ToolOutput::new(render_locations(&locations, &base)))
            }
            Action::References => {
                let (line, column) = resolve_position(&path, params.line, params.character, params.symbol.as_deref())?;
                let locations = client.references(&path, line, column, timeout).await?;
                if locations.is_empty() {
                    return Ok(ToolOutput::new("No references found."));
                }
                Ok(ToolOutput::new(render_locations(&locations, &base)))
            }
            Action::Hover => {
                let (line, column) = resolve_position(&path, params.line, params.character, params.symbol.as_deref())?;
                let hover = client.hover(&path, line, column, timeout).await?;
                match hover {
                    Some(text) => Ok(ToolOutput::new(format!(
                        "{}:{line}:{column}\n{text}",
                        display_path(&path, &base)
                    ))),
                    None => Ok(ToolOutput::new("No hover information.")),
                }
            }
            Action::Symbols => match params.query.as_deref().filter(|query| !query.trim().is_empty()) {
                Some(query) => {
                    let symbols = client.workspace_symbols(query, timeout).await?;
                    if symbols.is_empty() {
                        return Ok(ToolOutput::new(format!("No symbols matching '{query}'.")));
                    }
                    let mut lines: Vec<String> = symbols
                        .iter()
                        .map(|symbol| {
                            let location = match &symbol.path {
                                Some(path) => display_path(path, &base),
                                None => String::new(),
                            };
                            format!(
                                "{location}:{}:{}: {} ({})",
                                symbol.line, symbol.column, symbol.name, symbol.kind
                            )
                        })
                        .collect();
                    lines.sort();
                    Ok(ToolOutput::new(lines.join("\n")))
                }
                None => {
                    let symbols = client.document_symbols(&path, timeout).await?;
                    if symbols.is_empty() {
                        return Ok(ToolOutput::new("No symbols found."));
                    }
                    let mut output = String::new();
                    render_symbols(&symbols, 0, &mut output);
                    Ok(ToolOutput::new(output))
                }
            },
            Action::Rename => {
                let (line, column) = resolve_position(&path, params.line, params.character, params.symbol.as_deref())?;
                let new_name = params
                    .new_name
                    .as_deref()
                    .map(str::trim)
                    .filter(|name| !name.is_empty())
                    .ok_or_else(|| anyhow!("new_name is required for action 'rename'."))?;
                let Some(edit) = client.rename(&path, line, column, new_name, timeout).await? else {
                    return Ok(ToolOutput::new(format!(
                        "No rename: the language server declined to rename the symbol at {}:{line}:{column}.",
                        display_path(&path, &base)
                    )));
                };
                let edits = edit.edit_count();
                let files = edit.files.len();
                let intro = format!(
                    "Rename to '{new_name}': {edits} edit{} in {files} file{}.",
                    if edits == 1 { "" } else { "s" },
                    if files == 1 { "" } else { "s" }
                );
                let plans = lsp_edit::plan(&edit, &base)?;
                if !params.apply {
                    return Ok(ToolOutput::new(lsp_edit::render_plan(&plans, &intro)));
                }
                if !lsp_edit::any_change(&plans) {
                    return Ok(ToolOutput::new(format!(
                        "{intro}\nNo changes: the edit reproduces the current content."
                    )));
                }
                let body = lsp_edit::commit(&ctx, &plans).await?;
                Ok(lsp_edit::attach(
                    ToolOutput::new(format!("{intro}\n{body}")).with_title(file.to_string()),
                    &plans,
                ))
            }
            Action::CodeActions => {
                let (line, column) = resolve_position(&path, params.line, params.character, params.symbol.as_deref())?;
                let (end_line, end_character) = match (params.end_line, params.end_character) {
                    (None, None) => (line, column),
                    (Some(end_line), Some(end_character)) => {
                        if end_line < line || (end_line == line && end_character < column) {
                            bail!("the range ends before it starts: line {end_line}, character {end_character}.");
                        }
                        (end_line, end_character)
                    }
                    _ => bail!("Provide both end_line and end_character, or neither."),
                };
                let actions = client
                    .code_actions(&path, line - 1, column - 1, end_line - 1, end_character - 1, timeout)
                    .await?;
                if actions.is_empty() {
                    return Ok(ToolOutput::new(format!(
                        "No code actions at {}:{line}:{column}.",
                        display_path(&path, &base)
                    )));
                }
                let Some(action_id) = params.action_id.as_deref().map(str::trim).filter(|id| !id.is_empty()) else {
                    let mut body = format!(
                        "Code actions at {}:{line}:{column} (apply one with action_id):\n",
                        display_path(&path, &base)
                    );
                    for (index, action) in actions.iter().enumerate() {
                        body.push_str(&format!("  {}. {}", index + 1, action.title));
                        if let Some(kind) = &action.kind {
                            body.push_str(&format!(" ({kind})"));
                        }
                        if let Some(reason) = &action.disabled {
                            body.push_str(&format!(" — disabled: {reason}"));
                        } else if action.command_only {
                            body.push_str(" — command only, not executed");
                        }
                        body.push('\n');
                    }
                    return Ok(ToolOutput::new(body));
                };
                let index: usize = action_id
                    .parse()
                    .map_err(|_| anyhow!("action_id '{action_id}' is not one of the ids listed for this position."))?;
                let Some(action) = index.checked_sub(1).and_then(|index| actions.get(index)) else {
                    bail!(
                        "action_id '{action_id}' is out of range; this position offers {} action{}.",
                        actions.len(),
                        if actions.len() == 1 { "" } else { "s" }
                    );
                };
                if let Some(reason) = &action.disabled {
                    bail!(
                        "action {action_id} '{}' is disabled by the server: {reason}",
                        action.title
                    );
                }
                let Some(edit) = &action.edit else {
                    bail!(
                        "action {action_id} '{}' is a command only, and the lsp tool does not execute commands.",
                        action.title
                    );
                };
                let plans = lsp_edit::plan(edit, &base)?;
                if !lsp_edit::any_change(&plans) {
                    return Ok(ToolOutput::new(format!(
                        "Action '{0}' changes nothing: the edit reproduces the current content.",
                        action.title
                    )));
                }
                let body = lsp_edit::commit(&ctx, &plans).await?;
                Ok(lsp_edit::attach(
                    ToolOutput::new(format!("Applied '{}':\n{body}", action.title)).with_title(file.to_string()),
                    &plans,
                ))
            }
            Action::Status => unreachable!("status returns before any file is resolved"),
        }
    }
}

async fn status_output(base: &Path) -> String {
    let clients = horon_agent_lsp::running_clients().await;
    if clients.is_empty() {
        return "No language server clients are running.".to_string();
    }
    clients
        .into_iter()
        .map(|client| {
            format!(
                "{}  {}  pid={}  idle={}s",
                client.server,
                display_path(&client.root, base),
                client.pid,
                client.idle_secs
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn working_dir(ctx: &ToolContext) -> PathBuf {
    let base = ctx
        .working_dir
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    std::fs::canonicalize(&base).unwrap_or(base)
}

fn effective_timeout(requested: Option<f64>) -> Duration {
    let seconds = requested.unwrap_or(DEFAULT_TIMEOUT_SECS);
    Duration::from_secs_f64(seconds.clamp(MIN_TIMEOUT_SECS, MAX_TIMEOUT_SECS))
}

/// Turn the model's 1-based position into a 1-based line and column.
///
/// A missing column is resolved by finding `symbol` on that line, which is how
/// a model that only knows the identifier can still ask a position question.
fn resolve_position(
    path: &Path,
    line: Option<u32>,
    character: Option<u32>,
    symbol: Option<&str>,
) -> Result<(u32, u32)> {
    let line = line.ok_or_else(|| anyhow!("line is required for this action."))?;
    if line == 0 {
        bail!("line is 1-based; line 0 does not exist.");
    }
    match character {
        Some(0) => bail!("character is 1-based; character 0 does not exist."),
        Some(column) => Ok((line, column)),
        None => {
            let Some(symbol) = symbol else {
                bail!("Provide `character` or `symbol` to locate a position on line {line}.");
            };
            if symbol.is_empty() {
                bail!("Provide `character` or `symbol` to locate a position on line {line}.");
            }
            let text = std::fs::read_to_string(path)?;
            let Some(source_line) = text.lines().nth((line - 1) as usize) else {
                bail!("Line {line} is beyond the end of {}.", path.display());
            };
            let Some(byte) = source_line.find(symbol) else {
                bail!("Symbol '{symbol}' not found on line {line}");
            };
            let column = source_line[..byte].encode_utf16().count() as u32 + 1;
            Ok((line, column))
        }
    }
}

fn render_locations(locations: &[horon_agent_lsp::Location], base: &Path) -> String {
    let mut lines: Vec<String> = locations
        .iter()
        .map(|location| {
            let display = display_path(&location.path, base);
            match source_line(&location.path, location.line) {
                Some(text) if !text.is_empty() => {
                    format!("{display}:{}:{}: {text}", location.line, location.column)
                }
                _ => format!("{display}:{}:{}", location.line, location.column),
            }
        })
        .collect();
    lines.sort();
    lines.dedup();
    lines.join("\n")
}

fn render_symbols(symbols: &[horon_agent_lsp::Symbol], depth: usize, output: &mut String) {
    for symbol in symbols {
        output.push_str(&format!(
            "{}{}: {} ({})\n",
            "  ".repeat(depth),
            symbol.line,
            symbol.name,
            symbol.kind
        ));
        render_symbols(&symbol.children, depth + 1, output);
    }
}

fn source_line(path: &Path, line: u32) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let source = text.lines().nth(line.checked_sub(1)? as usize)?;
    let trimmed = source.trim();
    if trimmed.chars().count() <= MAX_LINE_CHARS {
        return Some(trimmed.to_string());
    }
    let mut clipped: String = trimmed.chars().take(MAX_LINE_CHARS).collect();
    clipped.push('…');
    Some(clipped)
}

fn display_path(path: &Path, base: &Path) -> String {
    path.strip_prefix(base)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn one_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
#[path = "lsp_tests.rs"]
mod tests;
