//! Opt-in mid-stream rules.
//!
//! An operator can declare `[[stream_rules]]` entries, each with an `id`, a
//! `pattern` (Rust `regex`) and a `body`. While a response streams, the
//! assistant's own text is matched against those patterns. A match aborts the
//! response before any of it reaches the conversation, injects the rule body as
//! a system reminder, and re-issues the request from the same conversation
//! state, so the model sees the rule it broke instead of shipping the text.
//!
//! Four properties are deliberate:
//!
//! * The matcher is never constructed when `[features] stream_rules` is off, so
//!   a default install pays nothing per token and behaves exactly as before.
//! * A rule that already matched in this turn is never retried, so a model that
//!   keeps repeating one phrase cannot spend the whole injection budget on it.
//! * Only assistant text is scanned. Tool arguments arrive as separate stream
//!   events and are never handed to a rule.
//! * The scan is bounded: each delta is matched against the last
//!   [`LOOKBACK_BYTES`] of text plus one preceding character, which is enough
//!   for a phrase split across two chunks while keeping a long response from
//!   costing a full re-scan per token.

use super::*;
use serde_json::Value;
use std::collections::BTreeMap;

/// Bytes of already-streamed assistant text one rule may see.
///
/// A pattern longer than this cannot match. That is the documented limit of the
/// rolling buffer, and it keeps the per-delta cost constant.
pub(super) const LOOKBACK_BYTES: usize = 4 * 1024;

/// Rule injections allowed in one turn.
///
/// The fourth distinct rule to match stops the turn instead of retrying, so a
/// provider that keeps producing banned text cannot hold the turn open forever.
pub(super) const MAX_INJECTIONS: u32 = 3;

/// Injection point reported for a mid-stream rule reminder.
///
/// The soft-interrupt points A-D describe user and system queues; a rule abort
/// is its own point, so a client can tell the two apart without inspecting the
/// content.
pub(super) const STREAM_RULE_POINT: &str = "S";

struct StreamRule {
    id: String,
    pattern: regex::Regex,
    body: String,
    /// The pattern watches assistant text.
    text: bool,
    /// The pattern watches every tool call.
    tool_any: bool,
    /// The pattern watches these tool names, lowercased.
    tool_names: HashSet<String>,
    /// Globs on a tool call's `file_path`/`path` argument; empty matches any.
    paths: Vec<glob::Pattern>,
    delivery: crate::config::StreamRuleDelivery,
    repeat: crate::config::StreamRuleRepeat,
}

impl StreamRule {
    fn watches_text(&self) -> bool {
        self.text
    }

    fn watches_tool(&self, tool: &str) -> bool {
        if self.tool_any {
            return true;
        }
        let lowered = tool.to_ascii_lowercase();
        self.tool_names.contains(lowered.as_str())
    }

    fn tracks_repeat(&self) -> bool {
        self.repeat != crate::config::StreamRuleRepeat::Turn
    }
}

/// One rule that matched, with the body the caller injects.
pub(super) struct StreamRuleHit {
    pub(super) id: String,
    pub(super) body: String,
}

/// What the turn loop should do about a text match.
pub(super) enum StreamRuleAction {
    /// Inject this rule and re-issue the request.
    Retry(StreamRuleHit),
    /// The per-turn injection cap is spent. End the turn naming this rule.
    Capped(StreamRuleHit),
}

/// What the turn loop should do about a tool-call match.
pub(super) enum ToolRuleAction {
    /// Refuse the call before it reaches a tool.
    Block(StreamRuleHit),
    /// Run the call and carry the rule body in its result.
    Passive(StreamRuleHit),
}

/// Per-turn rule state: the compiled rules, the ids already injected, and the
/// number of injections spent.
///
/// It lives for one turn, across every retry of that turn, which is what makes
/// "never retry a rule that already matched" and the three-injection cap hold
/// over repeated attempts.
pub(super) struct TurnStreamRules {
    rules: Vec<StreamRule>,
    matched: HashSet<String>,
    injections: u32,
    /// User-turn index this state was built for, used by repeat policies.
    current_turn: u32,
    /// Rules that fired this turn and whose repeat policy needs the ledger.
    fired: Vec<(String, u32)>,
}

impl TurnStreamRules {
    /// Build the turn's rules from the loaded config.
    ///
    /// With the feature off nothing is compiled: an operator who has not opted
    /// in gets an empty set whose checks return immediately. Rules whose repeat
    /// policy suppresses them this turn are skipped before compilation, so a
    /// session that has already spent a `once` rule pays nothing for it.
    pub(super) fn from_config(ledger: &BTreeMap<String, u32>, current_turn: u32) -> Self {
        let config = crate::config::config();
        if !config.features.stream_rules {
            return Self::empty(current_turn);
        }
        let state = Self::from_entries_with_ledger(&config.stream_rules, ledger, current_turn);
        logging::info(&format!(
            "Mid-stream rules enabled: {} of {} configured rule(s) are enforceable",
            state.rule_count(),
            config.stream_rules.len()
        ));
        state
    }

    fn empty(current_turn: u32) -> Self {
        Self {
            rules: Vec::new(),
            matched: HashSet::new(),
            injections: 0,
            current_turn,
            fired: Vec::new(),
        }
    }

    /// Compile the entries with no persisted ledger (the default repeat policy
    /// never reads one).
    #[cfg(test)]
    pub(super) fn from_entries(entries: &[crate::config::StreamRuleConfig]) -> Self {
        Self::from_entries_with_ledger(entries, &BTreeMap::new(), 0)
    }

    /// Compile the entries, dropping any that cannot be enforced and any whose
    /// repeat policy suppresses them on `current_turn`.
    ///
    /// An entry with an empty id or pattern, an unparsable pattern or glob, an
    /// unknown scope, or an id already taken is skipped with a warning rather
    /// than failing the turn: a typo in one rule must not disable the rest or
    /// break the session. Duplicate ids are dropped because the transcript
    /// records rules by id.
    pub(super) fn from_entries_with_ledger(
        entries: &[crate::config::StreamRuleConfig],
        ledger: &BTreeMap<String, u32>,
        current_turn: u32,
    ) -> Self {
        let mut rules: Vec<StreamRule> = Vec::with_capacity(entries.len());
        for entry in entries {
            let id = entry.id.trim();
            let pattern = entry.pattern.trim();
            if id.is_empty() || pattern.is_empty() {
                logging::warn("Ignoring a stream rule with an empty id or pattern");
                continue;
            }
            if rules.iter().any(|rule| rule.id == id) {
                logging::warn(&format!("Ignoring a stream rule with the duplicate id '{id}'"));
                continue;
            }
            if suppressed_by_repeat(entry, ledger, current_turn) {
                logging::debug(&format!("Stream rule '{id}' is suppressed by its repeat policy"));
                continue;
            }
            let compiled = match regex::Regex::new(pattern) {
                Ok(compiled) => compiled,
                Err(error) => {
                    logging::warn(&format!(
                        "Ignoring stream rule '{id}': the pattern does not compile ({error})"
                    ));
                    continue;
                }
            };

            let (mut text, mut tool_any, mut tool_names) = (false, false, HashSet::new());
            if entry.scope.is_empty() {
                text = true;
            }
            let mut unknown_scope = None;
            for scope in &entry.scope {
                let scope = scope.trim();
                if scope.is_empty() || scope.eq_ignore_ascii_case("text") {
                    text = true;
                } else if scope.eq_ignore_ascii_case("tool") {
                    tool_any = true;
                } else if let Some(name) = scope.strip_prefix("tool:").or_else(|| scope.strip_prefix("tool.")) {
                    let name = name.trim();
                    if !name.is_empty() {
                        tool_names.insert(name.to_ascii_lowercase());
                    }
                } else {
                    unknown_scope = Some(scope.to_string());
                }
            }
            if let Some(scope) = unknown_scope {
                logging::warn(&format!(
                    "Ignoring stream rule '{id}': unknown scope '{scope}' (use text, tool, or tool:<name>)"
                ));
                continue;
            }

            let mut paths = Vec::with_capacity(entry.paths.len());
            let mut bad_glob = None;
            for path in &entry.paths {
                match glob::Pattern::new(path) {
                    Ok(compiled) => paths.push(compiled),
                    Err(error) => {
                        bad_glob = Some((path.clone(), error));
                        break;
                    }
                }
            }
            if let Some((path, error)) = bad_glob {
                logging::warn(&format!(
                    "Ignoring stream rule '{id}': the path glob '{path}' does not compile ({error})"
                ));
                continue;
            }

            rules.push(StreamRule {
                id: id.to_string(),
                pattern: compiled,
                body: entry.body.clone(),
                text,
                tool_any,
                tool_names,
                paths,
                delivery: entry.delivery,
                repeat: entry.repeat,
            });
        }
        Self {
            rules,
            matched: HashSet::new(),
            injections: 0,
            current_turn,
            fired: Vec::new(),
        }
    }

    /// Number of rules that survived compilation.
    pub(super) fn rule_count(&self) -> usize {
        self.rules.len()
    }

    /// Injections spent in this turn.
    pub(super) fn injections(&self) -> u32 {
        self.injections
    }

    /// Match the rolling window of `text` against every interrupt-delivery text
    /// rule that has not matched yet, in config order.
    ///
    /// The first hit decides the action, so two rules matching in the same chunk
    /// are resolved deterministically: the earlier entry wins and the later one
    /// is still available on the next attempt.
    pub(super) fn check(&mut self, text: &str) -> Option<StreamRuleAction> {
        if self.rules.is_empty() {
            return None;
        }
        let window = lookback_window(text);
        let hit = self.rules.iter().find(|rule| {
            rule.watches_text()
                && rule.delivery == crate::config::StreamRuleDelivery::Interrupt
                && !self.matched.contains(&rule.id)
                && rule.pattern.is_match(window)
        })?;
        let hit = StreamRuleHit {
            id: hit.id.clone(),
            body: hit.body.clone(),
        };
        Some(if self.injections >= MAX_INJECTIONS {
            StreamRuleAction::Capped(hit)
        } else {
            StreamRuleAction::Retry(hit)
        })
    }

    /// Collect every passive text rule that matches the completed response.
    ///
    /// Passive rules never abort: the caller merges these reminders into the
    /// next request instead of re-issuing the one that just finished.
    pub(super) fn passive_text_hits(&mut self, text: &str) -> Vec<StreamRuleHit> {
        if self.rules.is_empty() {
            return Vec::new();
        }
        let window = lookback_window(text);
        let hits: Vec<StreamRuleHit> = self
            .rules
            .iter()
            .filter(|rule| {
                rule.watches_text()
                    && rule.delivery == crate::config::StreamRuleDelivery::Passive
                    && !self.matched.contains(&rule.id)
                    && rule.pattern.is_match(window)
            })
            .map(|rule| StreamRuleHit {
                id: rule.id.clone(),
                body: rule.body.clone(),
            })
            .collect();
        for hit in &hits {
            self.mark_fired(&hit.id);
        }
        hits
    }

    /// Decide a tool call against the tool-scoped rules.
    ///
    /// The input is matched as compact JSON, and a rule with `paths` only fires
    /// when the call's `file_path`/`path` argument matches one of its globs.
    pub(super) fn check_tool(&mut self, tool: &str, input: &Value) -> Option<ToolRuleAction> {
        if self.rules.is_empty() {
            return None;
        }
        let serialized = serde_json::to_string(input).unwrap_or_default();
        let rule = self.rules.iter().find(|rule| {
            !self.matched.contains(&rule.id)
                && rule.watches_tool(tool)
                && self.tool_path_matches(rule, input)
                && rule.pattern.is_match(&serialized)
        })?;
        let hit = StreamRuleHit {
            id: rule.id.clone(),
            body: rule.body.clone(),
        };
        let delivery = rule.delivery;
        self.mark_fired(&hit.id);
        Some(match delivery {
            crate::config::StreamRuleDelivery::Interrupt => ToolRuleAction::Block(hit),
            crate::config::StreamRuleDelivery::Passive => ToolRuleAction::Passive(hit),
        })
    }

    fn tool_path_matches(&self, rule: &StreamRule, input: &Value) -> bool {
        if rule.paths.is_empty() {
            return true;
        }
        let Some(path) = input
            .get("file_path")
            .or_else(|| input.get("path"))
            .and_then(Value::as_str)
        else {
            return false;
        };
        rule.paths.iter().any(|pattern| pattern.matches(path))
    }

    /// Record that this rule's body was injected, spending one injection.
    pub(super) fn record_injection(&mut self, hit: &StreamRuleHit) {
        self.matched.insert(hit.id.clone());
        self.injections += 1;
        self.mark_fired(&hit.id);
    }

    /// Mark a rule spent for this turn and remember the ledger write its repeat
    /// policy needs.
    fn mark_fired(&mut self, id: &str) {
        self.matched.insert(id.to_string());
        let tracked = self
            .rules
            .iter()
            .find(|rule| rule.id == id)
            .is_some_and(StreamRule::tracks_repeat);
        if tracked {
            self.fired.push((id.to_string(), self.current_turn));
        }
    }

    /// Move this turn's repeat-policy fires into the session ledger.
    pub(super) fn flush_ledger(&mut self, ledger: &mut BTreeMap<String, u32>) {
        for (id, turn) in self.fired.drain(..) {
            ledger.insert(id, turn);
        }
    }
}

fn suppressed_by_repeat(
    entry: &crate::config::StreamRuleConfig,
    ledger: &BTreeMap<String, u32>,
    current_turn: u32,
) -> bool {
    let last = ledger.get(entry.id.trim());
    match entry.repeat {
        crate::config::StreamRuleRepeat::Turn => false,
        crate::config::StreamRuleRepeat::Once => last.is_some(),
        crate::config::StreamRuleRepeat::AfterGap => {
            last.is_some_and(|last| current_turn.saturating_sub(*last) < entry.repeat_gap.max(1))
        }
    }
}

/// The current user-turn index for repeat policies: one-based count of user
/// messages carrying text, excluding harness-inserted system reminders.
pub(super) fn user_turn_index(session: &crate::session::Session) -> u32 {
    session
        .active_context()
        .iter()
        .filter(|message| {
            message.role == Role::User
                && message.display_role.is_none()
                && message
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::Text { .. }))
        })
        .count() as u32
}

/// The assistant text one rule may see: the last [`LOOKBACK_BYTES`] bytes plus
/// one preceding character.
///
/// The extra character is boundary context. A pattern anchored with `\b` or `^`
/// would otherwise see a false boundary wherever the window happens to start,
/// so the window always begins one character before the region it scans.
pub(super) fn lookback_window(text: &str) -> &str {
    if text.len() <= LOOKBACK_BYTES {
        return text;
    }
    let mut start = text.len() - LOOKBACK_BYTES;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    if let Some(boundary) = text[..start].chars().next_back() {
        start -= boundary.len_utf8();
    }
    &text[start..]
}

/// The reminder a matching rule injects.
pub(super) fn reminder(hit: &StreamRuleHit) -> String {
    format!(
        "<system-reminder>Mid-stream rule \"{}\" matched while you were writing. {}</system-reminder>",
        hit.id, hit.body
    )
}

/// The notice written when the injection cap stops the turn.
pub(super) fn cap_notice(hit: &StreamRuleHit) -> String {
    format!(
        "Mid-stream rule \"{}\" matched after {} rule injections this turn; the turn stopped without another retry.",
        hit.id, MAX_INJECTIONS
    )
}

/// The tool result a blocked tool-scoped rule returns instead of running.
pub(super) fn blocked_message(hit: &StreamRuleHit) -> String {
    format!("Blocked by rule \"{}\": {}", hit.id, hit.body)
}

/// The reminder appended to a passively watched tool's result.
pub(super) fn tool_passive_reminder(hit: &StreamRuleHit) -> String {
    format!(
        "\n\n<system-reminder>Rule \"{}\": {}</system-reminder>",
        hit.id, hit.body
    )
}

/// The reminder a passive text rule merges into the next request.
pub(super) fn text_passive_reminder(hit: &StreamRuleHit) -> String {
    format!(
        "<system-reminder>Rule \"{}\" matched: {}</system-reminder>",
        hit.id, hit.body
    )
}

impl Agent {
    /// Store the notice the injection cap leaves behind.
    ///
    /// It is an assistant-role text message displayed as a system notice, so the
    /// model reads it on the next turn and the user reads it in the transcript,
    /// exactly like the loop-guard and turn-recovery stops.
    pub(super) fn record_stream_rule_stop(&mut self, notice: &str) {
        self.add_message_with_display_role(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: notice.to_string(),
                cache_control: None,
            }],
            Some(StoredDisplayRole::System),
        );
    }
}

#[cfg(test)]
#[path = "stream_rules_tests.rs"]
mod tests;
