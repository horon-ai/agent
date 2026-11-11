use horon_agent_message_types::{ContentBlock, Message, Role};
use horon_agent_token_estimate::{
    TokenFamily, estimate_tokens as estimate_tokens_by_script, estimate_tokens_for_family,
};
use std::collections::HashSet;
use std::hash::{Hash, Hasher};

pub mod handoff_frame;
pub mod pruning;

/// Default token budget (200k tokens - matches Claude's actual context limit)
pub const DEFAULT_TOKEN_BUDGET: usize = 200_000;

/// If context is above this threshold when compaction starts, do a synchronous
/// hard-compact (drop old messages) so the API call doesn't fail.
pub const CRITICAL_THRESHOLD: f32 = 0.95;

/// Minimum threshold for manual compaction (can compact at any time above this)
pub const MANUAL_COMPACT_MIN_THRESHOLD: f32 = 0.10;

/// Absolute minimum turns to keep during emergency compaction
pub const MIN_TURNS_TO_KEEP: usize = 2;

/// Max bytes (UTF-8 length, not chars) for a single tool result during emergency truncation
pub const EMERGENCY_TOOL_RESULT_MAX_CHARS: usize = 4000;

/// Max chars to keep for an inline image payload during emergency recovery.
/// Images are usually base64 screenshots; at hard-threshold time the useful
/// state should be represented by nearby tool text/summary, not by replaying a
/// huge raw image in the recent tail.
pub const EMERGENCY_IMAGE_MAX_CHARS: usize = 1024;

/// Approximate maximum request body size (in base64 characters) we aim to keep
/// the transcript under when recovering from a provider "request too large" /
/// 413 payload error. Anthropic rejects requests whose serialized body exceeds
/// roughly 32 MB; this distinct failure mode is driven almost entirely by inline
/// base64 images, which the normal token-budget accounting deliberately
/// undercounts (see `IMAGE_TOKEN_COST`). We target a conservative budget well
/// under the hard provider cap so a single retry reliably fits.
pub const PAYLOAD_IMAGE_CHAR_BUDGET: usize = 12 * 1024 * 1024;

/// Labels the token-estimation scheme the compaction pipeline measures with.
///
/// Documentation only: no decision branches on it. A report that quotes a token
/// count can quote this string, and a change to the rates in
/// `horon-agent-token-estimate` must bump it.
pub const TOKEN_ESTIMATE_VERSION: &str = "script-aware-v2";

/// Characters per token assumed when a token budget has to be turned back into
/// a character allowance.
///
/// The inverse direction only. Characters are never turned into tokens with a
/// fixed factor: [`estimate_text_tokens`] prices each script separately, and
/// dividing by four is exactly the error this split removes. The value stays 4
/// so a token budget still buys the character allowance it bought before.
pub const CHARS_PER_TOKEN_UPPER_BOUND: usize = 4;

/// Approximate token cost charged for a single inline image.
///
/// Image content blocks carry base64-encoded payloads that are often hundreds
/// of kilobytes. Counting that raw base64 length as message text (len / 4)
/// massively overestimates the real context cost: providers tokenize images by
/// resolution, not by transport-encoded byte length, and a typical screenshot
/// costs on the order of ~1-2k tokens regardless of base64 size. Using the raw
/// length caused the token estimate to balloon far above the real
/// provider-observed input, spuriously tripping the compaction threshold and
/// driving repeated back-to-back ("triple") compactions that could not bring
/// the estimate down because the images stayed in the recent kept turns.
///
/// We charge a flat, slightly conservative per-image token budget instead.
pub const IMAGE_TOKEN_COST: usize = 1_600;

/// Fixed token overhead for system prompt + tool definitions.
/// These are not counted in message content but do count toward the context limit.
/// Estimated conservatively: ~8k tokens for system prompt + ~10k for 50+ tools.
pub const SYSTEM_OVERHEAD_TOKENS: usize = 18_000;

// ── Context budget policy (D6) ──────────────────────────────────────────────
//
// One owner decides when compaction runs and how much of the window stays
// verbatim. The budget is derived from the model window in three steps: a
// reserve (headroom for the response and provider-side overhead), a trigger
// ratio for the window tier, and a recent-tail token allowance. The trigger is
// always clamped by the reserve so compaction can never target a point that
// leaves no room to answer.

/// Default headroom kept free below the trigger point.
pub const DEFAULT_RESERVE_TOKENS: usize = 16_384;

/// Fraction of the window that scales the reserve on large windows.
pub const RESERVE_WINDOW_FRACTION: f64 = 0.04;

/// Ceiling on the window-scaled reserve.
pub const RESERVE_WINDOW_CAP: usize = 49_152;

/// Default token allowance for the verbatim recent tail.
pub const DEFAULT_RECENT_TOKENS: usize = 20_000;

/// Floor on the recent-tail allowance, so a tiny window still keeps a usable
/// amount of conversation verbatim.
pub const MIN_RECENT_TOKENS: usize = 1_024;

/// Fraction of the trigger point the kept tail may reach after compaction.
///
/// The tail plus the summary allowance must stay below this band, otherwise a
/// small window compacts and immediately crosses the trigger again.
pub const COMPACTION_RECOVERY_BAND: f32 = 0.8;

/// Ceiling on the token allowance reserved for the summary itself when the
/// recovery band is applied.
pub const SUMMARY_TOKEN_ALLOWANCE_CAP: usize = 4_096;

/// Slack kept between the trigger point and the reserve boundary. The trigger
/// may never come closer than this to `window - reserve`.
pub const THRESHOLD_SAFETY_HEADROOM_TOKENS: usize = 1_024;

/// Adaptive ratio step applied from the previous compaction's saving ratio.
pub const ADAPTIVE_RATIO_STEP: f64 = 0.05;

/// Lower bound of the adaptive ratio band.
pub const ADAPTIVE_RATIO_MIN: f64 = 0.40;

/// Upper bound of the adaptive ratio band.
pub const ADAPTIVE_RATIO_MAX: f64 = 0.85;

/// Reserve below which the recent tail must not eat into the remaining window.
pub const RECENT_TAIL_RATIO_SLACK: f64 = 0.05;

/// Window tier → trigger ratio.
///
/// Larger windows can afford to run further from full because a single turn
/// re-sends the whole transcript: at 512k the request cost of one more turn is
/// already large, so compaction starts at 70% instead of 80%.
pub fn base_context_ratio(window: usize) -> f64 {
    if window <= 16_000 {
        0.45
    } else if window <= 32_000 {
        0.50
    } else if window <= 64_000 {
        0.55
    } else if window <= 128_000 {
        0.60
    } else if window <= 512_000 {
        0.70
    } else {
        0.80
    }
}

/// Apply the previous compaction's saving ratio to the window-tier ratio.
///
/// Saving more than half the context means the summary was cheap, so compaction
/// can wait longer (lower ratio). Saving almost nothing means compaction barely
/// helped, so it should start earlier (higher ratio). The result is clamped to
/// the policy band.
pub fn adaptive_context_ratio(base_ratio: f64, prior_saving_ratio: Option<f64>) -> f64 {
    let adjusted = match prior_saving_ratio {
        Some(saved) if saved > 0.5 => base_ratio - ADAPTIVE_RATIO_STEP,
        Some(saved) if saved < 0.1 => base_ratio + ADAPTIVE_RATIO_STEP,
        _ => base_ratio,
    };
    adjusted.clamp(ADAPTIVE_RATIO_MIN, ADAPTIVE_RATIO_MAX)
}

/// Effective reserve: the configured value, raised to at least the window-scaled
/// minimum (`window * 0.04`, capped at 48 KiB). A larger configured reserve is
/// never lowered.
pub fn effective_reserve_tokens(configured: usize, window: usize) -> usize {
    let scaled = ((window as f64 * RESERVE_WINDOW_FRACTION) as usize).min(RESERVE_WINDOW_CAP);
    configured.max(scaled)
}

/// Recent-tail token allowance: the configured value, lowered when the window
/// cannot spare it. `window * (1 - ratio - 0.05)` is what remains after the
/// trigger point plus slack, with a 1024-token floor.
pub fn recent_tokens_for_window(window: usize, ratio: f64, configured: usize) -> usize {
    let remaining = (window as f64 * (1.0 - ratio - RECENT_TAIL_RATIO_SLACK))
        .round()
        .max(0.0) as usize;
    configured.min(remaining.max(MIN_RECENT_TOKENS))
}

/// Resolved budget for one context window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContextBudget {
    /// Model context window the budget was derived from.
    pub window: usize,
    /// Headroom kept free below the trigger point.
    pub reserve: usize,
    /// Trigger ratio after the adaptive adjustment.
    pub ratio: f64,
    /// Token count at which compaction triggers.
    pub threshold_tokens: usize,
    /// Token allowance for the verbatim recent tail.
    pub recent_tokens: usize,
}

/// The configured window cannot hold a usable budget: the reserve alone (plus
/// the safety headroom) exhausts it. Compaction cannot be planned at all, so
/// this must be reported before any provider call is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextBudgetError {
    ContextBudgetTooSmall {
        window: usize,
        reserve: usize,
        headroom_tokens: usize,
    },
}

impl std::fmt::Display for ContextBudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ContextBudgetTooSmall {
                window,
                reserve,
                headroom_tokens,
            } => write!(
                f,
                "context budget too small: window={window} tokens, reserve={reserve}, headroom={headroom_tokens}"
            ),
        }
    }
}

impl std::error::Error for ContextBudgetError {}

/// Derive the context budget for `window`.
///
/// `threshold = min(window * ratio, window - reserve - 1024)`. When the ceiling
/// term is zero the window cannot hold a budget and
/// [`ContextBudgetError::ContextBudgetTooSmall`] is returned.
pub fn compute_context_budget(
    window: usize,
    configured_reserve: usize,
    configured_recent: usize,
    prior_saving_ratio: Option<f64>,
) -> Result<ContextBudget, ContextBudgetError> {
    let reserve = effective_reserve_tokens(configured_reserve, window);
    let ratio = adaptive_context_ratio(base_context_ratio(window), prior_saving_ratio);

    let ceiling = window.saturating_sub(reserve + THRESHOLD_SAFETY_HEADROOM_TOKENS);
    if ceiling == 0 {
        return Err(ContextBudgetError::ContextBudgetTooSmall {
            window,
            reserve,
            headroom_tokens: THRESHOLD_SAFETY_HEADROOM_TOKENS,
        });
    }

    let threshold_tokens = ((window as f64) * ratio).round() as usize;
    let threshold_tokens = threshold_tokens.min(ceiling);
    let recent_tokens = recent_tokens_for_window(window, ratio, configured_recent);
    let recent_tokens = clamp_recent_to_recovery_band(threshold_tokens, recent_tokens);

    Ok(ContextBudget {
        window,
        reserve,
        ratio,
        threshold_tokens,
        recent_tokens,
    })
}

/// Cap the recent-tail allowance so the kept tail plus the summary allowance
/// stays inside [`COMPACTION_RECOVERY_BAND`] of the trigger point.
///
/// A tail sized only by the remaining window can already sit close to the
/// trigger: on a 24k window the old 10,800-token tail plus a summary crossed the
/// 6,592-token trigger immediately, so compaction repeated every turn. The
/// summary allowance shrinks with the window (a quarter of the trigger, capped
/// at [`SUMMARY_TOKEN_ALLOWANCE_CAP`]) because a small window cannot spend the
/// full 4,096 tokens on a summary.
fn clamp_recent_to_recovery_band(threshold_tokens: usize, recent_tokens: usize) -> usize {
    let summary_allowance = (threshold_tokens / 4).min(SUMMARY_TOKEN_ALLOWANCE_CAP);
    let band_ceiling = (threshold_tokens as f64 * f64::from(COMPACTION_RECOVERY_BAND)) as usize;
    let band_recent = band_ceiling.saturating_sub(summary_allowance).max(MIN_RECENT_TOKENS);
    recent_tokens.min(band_recent)
}

/// Context size to measure: the conservative (larger) of the local estimate and
/// the provider-reported usage. Cache-hit tokens are part of the context and are
/// never subtracted; `effective_context_tokens_from_usage` already returns the
/// full prompt size for both accounting styles.
pub fn conservative_context_tokens(estimate_tokens: usize, observed_input_tokens: Option<u64>) -> usize {
    let observed = observed_input_tokens
        .and_then(|tokens| usize::try_from(tokens).ok())
        .unwrap_or(0);
    estimate_tokens.max(observed)
}

/// Index of the first message kept verbatim when the recent tail is limited to
/// `recent_tokens` tokens.
///
/// Messages are measured in tokens ([`message_token_count`]), so a Korean
/// message is no longer priced by its UTF-8 byte length. At least
/// `min_turns` messages are always kept, and the newest user message is always
/// inside the kept tail so the current request is never summarized away. Tool
/// call/result pairing is enforced by the caller with [`safe_compaction_cutoff`].
pub fn recent_cutoff_for_token_budget(messages: &[Message], recent_tokens: usize, min_turns: usize) -> usize {
    if messages.is_empty() {
        return 0;
    }

    let mut kept = 0usize;
    let mut tokens = 0usize;
    for message in messages.iter().rev() {
        let message_tokens = message_token_count(message);
        if kept > 0 && tokens + message_tokens > recent_tokens {
            break;
        }
        tokens = tokens.saturating_add(message_tokens);
        kept += 1;
    }

    let floor = min_turns.min(messages.len());
    let cutoff = messages.len().saturating_sub(kept.max(floor));

    // Never summarize the newest user request: keep it and everything after it.
    match messages.iter().rposition(|message| message.role == Role::User) {
        Some(newest_user) => cutoff.min(newest_user),
        None => cutoff,
    }
}

/// Move a recent-tail cutoff back to the start of the turn it lands inside.
///
/// The kept tail should open on a user instruction, not on the middle of a
/// tool round. When the slack between the planned cutoff and the nearest turn
/// start is worth less than `slack_tokens`, pulling the cutoff back keeps the
/// tail coherent for the same price. Otherwise the cutoff stays where it was
/// and the split is reported so the summary prompt can explain the partial
/// turn.
///
/// Returns `(cutoff, split)`. `split` is true only when the returned cutoff is
/// not a turn start, i.e. the kept tail still begins mid-turn.
pub fn align_cutoff_to_turn_start(messages: &[Message], cutoff: usize, slack_tokens: usize) -> (usize, bool) {
    if cutoff == 0 || cutoff >= messages.len() || pruning::is_turn_start(&messages[cutoff]) {
        return (cutoff, false);
    }
    let start = match messages[..cutoff].iter().rposition(pruning::is_turn_start) {
        Some(start) => start,
        None => return (cutoff, true),
    };
    let moved: usize = messages[start..cutoff].iter().map(message_token_count).sum();
    if moved <= slack_tokens {
        (start, false)
    } else {
        (cutoff, true)
    }
}

/// Rolling window size for token history (proactive/semantic modes)
pub const TOKEN_HISTORY_WINDOW: usize = 20;

/// Maximum characters to embed per message (first N chars capture semantic content)
pub const EMBED_MAX_CHARS_PER_MSG: usize = 512;

/// Rolling window of per-turn embeddings used for topic-shift detection
pub const EMBEDDING_HISTORY_WINDOW: usize = 10;

/// Per-manager semantic embedding cache capacity.
pub const SEMANTIC_EMBED_CACHE_CAPACITY: usize = 256;

pub const SUMMARY_PROMPT: &str = r#"Summarize our conversation so you can continue this work later.

Write in natural language with these sections:
- **Context:** What we're working on and why (1-2 sentences)
- **What we did:** Key actions taken, files changed, problems solved
- **Current state:** What works, what's broken, what's next
- **User preferences:** Specific requirements or decisions they made

Be concise but preserve important details. You can search the full conversation later if you need exact error messages or code snippets."#;

/// A completed summary covering turns up to a certain point
#[derive(Debug, Clone)]
pub struct Summary {
    pub text: String,
    pub openai_encrypted_content: Option<String>,
    /// Ordered provider-native window, replayed whole when non-empty.
    ///
    /// `/responses/compact` returns the canonical next context window: a
    /// compaction item plus the items the server retained. Only the ordered
    /// array can be replayed, so it is carried instead of being reduced to one
    /// `encrypted_content` string.
    pub native_window: Vec<serde_json::Value>,
    /// Route that produced `native_window`.
    ///
    /// Mirrors `StoredNativeRoute` in session-types; conversion happens where
    /// the state is persisted or restored. A native window is replayable only
    /// on the route that created it.
    pub native_route: Option<NativeRouteBinding>,
    pub covers_up_to_turn: usize,
    pub original_turn_count: usize,
}

/// Provider/model/route identity a native compaction window is bound to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeRouteBinding {
    pub provider: String,
    pub model: String,
    pub route: Option<String>,
}

impl NativeRouteBinding {
    /// Whether this binding describes the same route as the given one.
    ///
    /// A missing recorded route matches any current route; a recorded one must
    /// match exactly, so switching from OAuth to an API key on the same model is
    /// still a different route.
    pub fn matches(&self, provider: &str, model: &str, route: Option<&str>) -> bool {
        self.provider.eq_ignore_ascii_case(provider)
            && self.model == model
            && match (self.route.as_deref(), route) {
                (Some(bound), Some(current)) => bound == current,
                (Some(_), None) => false,
                (None, _) => true,
            }
    }
}

/// Event emitted when compaction is applied
#[derive(Debug, Clone)]
pub struct CompactionEvent {
    pub trigger: String,
    pub pre_tokens: Option<u64>,
    pub post_tokens: Option<u64>,
    pub tokens_saved: Option<u64>,
    pub duration_ms: Option<u64>,
    pub messages_dropped: Option<usize>,
    pub messages_compacted: Option<usize>,
    pub summary_chars: Option<usize>,
    pub active_messages: Option<usize>,
}

/// What happened when ensure_context_fits was called
#[derive(Debug, Clone, PartialEq)]
pub enum CompactionAction {
    /// Nothing needed, context is fine.
    None,
    /// Background summarization started.
    BackgroundStarted { trigger: String },
    /// Emergency hard compact performed. Contains number of messages dropped.
    HardCompacted(usize),
}

/// Stats about compaction state
#[derive(Debug, Clone)]
pub struct CompactionStats {
    pub total_turns: usize,
    pub active_messages: usize,
    pub has_summary: bool,
    pub is_compacting: bool,
    pub token_estimate: usize,
    pub effective_tokens: usize,
    pub observed_input_tokens: Option<u64>,
    pub context_usage: f32,
}

pub fn compacted_summary_text_block(summary: &str) -> String {
    format!("## Previous Conversation Summary\n\n{}\n\n---\n\n", summary)
}

pub fn build_compaction_prompt(
    messages: &[Message],
    existing_summary: Option<&Summary>,
    max_prompt_chars: usize,
) -> String {
    let mut conversation_text = build_compaction_conversation_text(messages, existing_summary);
    let overhead = SUMMARY_PROMPT.len() + 50;
    if conversation_text.len() + overhead > max_prompt_chars && max_prompt_chars > overhead {
        let budget = max_prompt_chars - overhead;
        conversation_text = truncate_str_boundary(&conversation_text, budget).to_string();
        conversation_text.push_str("\n\n... [earlier conversation truncated to fit context window]\n");
    }
    format!("{}\n\n---\n\n{}", conversation_text, SUMMARY_PROMPT)
}

pub fn build_compaction_conversation_text(messages: &[Message], existing_summary: Option<&Summary>) -> String {
    let mut conversation_text = String::new();
    if let Some(summary) = existing_summary {
        conversation_text.push_str("## Previous Summary\n\n");
        conversation_text.push_str(&summary.text);
        conversation_text.push_str("\n\n## New Conversation\n\n");
    }

    for msg in messages {
        let role_str = match msg.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
        };
        conversation_text.push_str(&format!("**{}:**\n", role_str));
        // Provider-native search call inputs, so a result can name its query.
        let mut native_call_inputs: std::collections::HashMap<String, serde_json::Value> =
            std::collections::HashMap::new();
        for block in &msg.content {
            match block {
                ContentBlock::ProviderNative { provider, item } => {
                    use horon_agent_message_types::provider_native;
                    let Some(display) = provider_native::provider_native_display(provider, item) else {
                        continue;
                    };
                    if display.output.is_none() {
                        if let Some(input) = display.input {
                            native_call_inputs.insert(display.id, input);
                        }
                        continue;
                    }
                    if let Some(text) = provider_native::provider_native_text_fallback(
                        provider,
                        item,
                        native_call_inputs.get(&display.id),
                    ) {
                        let truncated = if text.len() > 500 {
                            format!("{}... (truncated)", truncate_str_boundary(&text, 500))
                        } else {
                            text
                        };
                        conversation_text.push_str(&format!("[Result: {}]\n", truncated));
                    }
                }
                ContentBlock::Text { text, .. } => {
                    conversation_text.push_str(text);
                    conversation_text.push('\n');
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    conversation_text.push_str(&format!("[Tool: {} - {}]\n", name, input));
                }
                ContentBlock::ToolResult { content, .. } => {
                    let truncated = if content.len() > 500 {
                        format!("{}... (truncated)", truncate_str_boundary(content, 500))
                    } else {
                        content.clone()
                    };
                    conversation_text.push_str(&format!("[Result: {}]\n", truncated));
                }
                ContentBlock::Reasoning { .. }
                | ContentBlock::ReasoningTrace { .. }
                | ContentBlock::AnthropicThinking { .. }
                | ContentBlock::OpenAIReasoning { .. }
                | ContentBlock::ToolReference { .. } => {}
                ContentBlock::Image { .. } => conversation_text.push_str("[Image]\n"),
                ContentBlock::Audio { .. } => conversation_text.push_str("[audio]\n"),
                ContentBlock::OpenAICompaction { .. } => conversation_text.push_str("[OpenAI native compaction]\n"),
            }
        }
        conversation_text.push('\n');
    }
    conversation_text
}

pub fn truncate_str_boundary(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes.min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub fn mean_embedding(embeddings: &[&Vec<f32>], dim: usize) -> Vec<f32> {
    let mut mean = vec![0f32; dim];
    for emb in embeddings {
        for (i, v) in emb.iter().enumerate() {
            if i < dim {
                mean[i] += v;
            }
        }
    }
    let n = embeddings.len().max(1) as f32;
    for v in &mut mean {
        *v /= n;
    }
    let norm: f32 = mean.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for v in &mut mean {
            *v /= norm;
        }
    }
    mean
}

/// Find a safe compaction cutoff that does not leave kept tool results without
/// their corresponding tool calls.
pub fn safe_compaction_cutoff(messages: &[Message], initial_cutoff: usize) -> usize {
    let mut cutoff = initial_cutoff.min(messages.len());

    // Track tool call/result ids in the kept portion.
    let mut available_tool_ids = HashSet::new();
    let mut missing_tool_ids = HashSet::new();

    for msg in &messages[cutoff..] {
        for block in &msg.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    available_tool_ids.insert(id.clone());
                    missing_tool_ids.remove(id);
                }
                ContentBlock::ToolResult { tool_use_id, .. } if !available_tool_ids.contains(tool_use_id) => {
                    missing_tool_ids.insert(tool_use_id.clone());
                }
                _ => {}
            }
        }
    }

    if missing_tool_ids.is_empty() {
        return cutoff;
    }

    // Walk backward once, progressively growing the kept suffix until every
    // kept tool result has its matching tool use in the same suffix.
    for (idx, msg) in messages[..cutoff].iter().enumerate().rev() {
        for block in &msg.content {
            match block {
                ContentBlock::ToolUse { id, .. } => {
                    available_tool_ids.insert(id.clone());
                    missing_tool_ids.remove(id);
                }
                ContentBlock::ToolResult { tool_use_id, .. } if !available_tool_ids.contains(tool_use_id) => {
                    missing_tool_ids.insert(tool_use_id.clone());
                }
                _ => {}
            }
        }
        if missing_tool_ids.is_empty() {
            cutoff = idx;
            return cutoff;
        }
    }

    // If we couldn't find every matching tool call, don't compact at all.
    0
}

pub fn message_char_count(msg: &Message) -> usize {
    content_char_count(&msg.content)
}

/// Token cost of one message: the script-aware price of its content plus one
/// token of per-message framing.
///
/// This is the unit the recent-tail cutoff, the projection budget and the
/// compaction estimate count in. [`message_char_count`] still exists for
/// callers that report sizes in characters; the two are never converted into
/// each other.
pub fn message_token_count(msg: &Message) -> usize {
    message_token_count_for_family(msg, TokenFamily::O200k)
}

/// [`message_token_count`] priced for `family`.
///
/// The pipeline estimates for o200k, the vocabulary current models use. cl100k
/// is exposed because it spends close to one token per Hangul syllable: a
/// caller that knows its provider is Claude-family can charge the same message
/// higher instead of inheriting the o200k number.
pub fn message_token_count_for_family(msg: &Message, family: TokenFamily) -> usize {
    content_token_count_for_family(&msg.content, family) + 1
}

/// Token cost of a message's content blocks, without the per-message framing.
///
/// Text is priced by the content-aware estimator, which reads the script mix.
/// An image keeps its flat [`IMAGE_TOKEN_COST`], because providers bill it by
/// resolution rather than by the length of its base64 payload.
pub fn content_token_count(content: &[ContentBlock]) -> usize {
    content_token_count_for_family(content, TokenFamily::O200k)
}

fn content_token_count_for_family(content: &[ContentBlock], family: TokenFamily) -> usize {
    content.iter().map(|block| block_token_count(block, family)).sum()
}

fn block_token_count(block: &ContentBlock, family: TokenFamily) -> usize {
    match block {
        ContentBlock::Text { text, .. } => estimate_tokens_for_family(text, family),
        ContentBlock::Reasoning { text } => estimate_tokens_for_family(text, family),
        ContentBlock::ReasoningTrace { text } => estimate_tokens_for_family(text, family),
        ContentBlock::AnthropicThinking { thinking, signature } => {
            estimate_tokens_for_family(thinking, family) + estimate_tokens_for_family(signature, family)
        }
        ContentBlock::OpenAIReasoning {
            id,
            summary,
            encrypted_content,
            status,
        } => {
            estimate_tokens_for_family(id, family)
                + summary
                    .iter()
                    .map(|part| estimate_tokens_for_family(part, family))
                    .sum::<usize>()
                + encrypted_content
                    .as_deref()
                    .map(|text| estimate_tokens_for_family(text, family))
                    .unwrap_or(0)
                + status
                    .as_deref()
                    .map(|text| estimate_tokens_for_family(text, family))
                    .unwrap_or(0)
        }
        ContentBlock::ToolUse { input, .. } => estimate_tokens_for_family(&input.to_string(), family),
        ContentBlock::ToolResult { content, .. } => estimate_tokens_for_family(content, family),
        // An image or audio clip is charged by attachment, not by transport
        // length, so both keep a flat cost that the estimator cannot move.
        // See IMAGE_TOKEN_COST.
        ContentBlock::Image { .. } | ContentBlock::Audio { .. } => IMAGE_TOKEN_COST,
        ContentBlock::OpenAICompaction { encrypted_content, .. } => {
            estimate_tokens_for_family(encrypted_content, family)
        }
        ContentBlock::ToolReference { tool_name, .. } => estimate_tokens_for_family(tool_name, family),
        ContentBlock::ProviderNative { item, .. } => estimate_tokens_for_family(&item.to_string(), family),
    }
}

pub fn content_char_count(content: &[ContentBlock]) -> usize {
    content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text, .. } => text.len(),
            ContentBlock::Reasoning { text } => text.len(),
            ContentBlock::ReasoningTrace { text } => text.len(),
            ContentBlock::AnthropicThinking { thinking, signature } => thinking.len() + signature.len(),
            ContentBlock::OpenAIReasoning {
                id,
                summary,
                encrypted_content,
                status,
            } => {
                id.len()
                    + summary.iter().map(String::len).sum::<usize>()
                    + encrypted_content.as_ref().map(String::len).unwrap_or(0)
                    + status.as_ref().map(String::len).unwrap_or(0)
            }
            ContentBlock::ToolUse { input, .. } => input.to_string().len() + 50,
            ContentBlock::ToolResult { content, .. } => content.len() + 20,
            // Charge a flat token cost for images instead of the raw base64
            // payload length. See IMAGE_TOKEN_COST: counting base64 length here
            // overestimates context by ~100x and triggers spurious repeated
            // compactions.
            ContentBlock::Image { .. } | ContentBlock::Audio { .. } => IMAGE_TOKEN_COST * CHARS_PER_TOKEN_UPPER_BOUND,
            ContentBlock::OpenAICompaction { encrypted_content, .. } => encrypted_content.len(),
            // The provider expands a reference into the full definition, but
            // that definition is already accounted for by the tool catalog.
            ContentBlock::ToolReference { tool_name, .. } => tool_name.len() + 20,
            // Search results are billed as input tokens on replay; the raw JSON
            // (mostly encrypted page content) is a reasonable size proxy.
            ContentBlock::ProviderNative { item, .. } => item.to_string().len(),
        })
        .sum()
}

pub fn summary_payload_char_count(summary: &Summary) -> usize {
    if !summary.native_window.is_empty() {
        // A retained window is replayed item by item; its JSON is the payload
        // the provider charges for, not the (empty) human summary.
        return summary.native_window.iter().map(json_len).sum();
    }
    summary
        .openai_encrypted_content
        .as_ref()
        .map(|value| value.len())
        .unwrap_or_else(|| summary.text.len())
}

fn json_len(value: &serde_json::Value) -> usize {
    value.to_string().len()
}

/// Token cost of raw text, priced by the estimator the pipeline measures with.
///
/// Callers holding text without a [`Message`] or [`Summary`] wrapper — a summary
/// being assembled, a prompt fragment — use this instead of dividing a byte
/// count by four.
pub fn estimate_text_tokens(text: &str) -> usize {
    estimate_tokens_by_script(text)
}

/// Tokens the summary occupies when it is replayed into the request.
pub fn summary_payload_token_count(summary: &Summary) -> usize {
    if !summary.native_window.is_empty() {
        // A retained window is replayed item by item; its JSON is the payload
        // the provider charges for, not the (empty) human summary.
        return summary
            .native_window
            .iter()
            .map(|item| estimate_text_tokens(&item.to_string()))
            .sum();
    }
    match summary.openai_encrypted_content.as_deref() {
        Some(value) => estimate_text_tokens(value),
        None => estimate_text_tokens(&summary.text),
    }
}

pub fn estimate_compaction_tokens(
    summary: Option<&Summary>,
    active_message_chars: usize,
    token_budget: usize,
) -> usize {
    let summary_chars = summary.map(summary_payload_char_count).unwrap_or(0);
    estimate_compaction_tokens_from_chars(summary_chars + active_message_chars, token_budget)
}

/// Best-effort context size (tokens) from a provider usage report.
///
/// Providers disagree on what `input_tokens` means:
/// - **Split accounting** (Anthropic-style): `input_tokens` is only the
///   *uncached* remainder; cache reads/writes are separate counters, so the
///   real context size is `input + cache_read + cache_creation`.
/// - **Subset accounting** (OpenAI-style): `input_tokens` (`prompt_tokens`)
///   already includes cached tokens; `cached_tokens` is a subset and must NOT
///   be added again.
///
/// This is the single source of truth for that heuristic. Both the sidebar
/// context figure and the compaction manager's observed-token feed must use it
/// so the two never disagree (issue #441). When in doubt, avoid over-counting
/// unless there is strong evidence of split accounting.
pub fn effective_context_tokens_from_usage(
    provider_name: &str,
    input_tokens: u64,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
) -> u64 {
    effective_context_tokens_from_usage_with(
        provider_name,
        None,
        input_tokens,
        cache_read_input_tokens,
        cache_creation_input_tokens,
    )
}

/// The same, with the backend stating whether its usage is inclusive.
///
/// `inclusive` is `Some(true)` for a backend whose reported input already contains the
/// cached slices, `Some(false)` for one that reports the uncached remainder, and `None`
/// when the class does not say and the name-based heuristic must decide.
pub fn effective_context_tokens_from_usage_with(
    provider_name: &str,
    inclusive: Option<bool>,
    input_tokens: u64,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
) -> u64 {
    let cache_read = cache_read_input_tokens.unwrap_or(0);
    let cache_creation = cache_creation_input_tokens.unwrap_or(0);
    let provider_name = provider_name.to_lowercase();

    // A backend that states its own accounting is believed over the name: a plugin
    // backend carries the Responses shape under a plugin id the name list cannot know.
    if inclusive == Some(true) {
        return input_tokens;
    }

    // Subset accounting: the reported input already contains the cached slices, so
    // adding them again double counts. A write is not a split signal here either; these
    // providers report writes inside the input just like reads.
    const SUBSET_PROVIDERS: &[&str] = &[
        "openai",
        "codex",
        "deepseek",
        "xai",
        "grok",
        "kimi",
        "qwen",
        "openrouter",
        "command-code",
        "nous",
        "github-copilot",
    ];
    if SUBSET_PROVIDERS.iter().any(|name| provider_name.contains(name)) {
        return input_tokens;
    }

    let split_cache_accounting = provider_name.contains("anthropic")
        || provider_name.contains("claude")
        || cache_creation > 0
        || cache_read > input_tokens;

    if split_cache_accounting {
        input_tokens.saturating_add(cache_read).saturating_add(cache_creation)
    } else {
        input_tokens
    }
}

/// Context size in tokens for a summary plus the messages that stay verbatim.
///
/// This is the pipeline's own measurement: every message is priced by
/// [`message_token_count`], the summary by the payload it replays, and the
/// system prompt with the tool catalog by [`SYSTEM_OVERHEAD_TOKENS`].
pub fn estimate_compaction_tokens_from_text(
    summary: Option<&Summary>,
    messages: &[Message],
    token_budget: usize,
) -> usize {
    let summary_tokens = summary.map(summary_payload_token_count).unwrap_or(0);
    let message_tokens: usize = messages.iter().map(message_token_count).sum();
    estimate_compaction_tokens_from_parts(summary_tokens, message_tokens, token_budget)
}

/// Context size in tokens from parts that were already measured.
///
/// For a caller that holds a summary as text rather than as a [`Summary`], or
/// that has already summed its messages: the same overhead rule applies, so the
/// entry points never disagree about the non-message part of a request.
pub fn estimate_compaction_tokens_from_parts(
    summary_tokens: usize,
    message_tokens: usize,
    token_budget: usize,
) -> usize {
    summary_tokens + message_tokens + system_overhead_tokens(token_budget)
}

/// Legacy character-count entry point.
///
/// A character count carries no script information, so this path keeps dividing
/// by [`CHARS_PER_TOKEN_UPPER_BOUND`] and undercounts Korean the way the whole
/// pipeline used to. It stays for callers that hold only a count; anything
/// holding the messages calls [`estimate_compaction_tokens_from_text`].
pub fn estimate_compaction_tokens_from_chars(total_chars: usize, token_budget: usize) -> usize {
    total_chars / CHARS_PER_TOKEN_UPPER_BOUND + system_overhead_tokens(token_budget)
}

/// Overhead the system prompt and tool definitions add to every request.
///
/// They are not in the message list but do count toward the context limit. The
/// overhead is scaled to the budget so tests with tiny budgets are not dominated
/// by a constant they never send.
fn system_overhead_tokens(token_budget: usize) -> usize {
    if token_budget >= DEFAULT_TOKEN_BUDGET / 2 {
        SYSTEM_OVERHEAD_TOKENS
    } else {
        0
    }
}

pub fn semantic_goal_text(messages: &[Message]) -> String {
    let mut text = String::new();
    for msg in messages {
        for block in &msg.content {
            match block {
                ContentBlock::Text { text: block_text, .. } => push_semantic_excerpt(&mut text, block_text, 200),
                ContentBlock::ToolResult { content, .. } => push_semantic_excerpt(&mut text, content, 100),
                _ => {}
            }
        }
    }
    text
}

pub fn semantic_message_text(msg: &Message) -> String {
    let mut text = String::new();
    for block in &msg.content {
        if let ContentBlock::Text { text: block_text, .. } = block {
            push_semantic_excerpt(&mut text, block_text, EMBED_MAX_CHARS_PER_MSG);
        }
    }
    text
}

pub fn push_semantic_excerpt(target: &mut String, source: &str, max_chars: usize) {
    if source.is_empty() {
        return;
    }
    if !target.is_empty() {
        target.push(' ');
    }
    target.extend(source.chars().take(max_chars));
}

pub fn semantic_cache_key(text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

pub fn build_emergency_summary_text(
    existing_summary: Option<&str>,
    dropped_count: usize,
    pre_tokens: u64,
    token_budget: usize,
    dropped_messages: &[Message],
) -> String {
    let mut summary_parts: Vec<String> = Vec::new();

    if let Some(existing) = existing_summary
        && !existing.is_empty()
    {
        summary_parts.push(existing.to_string());
    }

    summary_parts.push(format!(
        "**[Emergency compaction]**: {} messages were dropped to recover from context overflow. \
         The conversation had ~{}k tokens which exceeded the {}k limit.",
        dropped_count,
        pre_tokens / 1000,
        token_budget / 1000,
    ));

    let mut file_mentions = Vec::new();
    let mut tool_names = HashSet::new();
    for msg in dropped_messages {
        collect_emergency_summary_hints(msg, &mut tool_names, &mut file_mentions);
    }

    if !tool_names.is_empty() {
        let mut tools: Vec<_> = tool_names.into_iter().collect();
        tools.sort();
        summary_parts.push(format!("Tools used: {}", tools.join(", ")));
    }

    file_mentions.sort();
    file_mentions.dedup();
    if !file_mentions.is_empty() {
        file_mentions.truncate(30);
        summary_parts.push(format!("Files referenced: {}", file_mentions.join(", ")));
    }

    summary_parts.join("\n\n")
}

fn collect_emergency_summary_hints(msg: &Message, tool_names: &mut HashSet<String>, file_mentions: &mut Vec<String>) {
    for block in &msg.content {
        match block {
            ContentBlock::ToolUse { name, .. } => {
                tool_names.insert(name.clone());
            }
            ContentBlock::Text { text, .. } => {
                extract_file_mentions(text, file_mentions);
            }
            _ => {}
        }
    }
}

pub fn extract_file_mentions(text: &str, file_mentions: &mut Vec<String>) {
    for word in text.split_whitespace() {
        if looks_like_file_reference(word) {
            let cleaned = clean_file_reference(word);
            if !cleaned.is_empty() {
                file_mentions.push(cleaned.to_string());
            }
        }
    }
}

pub fn looks_like_file_reference(word: &str) -> bool {
    (word.contains('/') || word.contains('.'))
        && word.len() > 3
        && word.len() < 120
        && !word.starts_with("http")
        && (word.contains(".rs")
            || word.contains(".ts")
            || word.contains(".py")
            || word.contains(".toml")
            || word.contains(".json")
            || word.starts_with("src/")
            || word.starts_with("./"))
}

pub fn clean_file_reference(word: &str) -> &str {
    word.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '.' && c != '_' && c != '-')
}

/// Whether a provider error is a deterministic rejection of an inline image
/// (a 400 `invalid_request_error` naming an image block: unsupported
/// `media_type`, undecodable data, dimension or animation limits). The same
/// history would be rejected on every retry, so the agent strips images and
/// retries once instead of leaving the session permanently broken (#1712).
pub fn is_image_rejection_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    if is_request_payload_too_large_error(&lower) {
        return false;
    }
    let names_image_block = lower.contains(".image.source")
        || lower.contains(".image.")
        || lower.contains("image.source")
        || lower.contains("could not process image")
        || lower.contains("invalid image")
        || lower.contains("unsupported image")
        || lower.contains("image_parse_error")
        || lower.contains("invalid_image");
    let is_client_rejection = lower.contains("invalid_request_error")
        || lower.contains("invalid_image")
        || lower.contains("image_parse_error")
        || contains_independent_status_code(&lower, "400");
    names_image_block && is_client_rejection
}

/// Replace every inline image with a short text note. Used after a provider
/// rejected an image so the retried request carries none. Returns the number
/// of images replaced.
pub fn strip_all_images_in_contents(contents: &mut [&mut Vec<ContentBlock>]) -> usize {
    let mut stripped = 0;
    for content in contents.iter_mut() {
        for block in content.iter_mut() {
            let note = match block {
                ContentBlock::Image { media_type, .. } => format!(
                    "[Image omitted: the provider rejected an image in this conversation (media_type={media_type}). Re-open or re-screenshot it in PNG/JPEG if visual details are needed.]"
                ),
                ContentBlock::Audio { format, .. } => format!(
                    "[Audio omitted: the provider rejected an audio clip in this conversation (format={format}). Record or attach it again if it is needed.]"
                ),
                _ => continue,
            };
            *block = ContentBlock::Text {
                text: note,
                cache_control: None,
            };
            stripped += 1;
        }
    }
    stripped
}

/// Whether a provider error indicates the *serialized request body* was too
/// large (HTTP 413), as distinct from exceeding the model's token context
/// window. Anthropic surfaces this as `request_too_large` / "Request exceeds the
/// maximum size" / "413 Payload Too Large"; OpenAI and gateways use similar
/// wording. This failure mode is dominated by inline base64 images, which the
/// token-budget accounting deliberately undercounts, so it needs a dedicated
/// byte-size recovery rather than ordinary context compaction.
pub fn is_request_payload_too_large_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("request_too_large")
        || lower.contains("request too large")
        || lower.contains("payload too large")
        || lower.contains("request entity too large")
        || lower.contains("request exceeds the maximum size")
        || lower.contains("exceeds the maximum size")
        || contains_independent_status_code(&lower, "413")
}

/// Whether `haystack` contains `code` as a standalone status code rather than as
/// a fragment of a longer number (so "413" matches but "4130"/"version 4131"
/// does not). Mirrors the failover classifier's guard.
fn contains_independent_status_code(haystack: &str, code: &str) -> bool {
    let bytes = haystack.as_bytes();
    haystack.match_indices(code).any(|(start, _)| {
        let before_ok = start == 0 || !bytes[start - 1].is_ascii_digit();
        let end = start + code.len();
        let after_ok = end == bytes.len() || !bytes[end].is_ascii_digit();
        before_ok && after_ok
    })
}

/// Strip oversized inline images from `messages`, oldest-first, until the total
/// remaining base64 image payload fits within `target_total_chars`.
///
/// Unlike [`emergency_truncate_large_payloads`] (which is driven by the token
/// budget and replaces *every* image past a tiny per-image cap), this is the
/// byte-size recovery path for HTTP 413 "request too large" errors: it preserves
/// as many of the most recent images as the request size budget allows and only
/// drops the older ones. Each stripped image is replaced with a text marker so
/// the model still knows an image existed and where to recover it from.
///
/// Returns the number of images that were replaced with text markers.
pub fn emergency_strip_large_images(messages: &mut [Message], target_total_chars: usize) -> usize {
    let mut contents: Vec<&mut Vec<ContentBlock>> = messages.iter_mut().map(|m| &mut m.content).collect();
    strip_large_images_in_contents(&mut contents, target_total_chars)
}

/// Core of [`emergency_strip_large_images`], operating directly on a slice of
/// content-block vectors so it can be reused for both provider `Message`s and
/// the session's stored-message representation (which share `ContentBlock`).
pub fn strip_large_images_in_contents(contents: &mut [&mut Vec<ContentBlock>], target_total_chars: usize) -> usize {
    // Collect (content_index, block_index, payload_len) for every inline image
    // or audio clip, in transcript order (oldest first).
    let mut images: Vec<(usize, usize, usize)> = Vec::new();
    let mut total: usize = 0;
    for (ci, content) in contents.iter().enumerate() {
        for (bi, block) in content.iter().enumerate() {
            if let Some(len) = inline_attachment_len(block) {
                images.push((ci, bi, len));
                total = total.saturating_add(len);
            }
        }
    }

    if total <= target_total_chars {
        return 0;
    }

    let mut stripped = 0;
    // Drop oldest images first until we're under budget (always keep trying even
    // if a single huge recent image alone exceeds the budget — better to ship a
    // request the provider might still trim than to give up entirely).
    for (ci, bi, payload_len) in images {
        if total <= target_total_chars {
            break;
        }
        let block = &mut contents[ci][bi];
        let note = match block {
            ContentBlock::Image { media_type, .. } => format!(
                "[Image omitted during request-size recovery: media_type={media_type}, original_base64_chars={payload_len}. The request body exceeded the provider size limit; older images were dropped. Rely on adjacent browser/tool text, screenshots saved to disk, or re-open/re-screenshot if visual details are needed.]"
            ),
            ContentBlock::Audio { format, .. } => format!(
                "[Audio omitted during request-size recovery: format={format}, original_base64_chars={payload_len}. The request body exceeded the provider size limit; older audio clips were dropped.]"
            ),
            _ => continue,
        };
        *block = ContentBlock::Text {
            text: note,
            cache_control: None,
        };
        total = total.saturating_sub(payload_len);
        stripped += 1;
    }

    stripped
}

/// Inline attachment payload length: images and audio clips both live in the
/// request body as base64 and are trimmed together when the body is too large.
fn inline_attachment_len(block: &ContentBlock) -> Option<usize> {
    match block {
        ContentBlock::Image { data, .. } | ContentBlock::Audio { data, .. } => Some(data.len()),
        _ => None,
    }
}

/// Inline tool-result images (`read`) are decayed once the transcript holds
/// more than this many of them. See
/// [`decay_old_tool_images_in_contents`].
pub const TOOL_IMAGE_DECAY_TRIGGER_COUNT: usize = 12;

/// Inline tool-result images are also decayed once their combined base64
/// payload exceeds this many characters.
pub const TOOL_IMAGE_DECAY_TRIGGER_CHARS: usize = 48 * 1024 * 1024;

/// After decay, at most this many of the newest tool-result images keep their
/// inline data. The gap to the trigger count is hysteresis: decay rewrites
/// older messages (invalidating provider prompt caches and forcing a full
/// snapshot write), so it only runs every few images instead of on each.
pub const TOOL_IMAGE_DECAY_KEEP_COUNT: usize = 6;

/// After decay, the retained tool-result images fit within this many base64
/// characters, except that the newest image is always kept regardless of size.
pub const TOOL_IMAGE_DECAY_KEEP_CHARS: usize = 24 * 1024 * 1024;

/// Bound the inline image payload that tool results (images returned by
/// `read`) accumulate in a transcript.
///
/// Only images in messages that carry a `ToolResult` are considered, so images
/// the user pasted are untouched. Nothing happens until the tool images exceed
/// [`TOOL_IMAGE_DECAY_TRIGGER_COUNT`] or [`TOOL_IMAGE_DECAY_TRIGGER_CHARS`].
/// Then every tool image except the newest [`TOOL_IMAGE_DECAY_KEEP_COUNT`]
/// (that also fit within [`TOOL_IMAGE_DECAY_KEEP_CHARS`]) is replaced with a
/// short text marker. The newest image always survives so the model still sees
/// the image it just asked for.
///
/// Returns the number of images replaced.
pub fn decay_old_tool_images_in_contents(contents: &mut [&mut Vec<ContentBlock>]) -> usize {
    let mut images: Vec<(usize, usize, usize)> = Vec::new();
    let mut total: usize = 0;
    for (ci, content) in contents.iter().enumerate() {
        if !content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
        {
            continue;
        }
        for (bi, block) in content.iter().enumerate() {
            if let ContentBlock::Image { data, .. } = block {
                images.push((ci, bi, data.len()));
                total = total.saturating_add(data.len());
            }
        }
    }

    if images.len() <= TOOL_IMAGE_DECAY_TRIGGER_COUNT && total <= TOOL_IMAGE_DECAY_TRIGGER_CHARS {
        return 0;
    }

    let mut kept_count = 0usize;
    let mut kept_chars = 0usize;
    let mut stripped = 0usize;
    for (ci, bi, payload_len) in images.into_iter().rev() {
        let keep = kept_count == 0
            || (kept_count < TOOL_IMAGE_DECAY_KEEP_COUNT
                && kept_chars.saturating_add(payload_len) <= TOOL_IMAGE_DECAY_KEEP_CHARS);
        if keep {
            kept_count += 1;
            kept_chars = kept_chars.saturating_add(payload_len);
            continue;
        }
        let block = &mut contents[ci][bi];
        if let ContentBlock::Image { media_type, data } = block {
            let text = format!(
                "[Older tool screenshot omitted from history to bound transcript size: media_type={media_type}, original_base64_chars={}. Rely on adjacent tool text, or take a new screenshot if visual details are needed.]",
                data.len()
            );
            *block = ContentBlock::Text {
                text,
                cache_control: None,
            };
            stripped += 1;
        }
    }
    stripped
}

mod emergency_truncate;
pub use emergency_truncate::{
    EMERGENCY_SIGNAL_LINE_MAX_BYTES, EMERGENCY_SIGNAL_MAX_LINES, EMERGENCY_SIGNAL_TOTAL_MAX_BYTES,
    emergency_truncate_large_payloads, emergency_truncate_tool_results, emergency_truncated_tool_result,
    tail_str_boundary,
};

#[cfg(test)]
#[path = "recovery_band_tests.rs"]
mod recovery_band_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_text_keeps_native_search_query_and_urls() {
        let messages = vec![Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ProviderNative {
                    provider: "anthropic".to_string(),
                    item: serde_json::json!({"type": "server_tool_use", "id": "srvtoolu_1",
                        "name": "web_search", "input": {"query": "rust news"}}),
                },
                ContentBlock::ProviderNative {
                    provider: "anthropic".to_string(),
                    item: serde_json::json!({"type": "web_search_tool_result",
                        "tool_use_id": "srvtoolu_1", "content": [{"type": "web_search_result",
                        "url": "https://blog.rust-lang.org/", "title": "Rust Blog",
                        "encrypted_content": "ENC"}]}),
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        }];
        let text = build_compaction_conversation_text(&messages, None);
        assert!(text.contains("rust news"), "{text}");
        assert!(text.contains("https://blog.rust-lang.org/"), "{text}");
        assert!(!text.contains("ENC"), "{text}");
    }

    #[test]
    fn effective_context_split_accounting_adds_cache_counters() {
        // Anthropic-style: input is the uncached remainder.
        assert_eq!(
            effective_context_tokens_from_usage("anthropic", 10_000, Some(300_000), Some(5_000)),
            315_000
        );
        assert_eq!(
            effective_context_tokens_from_usage("Claude", 10_000, Some(300_000), None),
            310_000
        );
    }

    #[test]
    fn effective_context_subset_accounting_does_not_double_count() {
        // OpenAI-style: prompt_tokens already includes cached tokens.
        assert_eq!(
            effective_context_tokens_from_usage("openai", 400_000, Some(390_000), None),
            400_000
        );
        assert_eq!(
            effective_context_tokens_from_usage("openai-api", 10_000, Some(6_000), Some(2_000)),
            10_000
        );
        // No cache info at all: pass through.
        assert_eq!(
            effective_context_tokens_from_usage("opencode-go", 396_000, None, None),
            396_000
        );
    }

    #[test]
    fn a_backend_that_states_inclusive_usage_is_believed_over_its_name() {
        // A plugin backend reports the Responses shape under a plugin id the name list
        // cannot recognise: 1500 input already contains the 1000 read and 400 write.
        assert_eq!(
            effective_context_tokens_from_usage_with("router", Some(true), 1500, Some(1000), Some(400)),
            1500
        );
        // Without the statement the same numbers would be added together.
        assert_eq!(
            effective_context_tokens_from_usage_with("router", None, 1500, Some(1000), Some(400)),
            2900
        );
    }

    #[test]
    fn subset_providers_keep_writes_inside_the_input() {
        // A write is reported inside the input on these providers; adding it again would
        // overstate the context the same way adding a read would.
        for provider in ["openai", "codex", "deepseek", "xai", "kimi", "openrouter"] {
            assert_eq!(
                effective_context_tokens_from_usage(provider, 10_000, Some(6_000), Some(2_000)),
                10_000,
                "{provider}"
            );
        }
    }

    #[test]
    fn effective_context_infers_split_accounting_from_counter_shape() {
        // cache_read > input implies input can't already contain it.
        assert_eq!(
            effective_context_tokens_from_usage("unknown", 10_000, Some(500_000), None),
            510_000
        );
        // Any cache_creation implies split accounting.
        assert_eq!(
            effective_context_tokens_from_usage("unknown", 10_000, Some(2_000), Some(1_000)),
            13_000
        );
    }

    #[test]
    fn effective_context_zero_uncached_input_preserves_cached_prompt() {
        assert_eq!(
            effective_context_tokens_from_usage("anthropic", 0, Some(300_000), Some(5_000)),
            305_000
        );
    }

    #[test]
    fn builds_compaction_prompt_with_summary_and_truncated_tool_result() {
        let summary = Summary {
            text: "prior work".to_string(),
            openai_encrypted_content: None,
            native_window: Vec::new(),
            native_route: None,
            covers_up_to_turn: 1,
            original_turn_count: 1,
        };
        let message = Message::user("hello");
        let prompt = build_compaction_prompt(&[message], Some(&summary), 10_000);
        assert!(prompt.contains("## Previous Summary"));
        assert!(prompt.contains("prior work"));
        assert!(prompt.contains("**User:**"));
        assert!(prompt.contains(SUMMARY_PROMPT));
    }

    #[test]
    fn truncates_on_utf8_boundary() {
        assert_eq!(truncate_str_boundary("éabc", 1), "");
        assert_eq!(truncate_str_boundary("éabc", 2), "é");
    }

    #[test]
    fn mean_embedding_is_normalized() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let mean = mean_embedding(&[&a, &b], 2);
        let norm = (mean[0] * mean[0] + mean[1] * mean[1]).sqrt();
        assert!((norm - 1.0).abs() < 0.0001);
    }

    #[test]
    fn safe_cutoff_keeps_tool_use_with_tool_result() {
        let tool_use = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "read".to_string(),
                input: serde_json::json!({"file":"src/lib.rs"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        };
        let tool_result = Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: "ok".to_string(),
                is_error: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        };
        let messages = vec![Message::user("old"), tool_use, tool_result, Message::user("new")];

        assert_eq!(safe_compaction_cutoff(&messages, 2), 1);
    }

    #[test]
    fn estimates_tokens_with_large_budget_overhead() {
        let summary = Summary {
            text: "abcd".repeat(100),
            openai_encrypted_content: None,
            native_window: Vec::new(),
            native_route: None,
            covers_up_to_turn: 1,
            original_turn_count: 1,
        };

        assert_eq!(estimate_compaction_tokens(Some(&summary), 0, 1000), 100);
        assert_eq!(
            estimate_compaction_tokens(Some(&summary), 0, DEFAULT_TOKEN_BUDGET),
            100 + SYSTEM_OVERHEAD_TOKENS
        );
    }

    #[test]
    fn image_token_cost_is_bounded_not_base64_length() {
        // Regression: a large base64 image payload must not be counted as ~len/4
        // tokens. Doing so inflated the estimate ~100x and caused repeated
        // back-to-back compactions.
        let huge_base64 = "A".repeat(1_400_000);
        let mut image_msg = Message::user("");
        image_msg.content = vec![ContentBlock::Image {
            media_type: "image/png".to_string(),
            data: huge_base64.clone(),
        }];

        let chars = message_char_count(&image_msg);
        // Flat per-image cost in char-equivalents, far below the raw payload.
        assert_eq!(chars, IMAGE_TOKEN_COST * CHARS_PER_TOKEN_UPPER_BOUND);
        assert!(
            chars < huge_base64.len() / 10,
            "image should not be charged anywhere near its base64 length"
        );

        // The token estimate for four such images stays small.
        let tokens = estimate_compaction_tokens_from_chars(chars * 4, DEFAULT_TOKEN_BUDGET);
        assert!(
            tokens < SYSTEM_OVERHEAD_TOKENS + 4 * IMAGE_TOKEN_COST + 10,
            "four images should cost ~{} tokens, got {}",
            SYSTEM_OVERHEAD_TOKENS + 4 * IMAGE_TOKEN_COST,
            tokens
        );
    }

    #[test]
    fn audio_token_cost_matches_images() {
        // Audio is billed by the attachment, not by the base64 payload, so it
        // keeps the same flat cost an image does.
        let mut audio_msg = Message::user("");
        audio_msg.content = vec![ContentBlock::Audio {
            format: "wav".to_string(),
            data: "A".repeat(1_400_000),
        }];
        assert_eq!(
            message_char_count(&audio_msg),
            IMAGE_TOKEN_COST * CHARS_PER_TOKEN_UPPER_BOUND
        );
        assert_eq!(message_token_count(&audio_msg), IMAGE_TOKEN_COST + 1);
    }

    /// The Korean prose sample the estimator's anchors are measured against:
    /// 79 characters, 236 UTF-8 bytes, 44 real o200k tokens and 72 real cl100k
    /// tokens (tiktoken 0.14.0).
    const KOREAN_PROSE: &str = "에이전트가 파일을 읽고 편집한 뒤 테스트를 실행합니다. 이 과정에서 컨텍스트가 얼마나 남았는지 정확히 알아야 압축을 제때 시작할 수 있습니다.";

    /// A token budget still buys four characters per token, so the inverse
    /// direction did not move when the forward direction became script-aware.
    #[test]
    fn inverse_conversion_still_uses_the_upper_bound() {
        assert_eq!(CHARS_PER_TOKEN_UPPER_BOUND, 4);
        assert_eq!(20_000 * CHARS_PER_TOKEN_UPPER_BOUND, 80_000);

        // The one conversion that runs both ways on the same block: an image is
        // IMAGE_TOKEN_COST tokens and IMAGE_TOKEN_COST * 4 characters, so a
        // token budget and a character allowance agree on today's value.
        let mut image = Message::user("");
        image.content = vec![ContentBlock::Image {
            media_type: "image/png".to_string(),
            data: "A".repeat(64),
        }];
        assert_eq!(
            message_char_count(&image),
            IMAGE_TOKEN_COST * CHARS_PER_TOKEN_UPPER_BOUND
        );
        assert_eq!(message_token_count(&image), IMAGE_TOKEN_COST + 1);
    }

    /// The call sites that divide by the upper bound have to be driven, not just
    /// the constant's value.
    ///
    /// `inverse_conversion_still_uses_the_upper_bound` asserts the constant is 4
    /// and then does arithmetic with it, so a call site that stopped using the
    /// constant - dividing by 3, or by its own literal - would leave that test
    /// green. This drives the real entry points and compares each result against
    /// the constant-derived expectation, so a changed divisor fails.
    ///
    /// One case no test can catch: a call site that hardcodes a literal equal to
    /// the constant's current value is indistinguishable from using the constant,
    /// because they compute the same number. That is recorded rather than papered
    /// over.
    #[test]
    fn call_sites_honour_the_upper_bound_divisor() {
        // Chosen so every plausible wrong divisor gives a different answer:
        // /4 = 20_000, /3 = 26_666, /5 = 16_000.
        let chars = 80_000usize;
        assert_eq!(
            estimate_compaction_tokens_from_chars(chars, 0),
            chars / CHARS_PER_TOKEN_UPPER_BOUND,
            "the legacy character entry point must divide by the upper bound"
        );

        // The image leg converts a token cost into a character allowance with the
        // same constant, so a changed divisor moves this number too.
        let mut image = Message::user("");
        image.content = vec![ContentBlock::Image {
            media_type: "image/png".to_string(),
            data: "A".repeat(64),
        }];
        assert_eq!(
            message_char_count(&image),
            IMAGE_TOKEN_COST * CHARS_PER_TOKEN_UPPER_BOUND,
            "the image character allowance must be derived from the same constant"
        );

        // A budget large enough to carry the system overhead, so the assertion
        // covers the whole expression rather than only the division.
        let budget = DEFAULT_TOKEN_BUDGET;
        assert_eq!(
            estimate_compaction_tokens_from_chars(chars, budget),
            chars / CHARS_PER_TOKEN_UPPER_BOUND + SYSTEM_OVERHEAD_TOKENS,
            "the overhead term must not absorb a divisor change"
        );
    }

    /// The pipeline's measurement is the estimator's, not a second rule that
    /// drifts away from it.
    #[test]
    fn token_estimate_matches_the_new_estimator() {
        let messages = vec![
            Message::user(KOREAN_PROSE),
            Message::assistant_text("The fix is in `lib.rs`."),
        ];
        let expected: usize = messages.iter().map(message_token_count).sum();
        assert_eq!(estimate_compaction_tokens_from_text(None, &messages, 1_000), expected);

        // A single text block is priced exactly as the estimator prices its text,
        // plus the one token of per-message framing.
        assert_eq!(
            estimate_compaction_tokens_from_text(None, &[Message::user(KOREAN_PROSE)], 1_000),
            estimate_text_tokens(KOREAN_PROSE) + 1
        );
    }

    /// cl100k spends close to one token per Hangul syllable, so it is the family
    /// that undercounts Korean when it is priced with the o200k rate. The
    /// per-message count must carry the script cost instead of falling back to
    /// the byte length.
    #[test]
    fn korean_messages_are_not_underestimated_on_cl100k() {
        assert_eq!(KOREAN_PROSE.chars().count(), 79);
        let message = Message::user(KOREAN_PROSE);
        let o200k = content_token_count(&message.content);
        let cl100k = content_token_count_for_family(&message.content, TokenFamily::Cl100k);

        assert_eq!(o200k, estimate_text_tokens(KOREAN_PROSE));
        assert_eq!(cl100k, estimate_tokens_for_family(KOREAN_PROSE, TokenFamily::Cl100k));
        assert!(cl100k > o200k, "cl100k {cl100k} must exceed o200k {o200k}");
        assert!(
            cl100k as f64 >= 72.0 * 0.85,
            "cl100k {cl100k} is below the 85% floor of the measured 72 tokens"
        );

        // The same message the base policy test uses, so the two numbers can be
        // compared across crates: 1_200 Hangul syllables, 3_600 UTF-8 bytes.
        let hangul = Message::user(&"가".repeat(1_200));
        let bytes = message_char_count(&hangul);
        let transcript: Vec<Message> = (0..40).map(|_| hangul.clone()).collect();
        let allowance = 20_000;
        let byte_kept = transcript
            .iter()
            .rev()
            .scan(0_usize, |spent, message| {
                *spent += message_char_count(message) / CHARS_PER_TOKEN_UPPER_BOUND + 1;
                Some(*spent)
            })
            .take_while(|spent| *spent <= allowance)
            .count();
        let kept = transcript.len() - recent_cutoff_for_token_budget(&transcript, allowance, 0);
        let tail: usize = transcript[..5].iter().map(message_token_count).sum();
        println!(
            "korean message: 1_200 syllables, {bytes} utf8 bytes\n\
             site                                        before      after\n\
             recent_cutoff_for_token_budget (kept of 40) {:>7} {:>10}\n\
             estimate_compaction_tokens_from_text        {:>7} {:>10}\n\
             hard_compact remaining suffix (5 messages)  {:>7} {:>10}\n\
             source_projection tail_tokens (5 messages)  {:>7} {:>10}",
            byte_kept,
            kept,
            5 * (bytes / CHARS_PER_TOKEN_UPPER_BOUND),
            estimate_compaction_tokens_from_text(None, &transcript[..5], 1_000),
            5 * (bytes / CHARS_PER_TOKEN_UPPER_BOUND),
            tail,
            5 * (bytes / CHARS_PER_TOKEN_UPPER_BOUND + 1),
            tail,
        );
    }

    #[test]
    fn builds_semantic_text_from_relevant_content() {
        let message = Message {
            role: Role::User,
            content: vec![
                ContentBlock::Text {
                    text: "hello world".to_string(),
                    cache_control: None,
                },
                ContentBlock::ToolResult {
                    tool_use_id: "call_1".to_string(),
                    content: "tool output".to_string(),
                    is_error: None,
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        };

        assert_eq!(semantic_message_text(&message), "hello world");
        assert_eq!(semantic_goal_text(&[message]), "hello world tool output");
        assert_eq!(semantic_cache_key("stable"), semantic_cache_key("stable"));
    }

    #[test]
    fn builds_emergency_summary_with_tools_and_files() {
        let messages = vec![
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call_1".to_string(),
                    name: "read".to_string(),
                    input: serde_json::json!({"file":"src/lib.rs"}),
                    thought_signature: None,
                }],
                timestamp: None,
                tool_duration_ms: None,
            },
            Message::user("Edited src/compaction.rs and Cargo.toml, ignored https://example.com"),
        ];

        let summary = build_emergency_summary_text(Some("previous"), 2, 201_000, 200_000, &messages);
        assert!(summary.contains("previous"));
        assert!(summary.contains("2 messages were dropped"));
        assert!(summary.contains("Tools used: read"));
        assert!(summary.contains("Files referenced: Cargo.toml, src/compaction.rs"));
        assert!(!summary.contains("https://example.com"));
    }

    #[test]
    fn emergency_truncation_is_utf8_safe() {
        let original = format!("{}middle{}", "é".repeat(20), "尾".repeat(20));
        let truncated = emergency_truncated_tool_result(&original, 25, None);
        assert!(truncated.contains("chars truncated for context recovery"));
        assert!(!truncated.starts_with("[tool error] "));
        assert!(truncated.is_char_boundary(truncated.len()));
        let error = emergency_truncated_tool_result(&original, 25, Some(true));
        assert!(error.starts_with("[tool error] "));
    }

    #[test]
    fn emergency_truncation_replaces_large_images_with_text_marker() {
        let mut messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "a".repeat(2048),
            }],
            timestamp: None,
            tool_duration_ms: None,
        }];

        let truncated = emergency_truncate_large_payloads(&mut messages, 4000, 1024);
        assert_eq!(truncated, 1);
        match &messages[0].content[0] {
            ContentBlock::Text { text, .. } => {
                assert!(text.contains("Image omitted during emergency context recovery"));
                assert!(text.contains("original_base64_chars=2048"));
            }
            other => panic!("expected image to be replaced with text marker, got {other:?}"),
        }
    }

    #[test]
    fn detects_request_payload_too_large_errors() {
        // Image rejections are a different class.
        assert!(is_image_rejection_error(
            "Anthropic API error (400 Bad Request): {\"type\":\"error\",\"error\":{\"type\":\"invalid_request_error\",\"message\":\"messages.720.content.0.tool_result.content.1.image.source.base64.media_type: Input should be 'image/jpeg', 'image/png', 'image/gif' or 'image/webp'\"}}"
        ));
        assert!(is_image_rejection_error(
            "400 invalid_request_error: messages.3.content.1.image.source.base64: Could not process image"
        ));
        assert!(!is_image_rejection_error(
            "Anthropic API error (413 Payload Too Large): image too big request_too_large"
        ));
        assert!(!is_image_rejection_error(
            "400 invalid_request_error: messages.5: `tool_use` ids were found without `tool_result` blocks"
        ));
        assert!(!is_image_rejection_error("rate limit exceeded"));

        assert!(is_request_payload_too_large_error(
            "Anthropic API error (413 Payload Too Large): {\"error\":{\"type\":\"request_too_large\",\"message\":\"Request exceeds the maximum size\"}}"
        ));
        assert!(is_request_payload_too_large_error("413 Request Entity Too Large"));
        assert!(is_request_payload_too_large_error("request too large"));
        // Not a payload error — should not match.
        assert!(!is_request_payload_too_large_error(
            "rate limit exceeded, retry after 20s"
        ));
        // Embedded digits must not trip the standalone 413 check.
        assert!(!is_request_payload_too_large_error("model version 4130 is unavailable"));
    }

    fn image_msg(data_len: usize) -> Message {
        Message {
            role: Role::User,
            content: vec![ContentBlock::Image {
                media_type: "image/png".to_string(),
                data: "a".repeat(data_len),
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    #[test]
    fn strip_large_images_drops_oldest_until_under_budget() {
        // Four 1000-char images = 4000 total; budget 2500 should drop the two
        // oldest (leaving 2000 <= 2500), keeping the two most recent.
        let mut messages = vec![image_msg(1000), image_msg(1000), image_msg(1000), image_msg(1000)];

        let stripped = emergency_strip_large_images(&mut messages, 2500);
        assert_eq!(stripped, 2);
        // Oldest two replaced with text markers.
        assert!(matches!(messages[0].content[0], ContentBlock::Text { .. }));
        assert!(matches!(messages[1].content[0], ContentBlock::Text { .. }));
        // Most recent two preserved as images.
        assert!(matches!(messages[2].content[0], ContentBlock::Image { .. }));
        assert!(matches!(messages[3].content[0], ContentBlock::Image { .. }));
        if let ContentBlock::Text { text, .. } = &messages[0].content[0] {
            assert!(text.contains("Image omitted during request-size recovery"));
            assert!(text.contains("original_base64_chars=1000"));
        }
    }

    fn tool_image_msg(data_len: usize) -> Message {
        Message {
            role: Role::User,
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: "call".to_string(),
                    content: "Captured main display".to_string(),
                    is_error: None,
                },
                ContentBlock::Image {
                    media_type: "image/png".to_string(),
                    data: "a".repeat(data_len),
                },
            ],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    fn decay(messages: &mut [Message]) -> usize {
        let mut contents: Vec<&mut Vec<ContentBlock>> = messages.iter_mut().map(|m| &mut m.content).collect();
        decay_old_tool_images_in_contents(&mut contents)
    }

    fn is_image(message: &Message) -> bool {
        message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::Image { .. }))
    }

    #[test]
    fn tool_image_decay_waits_for_trigger_then_keeps_newest() {
        let mut messages: Vec<Message> = (0..TOOL_IMAGE_DECAY_TRIGGER_COUNT)
            .map(|_| tool_image_msg(10))
            .collect();
        assert_eq!(decay(&mut messages), 0, "at the trigger count nothing decays");

        messages.push(tool_image_msg(10));
        let total = messages.len();
        assert_eq!(decay(&mut messages), total - TOOL_IMAGE_DECAY_KEEP_COUNT);
        for (index, message) in messages.iter().enumerate() {
            assert_eq!(is_image(message), index >= total - TOOL_IMAGE_DECAY_KEEP_COUNT);
            // The tool_result itself is preserved for tool_use pairing.
            assert!(matches!(message.content[0], ContentBlock::ToolResult { .. }));
        }
        let ContentBlock::Text { text, .. } = &messages[0].content[1] else {
            panic!("decayed image must become a text marker");
        };
        assert!(text.contains("Older tool screenshot omitted"));
        assert!(text.contains("original_base64_chars=10"));
        // Stable after decay: nothing more to do until the trigger is hit again.
        assert_eq!(decay(&mut messages), 0);
    }

    #[test]
    fn tool_image_decay_bounds_bytes_but_keeps_newest_huge_image() {
        let huge = TOOL_IMAGE_DECAY_KEEP_CHARS + 1;
        let mut messages = vec![tool_image_msg(huge), tool_image_msg(huge), tool_image_msg(huge)];
        assert_eq!(decay(&mut messages), 2);
        assert!(!is_image(&messages[0]));
        assert!(!is_image(&messages[1]));
        assert!(is_image(&messages[2]), "newest screenshot must still be sent");
    }

    #[test]
    fn tool_image_decay_ignores_user_pasted_images() {
        let mut messages = vec![image_msg(10)];
        messages.extend((0..TOOL_IMAGE_DECAY_TRIGGER_COUNT + 5).map(|_| tool_image_msg(10)));
        decay(&mut messages);
        assert!(is_image(&messages[0]), "user images are not tool screenshots");
    }

    #[test]
    fn strip_large_images_noop_when_under_budget() {
        let mut messages = vec![image_msg(500), image_msg(500)];
        let stripped = emergency_strip_large_images(&mut messages, 4000);
        assert_eq!(stripped, 0);
        assert!(matches!(messages[0].content[0], ContentBlock::Image { .. }));
        assert!(matches!(messages[1].content[0], ContentBlock::Image { .. }));
    }

    #[test]
    fn strip_large_images_strips_all_when_single_image_exceeds_budget() {
        // Even a lone over-budget image is stripped (better than re-sending an
        // oversized request that the provider will reject again).
        let mut messages = vec![image_msg(8000)];
        let stripped = emergency_strip_large_images(&mut messages, 2000);
        assert_eq!(stripped, 1);
        assert!(matches!(messages[0].content[0], ContentBlock::Text { .. }));
    }
}

#[cfg(test)]
mod compaction_policy_tests {
    use super::*;

    fn budget(window: usize, reserve: usize, recent: usize) -> ContextBudget {
        compute_context_budget(window, reserve, recent, None).expect("budget must fit")
    }

    #[test]
    fn compaction_policy_ratio_tier_boundaries() {
        assert_eq!(base_context_ratio(16_000), 0.45);
        assert_eq!(base_context_ratio(16_001), 0.50);
        assert_eq!(base_context_ratio(32_000), 0.50);
        assert_eq!(base_context_ratio(32_001), 0.55);
        assert_eq!(base_context_ratio(64_000), 0.55);
        assert_eq!(base_context_ratio(64_001), 0.60);
        assert_eq!(base_context_ratio(128_000), 0.60);
        assert_eq!(base_context_ratio(128_001), 0.70);
        assert_eq!(base_context_ratio(512_000), 0.70);
        assert_eq!(base_context_ratio(512_001), 0.80);
    }

    #[test]
    fn compaction_policy_reserve_scales_with_window_and_caps() {
        assert_eq!(effective_reserve_tokens(16_384, 200_000), 16_384);
        assert_eq!(effective_reserve_tokens(1024, 32_000), 1_280);
        assert_eq!(effective_reserve_tokens(16_384, 1_000_000), 40_000);
        assert_eq!(effective_reserve_tokens(16_384, 2_000_000), 49_152);
    }

    #[test]
    fn compaction_policy_threshold_and_recent_per_window_tier() {
        let b = budget(16_000, 1024, DEFAULT_RECENT_TOKENS);
        assert_eq!((b.reserve, b.threshold_tokens, b.recent_tokens), (1024, 7_200, 3_960));
        let b = budget(32_000, 1024, DEFAULT_RECENT_TOKENS);
        assert_eq!((b.reserve, b.threshold_tokens, b.recent_tokens), (1_280, 16_000, 8_800));
        let b = budget(64_000, 1024, DEFAULT_RECENT_TOKENS);
        assert_eq!(
            (b.reserve, b.threshold_tokens, b.recent_tokens),
            (2_560, 35_200, 20_000)
        );
        let b = budget(128_000, 1024, DEFAULT_RECENT_TOKENS);
        assert_eq!(
            (b.reserve, b.threshold_tokens, b.recent_tokens),
            (5_120, 76_800, 20_000)
        );
        let b = budget(512_000, 1024, DEFAULT_RECENT_TOKENS);
        assert_eq!(
            (b.reserve, b.threshold_tokens, b.recent_tokens),
            (20_480, 358_400, 20_000)
        );
        let b = budget(200_000, DEFAULT_RESERVE_TOKENS, DEFAULT_RECENT_TOKENS);
        assert_eq!(
            (b.reserve, b.threshold_tokens, b.recent_tokens),
            (16_384, 140_000, 20_000)
        );
    }

    #[test]
    fn compaction_policy_threshold_is_clamped_by_reserve_ceiling() {
        let b = budget(32_000, 20_000, DEFAULT_RECENT_TOKENS);
        assert_eq!(b.threshold_tokens, 10_976);
    }

    #[test]
    fn compaction_policy_context_budget_too_small() {
        assert_eq!(
            compute_context_budget(16_000, DEFAULT_RESERVE_TOKENS, DEFAULT_RECENT_TOKENS, None),
            Err(ContextBudgetError::ContextBudgetTooSmall {
                window: 16_000,
                reserve: 16_384,
                headroom_tokens: 1_024,
            })
        );
        // Exactly reserve + headroom still cannot hold a threshold.
        assert!(compute_context_budget(17_408, 16_384, DEFAULT_RECENT_TOKENS, None).is_err());
        assert!(compute_context_budget(17_409, 16_384, DEFAULT_RECENT_TOKENS, None).is_ok());
    }

    #[test]
    fn compaction_policy_adaptive_moves_ratio_both_directions() {
        assert!((adaptive_context_ratio(0.70, Some(0.60)) - 0.65).abs() < 1e-6);
        assert!((adaptive_context_ratio(0.70, Some(0.05)) - 0.75).abs() < 1e-6);
        assert!((adaptive_context_ratio(0.70, None) - 0.70).abs() < 1e-6);
        // Exactly 0.5 is neither "more than half saved" nor "less than 10%".
        assert!((adaptive_context_ratio(0.70, Some(0.50)) - 0.70).abs() < 1e-6);
        // Clamped to the policy band.
        assert!((adaptive_context_ratio(0.45, Some(0.60)) - 0.40).abs() < 1e-6);
        assert!((adaptive_context_ratio(0.82, Some(0.05)) - 0.85).abs() < 1e-6);
    }

    #[test]
    fn compaction_policy_adaptive_budget_numbers() {
        let b = compute_context_budget(200_000, 16_384, 20_000, Some(0.60)).unwrap();
        assert!((b.ratio - 0.65).abs() < 1e-6);
        assert_eq!(b.threshold_tokens, 130_000);
        let b = compute_context_budget(200_000, 16_384, 20_000, Some(0.05)).unwrap();
        assert!((b.ratio - 0.75).abs() < 1e-6);
        assert_eq!(b.threshold_tokens, 150_000);
    }

    #[test]
    fn compaction_policy_recent_tokens_are_dynamic() {
        assert_eq!(recent_tokens_for_window(200_000, 0.70, DEFAULT_RECENT_TOKENS), 20_000);
        assert_eq!(recent_tokens_for_window(32_000, 0.50, DEFAULT_RECENT_TOKENS), 14_400);
        assert_eq!(recent_tokens_for_window(16_000, 0.45, DEFAULT_RECENT_TOKENS), 8_000);
        assert_eq!(recent_tokens_for_window(200_000, 0.70, 4_000), 4_000);
        assert_eq!(recent_tokens_for_window(2_000, 0.80, DEFAULT_RECENT_TOKENS), 1_024);
    }

    #[test]
    fn compaction_policy_recent_cutoff_tracks_token_budget() {
        let messages: Vec<Message> = (0..30).map(|_| Message::user(&"x".repeat(4_000))).collect();
        // 4_000 ASCII characters = 821 tokens, plus 1 token of per-message
        // framing.
        assert_eq!(recent_cutoff_for_token_budget(&messages, 14_400, MIN_TURNS_TO_KEEP), 13);
        assert_eq!(recent_cutoff_for_token_budget(&messages, 20_000, MIN_TURNS_TO_KEEP), 6);
        // The floor keeps the minimum number of turns even when they exceed it.
        assert_eq!(recent_cutoff_for_token_budget(&messages, 1_000, MIN_TURNS_TO_KEEP), 28);
        // Everything fits: nothing to compact.
        assert_eq!(recent_cutoff_for_token_budget(&messages, 100_000, MIN_TURNS_TO_KEEP), 0);
    }

    #[test]
    fn compaction_policy_recent_cutoff_keeps_newest_user_intent() {
        let mut messages: Vec<Message> = (0..40).map(|_| Message::assistant_text(&"x".repeat(4_000))).collect();
        messages[3] = Message::user("the current request");
        assert_eq!(recent_cutoff_for_token_budget(&messages, 20_000, MIN_TURNS_TO_KEEP), 3);
    }

    #[test]
    fn compaction_policy_context_accounting_is_conservative() {
        assert_eq!(conservative_context_tokens(10_000, Some(12_000)), 12_000);
        assert_eq!(conservative_context_tokens(12_000, Some(10_000)), 12_000);
        assert_eq!(conservative_context_tokens(10_000, None), 10_000);
        // Cache-hit tokens are part of the context, never subtracted.
        assert_eq!(
            effective_context_tokens_from_usage("openai", 400_000, Some(390_000), None),
            400_000
        );
        assert_eq!(
            effective_context_tokens_from_usage("anthropic", 10_000, Some(300_000), Some(5_000)),
            315_000
        );
    }
}

#[cfg(test)]
mod image_rejection_tests {
    use super::*;

    #[test]
    fn strip_all_images_replaces_every_image_with_a_note() {
        let mut a = vec![
            ContentBlock::Text {
                text: "keep".into(),
                cache_control: None,
            },
            ContentBlock::Image {
                media_type: "image/bmp".into(),
                data: "Qk0=".into(),
            },
        ];
        let mut b = vec![ContentBlock::Image {
            media_type: "image/png".into(),
            data: "iVBO".into(),
        }];
        let mut contents = vec![&mut a, &mut b];
        assert_eq!(strip_all_images_in_contents(&mut contents), 2);
        assert!(matches!(&a[0], ContentBlock::Text { text, .. } if text == "keep"));
        assert!(matches!(&a[1], ContentBlock::Text { text, .. } if text.contains("media_type=image/bmp")));
        assert!(matches!(&b[0], ContentBlock::Text { .. }));
    }
}

#[cfg(test)]
mod cutoff_alignment_tests {
    use super::*;

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

    fn assistant(text: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: text.into(),
                cache_control: None,
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
            }],
            timestamp: None,
            tool_duration_ms: None,
        }
    }

    #[test]
    fn a_cutoff_on_a_turn_start_is_kept() {
        let messages = vec![
            user("first"),
            tool_result("t1", "out"),
            user("second"),
            assistant("done"),
        ];
        assert_eq!(align_cutoff_to_turn_start(&messages, 2, 1_000), (2, false));
    }

    #[test]
    fn a_cutoff_mid_turn_moves_back_when_the_slack_fits() {
        let messages = vec![
            user("first"),
            tool_result("t1", "out"),
            assistant("thinking"),
            user("second"),
        ];
        assert_eq!(align_cutoff_to_turn_start(&messages, 2, 1_000), (0, false));
    }

    #[test]
    fn a_cutoff_mid_turn_stays_put_and_reports_the_split_when_slack_is_tight() {
        let messages = vec![
            user("first"),
            tool_result("t1", "out"),
            assistant("thinking"),
            user("second"),
        ];
        assert_eq!(align_cutoff_to_turn_start(&messages, 2, 0), (2, true));
    }

    #[test]
    fn a_zero_cutoff_is_never_a_split() {
        let messages = vec![user("first"), assistant("done")];
        assert_eq!(align_cutoff_to_turn_start(&messages, 0, 0), (0, false));
    }
}
