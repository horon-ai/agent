//! Structured, source-bound resumption summaries.
//!
//! The provider answers with a typed JSON artifact (goal, done/incomplete/
//! cancelled work, files, evidence, decisions, next action) instead of free
//! prose. This module validates that artifact, renders it into the persisted
//! `summary_text`, and binds it to the exact original events it covered. A
//! response that is empty, tool-call-only, mistyped, or failed never becomes a
//! successful summary.
//!
//! Overflow shrinks the covered prefix by halves on tool-pair boundaries, at
//! most [`SummaryLimits::max_shrink_attempts`] times within
//! [`SummaryLimits::shrink_deadline`]. The excluded range stays in the live
//! tail in its original order, and the covered ids, digest, and first kept event
//! are recomputed for the reduced range.

use super::source_projection::{
    CoveredRange, PriorBoundary, SourceProjection, halve_covered_range, plan_covered_range, render_source_range,
};
use crate::message::ContentBlock;
use crate::provider::Provider;
use horon_agent_compaction_core::CHARS_PER_TOKEN_UPPER_BOUND;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// Upper bound on the restoration manifest entries persisted with a summary.
pub const MAX_RESTORATION_ENTRIES: usize = 10;

/// Absolute ceiling on the bytes budgeted for re-reading restored references.
pub const MAX_RESTORATION_BYTES: u64 = 50_000;

const SUMMARY_SYSTEM_PROMPT: &str = "You compact a working conversation into a structured resumption summary. \
Answer with one JSON object and nothing else. \
If the conversation ends with an unanswered question or a request awaiting the user's response, copy that exact \
question or request into \"pending_request\".";

/// Appended to the compaction prompt when the kept tail begins mid-turn.
const SPLIT_TURN_NOTE: &str = "The retained recent messages start in the middle of a turn. Fill \"split_turn_context\" \
with the user's original request for that turn and the key decisions made before the cut.\n\n";

const SUMMARY_SCHEMA_HEADER: &str = r#"Compact the conversation below so another agent can resume the work without re-reading it.

Return ONLY a JSON object with exactly these keys:
{
  "goal": string,                 // what the user is trying to achieve, in the user's own terms
  "done": [string],               // completed work, with concrete outcomes
  "incomplete": [string],         // started but unfinished work
  "cancelled": [string],          // work the user abandoned or explicitly dropped
  "files": [{"path": string, "note": string}], // files or skills that matter for resuming
  "evidence": [string],           // commands, test results, hashes, or event ids that prove state
  "decisions": [string],          // choices made and their reasons
  "pending_request": string,      // the question or request left unanswered at the end, if any
  "split_turn_context": string,   // only when the cut splits a turn: the user's original request and the decisions made before the cut
  "next_action": string           // the single next step to take
}

Rules:
- The newest user instruction wins. If the user revised an earlier request, record the latest version and drop the superseded one.
- Never claim an image was read; images are listed as not read.
- Do not invent files, outcomes, or evidence that the conversation does not show.
- Cite the source event id in brackets when a fact depends on a specific message.

"#;

/// Why a structured summary was not produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SummaryError {
    /// The provider returned no text at all.
    EmptyResponse,
    /// The provider returned only a tool call instead of an answer.
    ToolCallOnly,
    /// The response was not the required typed artifact.
    InvalidArtifact { detail: String },
    /// The provider call itself failed.
    Provider { detail: String },
    /// Shrinking could not make the prompt fit.
    OverflowNoProgress { attempts: u32 },
    /// The excluded tail alone exceeds the recent-tail allowance.
    TailExceedsBudget { tail_tokens: usize, budget_tokens: usize },
    /// Nothing safe to summarize.
    NothingToSummarize,
}

impl std::fmt::Display for SummaryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyResponse => write!(f, "summary response was empty"),
            Self::ToolCallOnly => write!(f, "summary response was a tool call, not a summary"),
            Self::InvalidArtifact { detail } => write!(f, "summary artifact invalid: {detail}"),
            Self::Provider { detail } => write!(f, "summary provider error: {detail}"),
            Self::OverflowNoProgress { attempts } => {
                write!(f, "summary overflow after {attempts} shrink attempt(s)")
            }
            Self::TailExceedsBudget {
                tail_tokens,
                budget_tokens,
            } => write!(
                f,
                "kept tail needs {tail_tokens} tokens but only {budget_tokens} are allowed"
            ),
            Self::NothingToSummarize => write!(f, "nothing safe to summarize"),
        }
    }
}

impl std::error::Error for SummaryError {}

/// One file or skill reference worth re-reading when the work resumes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RestorationEntry {
    pub path: String,
    pub kind: String,
    pub note: Option<String>,
}

/// Bounded restoration manifest persisted with a compacted event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestorationManifest {
    pub entries: Vec<RestorationEntry>,
    pub total_bytes: u64,
}

impl RestorationManifest {
    /// The stored form written to the session and the event graph.
    ///
    /// Every boundary that carries a manifest uses this one conversion, so a
    /// handoff document and a structured summary persist the same shape.
    pub fn to_stored(&self) -> crate::session::StoredRestorationManifest {
        crate::session::StoredRestorationManifest {
            entries: self
                .entries
                .iter()
                .map(|entry| crate::session::StoredRestorationEntry {
                    path: entry.path.clone(),
                    kind: entry.kind.clone(),
                    note: entry.note.clone(),
                })
                .collect(),
            total_bytes: self.total_bytes,
        }
    }
}

/// The typed artifact a provider must return.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryArtifact {
    pub goal: String,
    #[serde(default)]
    pub done: Vec<String>,
    #[serde(default)]
    pub incomplete: Vec<String>,
    #[serde(default)]
    pub cancelled: Vec<String>,
    #[serde(default)]
    pub files: Vec<ArtifactFile>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub decisions: Vec<String>,
    /// The question or request left unanswered when the conversation was cut.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_request: Option<String>,
    /// Present only when the kept tail begins mid-turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_turn_context: Option<String>,
    pub next_action: String,
}

/// One file reference inside a [`SummaryArtifact`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl SummaryArtifact {
    /// Parse and validate a provider response.
    pub fn parse(text: &str) -> Result<Self, SummaryError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(SummaryError::EmptyResponse);
        }
        if looks_like_tool_call_text(trimmed) {
            return Err(SummaryError::ToolCallOnly);
        }
        let json_text = extract_json_object(trimmed).ok_or_else(|| SummaryError::InvalidArtifact {
            detail: "response contained no JSON object".to_string(),
        })?;
        let value: serde_json::Value =
            serde_json::from_str(json_text).map_err(|err| SummaryError::InvalidArtifact {
                detail: format!("invalid JSON: {err}"),
            })?;
        if looks_like_tool_call_value(&value) {
            return Err(SummaryError::ToolCallOnly);
        }
        let object = value.as_object().ok_or_else(|| SummaryError::InvalidArtifact {
            detail: "artifact is not a JSON object".to_string(),
        })?;

        let goal = required_string(object, "goal")?;
        let next_action = required_string(object, "next_action")?;
        Ok(Self {
            goal,
            done: string_array(object, "done")?,
            incomplete: string_array(object, "incomplete")?,
            cancelled: string_array(object, "cancelled")?,
            files: file_array(object, "files")?,
            evidence: string_array(object, "evidence")?,
            decisions: string_array(object, "decisions")?,
            pending_request: optional_string(object, "pending_request")?,
            split_turn_context: optional_string(object, "split_turn_context")?,
            next_action,
        })
    }

    /// Render the artifact as the persisted summary text.
    pub fn render(&self) -> String {
        let mut out = String::new();
        push_section(&mut out, "Goal", std::slice::from_ref(&self.goal));
        push_section(&mut out, "Done", &self.done);
        push_section(&mut out, "Incomplete", &self.incomplete);
        push_section(&mut out, "Cancelled", &self.cancelled);
        if !self.files.is_empty() {
            out.push_str("## Files\n");
            for file in &self.files {
                match &file.note {
                    Some(note) => out.push_str(&format!("- `{}` — {}\n", file.path, note)),
                    None => out.push_str(&format!("- `{}`\n", file.path)),
                }
            }
            out.push('\n');
        }
        push_section(&mut out, "Evidence", &self.evidence);
        push_section(&mut out, "Decisions", &self.decisions);
        if let Some(pending) = self.pending_request.as_deref() {
            push_body(&mut out, "Pending request:", pending);
        }
        if let Some(context) = self.split_turn_context.as_deref() {
            push_body(&mut out, "Turn in progress at the cut:", context);
        }
        push_section(&mut out, "Next action", std::slice::from_ref(&self.next_action));
        out.trim_end().to_string()
    }
}

/// One prose section: a heading and a single paragraph.
fn push_body(out: &mut String, title: &str, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    out.push_str("## ");
    out.push_str(title);
    out.push('\n');
    out.push_str(text);
    out.push_str("\n\n");
}

fn push_section(out: &mut String, title: &str, items: &[String]) {
    if items.is_empty() || items.iter().all(|item| item.trim().is_empty()) {
        return;
    }
    out.push_str(&format!("## {title}\n"));
    for item in items {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        if items.len() == 1 && title != "Done" && title != "Incomplete" && title != "Cancelled" {
            out.push_str(item);
        } else {
            out.push_str("- ");
            out.push_str(item);
        }
        out.push('\n');
    }
    out.push('\n');
}

fn required_string(object: &serde_json::Map<String, serde_json::Value>, field: &str) -> Result<String, SummaryError> {
    let value = object.get(field).ok_or_else(|| SummaryError::InvalidArtifact {
        detail: format!("missing field `{field}`"),
    })?;
    let text = value.as_str().ok_or_else(|| SummaryError::InvalidArtifact {
        detail: format!("field `{field}` must be a string"),
    })?;
    if text.trim().is_empty() {
        return Err(SummaryError::InvalidArtifact {
            detail: format!("field `{field}` must not be empty"),
        });
    }
    Ok(text.trim().to_string())
}

fn optional_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Option<String>, SummaryError> {
    let Some(value) = object.get(field) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let text = value.as_str().ok_or_else(|| SummaryError::InvalidArtifact {
        detail: format!("field `{field}` must be a string"),
    })?;
    let text = text.trim();
    Ok((!text.is_empty()).then(|| text.to_string()))
}

fn string_array(object: &serde_json::Map<String, serde_json::Value>, field: &str) -> Result<Vec<String>, SummaryError> {
    let Some(value) = object.get(field) else {
        return Ok(Vec::new());
    };
    let array = value.as_array().ok_or_else(|| SummaryError::InvalidArtifact {
        detail: format!("field `{field}` must be an array"),
    })?;
    array
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
                .ok_or_else(|| SummaryError::InvalidArtifact {
                    detail: format!("field `{field}` must contain strings"),
                })
        })
        .collect()
}

fn file_array(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
) -> Result<Vec<ArtifactFile>, SummaryError> {
    let Some(value) = object.get(field) else {
        return Ok(Vec::new());
    };
    let array = value.as_array().ok_or_else(|| SummaryError::InvalidArtifact {
        detail: format!("field `{field}` must be an array"),
    })?;
    array
        .iter()
        .map(|item| {
            let entry = item.as_object().ok_or_else(|| SummaryError::InvalidArtifact {
                detail: format!("field `{field}` must contain objects"),
            })?;
            let path = required_string(entry, "path")?;
            let note = match entry.get("note") {
                Some(note) if note.is_null() => None,
                Some(note) => Some(
                    note.as_str()
                        .ok_or_else(|| SummaryError::InvalidArtifact {
                            detail: "field `files[].note` must be a string".to_string(),
                        })?
                        .trim()
                        .to_string(),
                )
                .filter(|note| !note.is_empty()),
                None => None,
            };
            Ok(ArtifactFile { path, note })
        })
        .collect()
}

fn looks_like_tool_call_text(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    lowered.starts_with("<tool_call") || lowered.starts_with("<function_call") || lowered.starts_with("<|tool")
}

fn looks_like_tool_call_value(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.contains_key("goal") {
        return false;
    }
    ["tool_calls", "tool_call", "function_call", "function"]
        .iter()
        .any(|key| object.contains_key(*key))
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| &text[start..=end])
}

/// One earlier summary this compaction builds on.
#[derive(Clone, Debug)]
pub struct PriorSummary {
    pub text: String,
    pub compaction_id: Option<String>,
    pub content_hash: Option<String>,
}

/// Bounds for one summary request.
#[derive(Clone, Copy, Debug)]
pub struct SummaryLimits {
    pub max_prompt_chars: usize,
    /// Tokens the kept tail may occupy before the window cannot hold it.
    pub tail_budget_tokens: usize,
    pub keep_newest_user: bool,
    pub max_shrink_attempts: u32,
    pub shrink_deadline: Duration,
    pub context_window: usize,
    /// The kept tail begins mid-turn, so the summary must describe the
    /// original request and the decisions made before the cut.
    pub split_turn: bool,
}

impl SummaryLimits {
    pub fn for_window(context_window: usize) -> Self {
        Self::for_window_with_reserve(context_window, 4_000)
    }

    /// `reserve` is the headroom kept free below the trigger point, so the kept
    /// tail may use everything above it.
    pub fn for_window_with_reserve(context_window: usize, reserve: usize) -> Self {
        let usable = context_window.saturating_sub(reserve);
        Self {
            max_prompt_chars: usable * CHARS_PER_TOKEN_UPPER_BOUND,
            tail_budget_tokens: usable,
            keep_newest_user: true,
            max_shrink_attempts: 3,
            shrink_deadline: Duration::from_secs(240),
            context_window,
            split_turn: false,
        }
    }
}

/// A validated summary bound to the original events it covered.
#[derive(Clone, Debug)]
pub struct BoundSummary {
    pub artifact: SummaryArtifact,
    pub summary_text: String,
    pub covered: CoveredRange,
    pub covered_event_ids: Vec<String>,
    pub covered_through_event_id: Option<String>,
    pub first_kept_event_id: Option<String>,
    pub source_prefix_digest: Option<String>,
    pub prior_compaction_id: Option<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub shrink_attempts: u32,
    pub restoration: RestorationManifest,
}

/// Generate a structured summary for `candidate_end` messages of `projection`.
pub async fn generate_structured_summary(
    provider: &dyn Provider,
    projection: &SourceProjection,
    candidate_end: usize,
    prior: Option<&PriorSummary>,
    limits: &SummaryLimits,
) -> Result<BoundSummary, SummaryError> {
    let started = Instant::now();
    let mut candidate_end = candidate_end;
    let mut shrink_attempts = 0u32;

    loop {
        let range = plan_covered_range(projection, candidate_end, limits.keep_newest_user)?;
        let tail_tokens = projection.tail_tokens(range);
        if tail_tokens > limits.tail_budget_tokens {
            return Err(SummaryError::TailExceedsBudget {
                tail_tokens,
                budget_tokens: limits.tail_budget_tokens,
            });
        }

        let prompt = build_structured_prompt(projection, range, prior, limits.max_prompt_chars, limits.split_turn);
        if prompt.len() > limits.max_prompt_chars {
            let next = shrink_or_fail(projection, range, limits, shrink_attempts, started)?;
            candidate_end = next.end();
            shrink_attempts += 1;
            continue;
        }

        let (text, usage) = match provider
            .complete_simple_with_usage(&prompt, SUMMARY_SYSTEM_PROMPT)
            .await
        {
            Ok(answer) => answer,
            Err(err) => {
                let detail = err.to_string();
                if is_overflow_error(&detail) {
                    let next = shrink_or_fail(projection, range, limits, shrink_attempts, started)?;
                    candidate_end = next.end();
                    shrink_attempts += 1;
                    continue;
                }
                return Err(SummaryError::Provider { detail });
            }
        };
        match SummaryArtifact::parse(&text) {
            Ok(artifact) => {
                let summary_text = artifact.render();
                if summary_text.trim().is_empty() {
                    return Err(SummaryError::EmptyResponse);
                }
                return Ok(bind_summary(
                    projection,
                    range,
                    SummaryCompletion {
                        artifact,
                        summary_text,
                        usage,
                        shrink_attempts,
                    },
                    prior,
                    limits,
                ));
            }
            Err(SummaryError::Provider { detail }) => {
                return Err(SummaryError::Provider { detail });
            }
            Err(error) => return Err(error),
        }
    }
}

/// Generate a structured summary on `summary_model` when it is set and differs
/// from the session model. A provider failure on that model is retried once on
/// the session provider; a model the provider rejects is skipped with a warning.
/// Returns the provider that produced the summary, for usage accounting.
pub async fn generate_structured_summary_with_model(
    provider: &std::sync::Arc<dyn Provider>,
    summary_model: Option<&str>,
    projection: &SourceProjection,
    candidate_end: usize,
    prior: Option<&PriorSummary>,
    limits: &SummaryLimits,
) -> Result<(BoundSummary, std::sync::Arc<dyn Provider>), SummaryError> {
    let alternate = summary_model
        .map(str::trim)
        .filter(|model| !model.is_empty() && *model != provider.model())
        .and_then(|model| {
            let alt = provider.fork();
            match alt.set_model(model) {
                Ok(()) => Some((model, alt)),
                Err(error) => {
                    crate::logging::warn(&format!(
                        "compaction summary_model {model} rejected: {error}; using the session model"
                    ));
                    None
                }
            }
        });
    if let Some((model, alt)) = alternate {
        match generate_structured_summary(alt.as_ref(), projection, candidate_end, prior, limits).await {
            Ok(bound) => return Ok((bound, alt)),
            Err(SummaryError::Provider { detail }) => crate::logging::warn(&format!(
                "compaction summary_model {model} failed: {detail}; retrying with the session model"
            )),
            Err(error) => return Err(error),
        }
    }
    let bound = generate_structured_summary(provider.as_ref(), projection, candidate_end, prior, limits).await?;
    Ok((bound, std::sync::Arc::clone(provider)))
}

/// Whether a provider error means the request did not fit its window.
pub fn is_overflow_error(detail: &str) -> bool {
    let lowered = detail.to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "context length",
        "context_length_exceeded",
        "maximum context",
        "prompt is too long",
        "too many tokens",
        "request too large",
        "payload too large",
        "reduce the length",
    ];
    MARKERS.iter().any(|marker| lowered.contains(marker))
}

fn shrink_or_fail(
    projection: &SourceProjection,
    range: CoveredRange,
    limits: &SummaryLimits,
    attempts: u32,
    started: Instant,
) -> Result<CoveredRange, SummaryError> {
    if attempts >= limits.max_shrink_attempts || started.elapsed() >= limits.shrink_deadline {
        return Err(SummaryError::OverflowNoProgress { attempts });
    }
    halve_covered_range(projection, range, limits.keep_newest_user).ok_or(SummaryError::OverflowNoProgress { attempts })
}

/// One accepted provider answer plus the bookkeeping the binding needs.
struct SummaryCompletion {
    artifact: SummaryArtifact,
    summary_text: String,
    usage: horon_agent_provider_core::SimpleCompletionUsage,
    shrink_attempts: u32,
}

fn bind_summary(
    projection: &SourceProjection,
    range: CoveredRange,
    completion: SummaryCompletion,
    prior: Option<&PriorSummary>,
    limits: &SummaryLimits,
) -> BoundSummary {
    let prior_compaction_id = prior.and_then(|summary| summary.compaction_id.clone());
    let prior_boundary = match prior_compaction_id.as_deref() {
        Some(event_id) => PriorBoundary::Compacted {
            event_id,
            content_hash: prior.and_then(|summary| summary.content_hash.as_deref()),
        },
        None => PriorBoundary::None,
    };
    BoundSummary {
        covered_event_ids: projection.covered_ids(range).unwrap_or_default(),
        covered_through_event_id: projection.covered_through_id(range),
        first_kept_event_id: projection.first_kept_id(range),
        source_prefix_digest: projection.source_prefix_digest(range, prior_boundary),
        prior_compaction_id,
        input_tokens: completion.usage.input_tokens,
        output_tokens: completion.usage.output_tokens,
        restoration: collect_restoration(projection, range, limits),
        covered: range,
        artifact: completion.artifact,
        summary_text: completion.summary_text,
        shrink_attempts: completion.shrink_attempts,
    }
}

/// Build the prompt: fixed schema and prior summary first, then the rendered
/// covered range within the remaining budget.
pub fn build_structured_prompt(
    projection: &SourceProjection,
    range: CoveredRange,
    prior: Option<&PriorSummary>,
    max_prompt_chars: usize,
    split_turn: bool,
) -> String {
    let prior_section = prior
        .map(|summary| {
            format!(
                "## Previous summary (already covered, keep what is still true)\n\n{}\n\n",
                summary.text.trim()
            )
        })
        .unwrap_or_default();
    let split_note = if split_turn { SPLIT_TURN_NOTE } else { "" };
    let fixed =
        SUMMARY_SCHEMA_HEADER.len() + prior_section.len() + split_note.len() + "## Conversation to summarize\n\n".len();
    let content_budget = max_prompt_chars.saturating_sub(fixed);
    let rendered = render_source_range(projection, range, content_budget);
    format!("{SUMMARY_SCHEMA_HEADER}{prior_section}{split_note}## Conversation to summarize\n\n{rendered}")
}

/// Collect at most [`MAX_RESTORATION_ENTRIES`] file or skill references touched
/// inside the covered range.
pub fn collect_restoration(
    projection: &SourceProjection,
    range: CoveredRange,
    limits: &SummaryLimits,
) -> RestorationManifest {
    collect_restoration_with_remaining(projection, range, limits.context_window, u64::MAX)
}

/// Collect restoration references bounded by the window, the absolute ceiling
/// and whatever budget the rest of the projection left unused.
pub fn collect_restoration_with_remaining(
    projection: &SourceProjection,
    range: CoveredRange,
    context_window: usize,
    remaining_chars: u64,
) -> RestorationManifest {
    let budget = restoration_byte_budget(context_window)
        .min(remaining_chars)
        .min(MAX_RESTORATION_BYTES);
    let mut entries: Vec<RestorationEntry> = Vec::new();
    let covered = projection.messages().get(range.start()..range.end());
    for source in covered.unwrap_or_default() {
        for block in &source.message.content {
            let ContentBlock::ToolUse { name, input, .. } = block else {
                continue;
            };
            let Some((path, kind)) = restoration_reference(input) else {
                continue;
            };
            if entries.iter().any(|entry| entry.path == path) {
                continue;
            }
            entries.push(RestorationEntry {
                path,
                kind,
                note: Some(name.clone()),
            });
            if entries.len() >= MAX_RESTORATION_ENTRIES {
                break;
            }
        }
        if entries.len() >= MAX_RESTORATION_ENTRIES {
            break;
        }
    }
    RestorationManifest {
        entries,
        total_bytes: budget,
    }
}

fn restoration_byte_budget(context_window: usize) -> u64 {
    let scaled = (context_window as f64 * 0.15) as u64;
    MAX_RESTORATION_BYTES.min(scaled)
}

fn restoration_reference(input: &serde_json::Value) -> Option<(String, String)> {
    for key in ["path", "file_path", "filePath", "notebook_path"] {
        if let Some(path) = input.get(key).and_then(|value| value.as_str())
            && !path.trim().is_empty()
        {
            return Some((path.trim().to_string(), "file".to_string()));
        }
    }
    for key in ["skill", "skill_name"] {
        if let Some(skill) = input.get(key).and_then(|value| value.as_str())
            && !skill.trim().is_empty()
        {
            return Some((skill.trim().to_string(), "skill".to_string()));
        }
    }
    None
}

#[cfg(test)]
mod summary_limit_tests {
    use super::*;

    /// `max_prompt_chars` has to be derived from the constant, not from a
    /// hardcoded number.
    ///
    /// F4's R1: this multiply site had no test deriving an expectation from
    /// `CHARS_PER_TOKEN_UPPER_BOUND`, so changing the multiplier passed every
    /// suite. The window and reserve are chosen so a /3 or /5 divisor gives a
    /// different answer (10_000 - 4_000 = 6_000 usable: x4 = 24_000, x3 = 18_000,
    /// x5 = 30_000).
    #[test]
    fn the_prompt_char_budget_is_the_token_budget_times_the_upper_bound() {
        let limits = SummaryLimits::for_window_with_reserve(10_000, 4_000);
        assert_eq!(limits.tail_budget_tokens, 6_000);
        assert_eq!(
            limits.max_prompt_chars,
            6_000 * horon_agent_compaction_core::CHARS_PER_TOKEN_UPPER_BOUND,
            "the character budget must come from the token budget and the constant"
        );
        assert_eq!(limits.max_prompt_chars, 24_000);
    }

    /// The reserve is subtracted before the multiply, so a reserve change moves
    /// the character budget too.
    #[test]
    fn the_reserve_is_subtracted_before_the_multiply() {
        let generous = SummaryLimits::for_window_with_reserve(10_000, 4_000);
        let tight = SummaryLimits::for_window_with_reserve(10_000, 1_000);
        assert_eq!(tight.tail_budget_tokens - generous.tail_budget_tokens, 3_000);
        assert_eq!(
            tight.max_prompt_chars - generous.max_prompt_chars,
            3_000 * horon_agent_compaction_core::CHARS_PER_TOKEN_UPPER_BOUND
        );
    }
}
