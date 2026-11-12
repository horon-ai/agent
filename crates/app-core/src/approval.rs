//! Approval modes: how much friction a tool call earns before it runs.
//!
//! The mode is a floor on friction, not a replacement for the command-risk
//! gate: a call that gate refuses is still refused there in every mode. What
//! this module decides is only whether a call may run without asking, and it
//! writes one audit line for every call it lets through.
//!
//! Two rules are load-bearing and easy to get backwards:
//!
//! 1. `Off` returns before the read-only branch. If the read-only check ran
//!    first, every read call would take the audit path, and a mode that is
//!    supposed to change nothing would start writing a log line per call.
//! 2. A dual-confirm class outranks the mode. The whole point of listing a
//!    class there is that no mode may auto-approve it.
//! 3. A restricted path outranks `Off` for the same reason, and a scoped grant
//!    never covers one: `[tools] restricted_paths` is a standing "ask me".

use crate::config::{ApprovalMode, BashPatternApproval, BashPatternRule};
use crate::tool::ToolEffects;

/// Class name for a shell command the risk classifier will not run as issued.
pub const DESTRUCTIVE_SHELL: &str = "destructive-shell";

/// What the dispatch path should do with one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalEvaluation {
    /// The call may run now.
    AutoApproved { reason: String },
    /// The call must wait for the user's decision.
    RequiresApproval { reason: String, is_dual_confirm: bool },
}

impl ApprovalEvaluation {
    pub fn is_auto_approved(&self) -> bool {
        matches!(self, Self::AutoApproved { .. })
    }

    pub fn reason(&self) -> &str {
        match self {
            Self::AutoApproved { reason } | Self::RequiresApproval { reason, .. } => reason,
        }
    }
}

fn is_pure_read(effects: ToolEffects) -> bool {
    !effects.writes() && !effects.appends() && !effects.processes() && !effects.networks()
}

/// One matching `[[tools.bash_patterns]]` rule.
///
/// The rule's text travels with the decision: the refusal and the approval
/// reason both name the pattern the user wrote, so a surprising decision can be
/// traced back to its line in config.toml.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BashPatternVerdict {
    pub pattern: String,
    pub approval: BashPatternApproval,
}

/// The first `[[tools.bash_patterns]]` rule that matches `command`.
pub fn bash_pattern_verdict(command: &str) -> Option<BashPatternVerdict> {
    let config = crate::config::config();
    match_bash_pattern(command, &config.tools.bash_patterns).map(|rule| BashPatternVerdict {
        pattern: rule.pattern.clone(),
        approval: rule.approval,
    })
}

/// The first matching rule in config order, or `None`.
///
/// `allow` is deliberately narrower than `deny`/`prompt`: it may only match the
/// whole command and only a command with no unquoted shell control characters,
/// because `git status && rm -rf x` must not inherit the approval a `git
/// status*` rule would otherwise grant the whole line.
pub(crate) fn match_bash_pattern<'a>(command: &str, rules: &'a [BashPatternRule]) -> Option<&'a BashPatternRule> {
    if rules.is_empty() {
        return None;
    }
    let whole = normalize_command(command);
    let segments: Vec<String> = horon_agent_command_risk::split_segments(command)
        .into_iter()
        .map(|segment| {
            segment
                .iter()
                .map(|token| token.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|segment| !segment.is_empty())
        .collect();
    for rule in rules {
        let Some(pattern) = compile_bash_pattern(&rule.pattern) else {
            continue;
        };
        match rule.approval {
            BashPatternApproval::Allow => {
                if !has_unquoted_shell_control(command) && pattern.is_match(&whole) {
                    return Some(rule);
                }
            }
            BashPatternApproval::Deny | BashPatternApproval::Prompt => {
                if pattern.is_match(&whole) || segments.iter().any(|segment| pattern.is_match(segment)) {
                    return Some(rule);
                }
            }
        }
    }
    None
}

/// Trim and collapse every whitespace run to one space, so a rule does not have
/// to reproduce a command's spacing exactly.
fn normalize_command(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Compile one rule pattern: `*` matches any run, everything else is literal.
fn compile_bash_pattern(pattern: &str) -> Option<regex::Regex> {
    let normalized = normalize_command(pattern);
    let mut expr = String::with_capacity(normalized.len() + 4);
    expr.push('^');
    let mut literal = String::new();
    for ch in normalized.chars() {
        if ch == '*' {
            if !literal.is_empty() {
                expr.push_str(&regex::escape(&literal));
                literal.clear();
            }
            expr.push_str(".*");
        } else {
            literal.push(ch);
        }
    }
    if !literal.is_empty() {
        expr.push_str(&regex::escape(&literal));
    }
    expr.push('$');
    regex::Regex::new(&expr).ok()
}

/// Whether a command carries a character that starts new syntax.
///
/// A backslash, expansion, substitution, or list operator can turn one command
/// into several, so an `allow` rule must not cover a line that contains one.
fn has_unquoted_shell_control(command: &str) -> bool {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Quote {
        None,
        Single,
        Double,
    }

    let mut quote = Quote::None;
    for ch in command.chars() {
        match quote {
            Quote::Single => {
                if ch == '\'' {
                    quote = Quote::None;
                }
            }
            Quote::Double => match ch {
                '\\' | '`' | '$' => return true,
                '"' => quote = Quote::None,
                _ => {}
            },
            Quote::None => match ch {
                '\'' => quote = Quote::Single,
                '"' => quote = Quote::Double,
                '\\' | '`' | '\n' | '\r' | ';' | '&' | '|' | '<' | '>' | '$' | '(' | ')' => return true,
                _ => {}
            },
        }
    }
    false
}

/// The command-risk class of a call, for `dual_confirm_classes` to match.
///
/// Reuses [`horon_agent_command_risk::assess`] rather than re-classifying: a
/// class is exactly a verdict this crate already makes. Only a shell command
/// names one, because only a shell command has a blast radius to assess.
pub fn danger_class(
    tool: &str,
    input: &serde_json::Value,
    working_dir: Option<&std::path::Path>,
) -> Option<&'static str> {
    if tool != "bash" {
        return None;
    }
    let command = input.get("command")?.as_str()?;
    let mut context = horon_agent_command_risk::RiskContext::from_env(working_dir.map(std::path::Path::to_path_buf));
    // Assess the same scratch path the child shell actually receives, the way
    // `bash_destructive_gate` does. Without this the daemon's own environment
    // leaves `scratch_dir` as None, so a command writing under the scratch
    // directory is rated `Confirm` here and `Low` at the gate that will run it.
    // When the scratch directory cannot be resolved, assess with no scratch path
    // rather than guessing one: the shell gate then sees the same `None` and the
    // two agree, which is the property N1 asked for. The old disagreement came
    // from one side setting it and the other not. The helper answers `None` on a
    // platform whose shells receive no scratch directory.
    context.scratch_dir = super::tool::tool_scratch_dir_for_risk();
    let assessment = horon_agent_command_risk::assess(command, &context);
    if assessment.level.runs_immediately() {
        None
    } else {
        Some(DESTRUCTIVE_SHELL)
    }
}

/// Everything one approval decision reads, so a caller cannot pass the same
/// seven values in a different order at each call site.
///
/// `danger` names the command-risk class the call fell into, when the caller
/// classified one. `dual_confirm` lists the classes that must always ask.
/// `bash_pattern` is the configured rule that matched the command, if any: a
/// dual-confirm class still outranks it. `paths` are the filesystem paths the
/// call names, and `restricted` is the `[tools] restricted_paths` pattern that
/// matched one of them, if any: a restricted path asks in every mode, including
/// `Off`.
pub struct ApprovalCall<'a> {
    pub tool: &'a str,
    pub effects: ToolEffects,
    pub mode: ApprovalMode,
    pub dual_confirm: &'a [String],
    pub danger: Option<&'a str>,
    pub bash_pattern: Option<&'a BashPatternVerdict>,
    pub paths: &'a [std::path::PathBuf],
    pub restricted: Option<&'a str>,
}

/// Decide one call, without writing anything.
pub fn evaluate(call: ApprovalCall<'_>) -> ApprovalEvaluation {
    let ApprovalCall {
        tool,
        effects,
        mode,
        dual_confirm,
        danger,
        bash_pattern,
        paths,
        restricted,
    } = call;
    if let Some(danger) = danger
        && dual_confirm.iter().any(|class| class.as_str() == danger)
    {
        return ApprovalEvaluation::RequiresApproval {
            reason: format!("{danger} is a dual-confirm class"),
            is_dual_confirm: true,
        };
    }

    // A restricted path asks in every mode, above both the `Off` return and a
    // `[[tools.bash_patterns]]` allow. The point of listing a path is that no
    // mode lets a call touch it, and a grant never covers it either.
    if let Some(pattern) = restricted {
        return ApprovalEvaluation::RequiresApproval {
            reason: format!("path matches restricted_paths '{pattern}'"),
            is_dual_confirm: false,
        };
    }

    if let Some(verdict) = bash_pattern {
        return match verdict.approval {
            BashPatternApproval::Allow => ApprovalEvaluation::AutoApproved {
                reason: format!("bash pattern '{}' allows it", verdict.pattern),
            },
            BashPatternApproval::Prompt => ApprovalEvaluation::RequiresApproval {
                reason: format!("bash pattern '{}' requires approval", verdict.pattern),
                is_dual_confirm: false,
            },
            // A deny is refused by the dispatcher before evaluation; keeping it
            // as an asking decision here means a future caller cannot silently
            // turn it into an auto-approval.
            BashPatternApproval::Deny => ApprovalEvaluation::RequiresApproval {
                reason: format!("bash pattern '{}' denies it", verdict.pattern),
                is_dual_confirm: false,
            },
        };
    }

    if mode == ApprovalMode::Off {
        return ApprovalEvaluation::AutoApproved {
            reason: "approval disabled".to_string(),
        };
    }

    if is_pure_read(effects) {
        return ApprovalEvaluation::AutoApproved {
            reason: "read-only call".to_string(),
        };
    }

    match mode {
        ApprovalMode::Off => ApprovalEvaluation::AutoApproved {
            reason: "approval disabled".to_string(),
        },
        ApprovalMode::Write => {
            // `write` asks before a call that can mutate files, and before a
            // call that runs a process or reaches the network. A tool that
            // declares a write effect but names no path (todo, ask, undo,
            // monitor, checkpoint) keeps running, because there is no file to
            // approve and asking would be noise the user cannot act on.
            let mutates_a_named_file = !paths.is_empty() && (effects.writes() || effects.appends());
            if effects.processes() || effects.networks() {
                ApprovalEvaluation::RequiresApproval {
                    reason: format!("{tool} runs a process or reaches the network in write mode"),
                    is_dual_confirm: false,
                }
            } else if mutates_a_named_file {
                ApprovalEvaluation::RequiresApproval {
                    reason: format!("{tool} mutates a file in write mode"),
                    is_dual_confirm: false,
                }
            } else {
                ApprovalEvaluation::AutoApproved {
                    reason: "no file mutation to approve in write mode".to_string(),
                }
            }
        }
        // `all` asks for every call that is not read-only. For a path-naming
        // mutation that is the same set `write` asks about; the difference is
        // the path-less write-effect tools above, which `all` asks about too.
        ApprovalMode::All => ApprovalEvaluation::RequiresApproval {
            reason: format!("{tool} is not read-only in all mode"),
            is_dual_confirm: false,
        },
    }
}

/// Record an auto-approval the caller decided outside [`evaluate`].
///
/// A scoped grant answers a call `evaluate` correctly marked as asking; the
/// audit line must still be written, with reason `grant`, so the log explains
/// why the user was not asked.
pub fn audit_granted_call(tool: &str, mode: ApprovalMode, tool_call_id: &str) {
    if mode != ApprovalMode::Off {
        audit(tool, mode, "grant", tool_call_id);
    }
}

fn audit(tool: &str, mode: ApprovalMode, reason: &str, tool_call_id: &str) {
    observe_audit(tool, mode, reason, tool_call_id);
    crate::logging::event_info(
        "TOOL_APPROVAL",
        [
            ("tool", tool.to_string()),
            ("mode", mode.as_str().to_string()),
            ("reason", reason.to_string()),
            ("tool_call_id", tool_call_id.to_string()),
        ],
    );
}

/// [`evaluate`] plus the audit record its decision earns.
///
/// Every auto-approval except the `Off` early return is recorded. `Off` writes
/// nothing, which is what keeps an unconfigured session identical to what it did
/// before approval modes existed.
pub fn evaluate_and_audit(call: ApprovalCall<'_>, tool_call_id: &str) -> ApprovalEvaluation {
    let mode = call.mode;
    let tool = call.tool;
    let evaluation = evaluate(call);
    if let ApprovalEvaluation::AutoApproved { reason } = &evaluation
        && mode != ApprovalMode::Off
    {
        audit(tool, mode, reason, tool_call_id);
    }
    evaluation
}

/// One audit record, kept for tests so a case can assert on the decision
/// without depending on the logger being initialized in the test process.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuditRecord {
    pub(crate) tool: String,
    pub(crate) mode: &'static str,
    pub(crate) reason: String,
    pub(crate) tool_call_id: String,
}

#[cfg(test)]
static AUDIT_SINK: std::sync::Mutex<Vec<AuditRecord>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn observe_audit(tool: &str, mode: ApprovalMode, reason: &str, tool_call_id: &str) {
    AUDIT_SINK.lock().expect("approval audit sink").push(AuditRecord {
        tool: tool.to_string(),
        mode: mode.as_str(),
        reason: reason.to_string(),
        tool_call_id: tool_call_id.to_string(),
    });
}

#[cfg(not(test))]
fn observe_audit(_tool: &str, _mode: ApprovalMode, _reason: &str, _tool_call_id: &str) {}

/// Audit records this process produced for `tool_call_id`.
#[cfg(test)]
pub(crate) fn audit_records_for(tool_call_id: &str) -> Vec<AuditRecord> {
    AUDIT_SINK
        .lock()
        .expect("approval audit sink")
        .iter()
        .filter(|record| record.tool_call_id == tool_call_id)
        .cloned()
        .collect()
}

#[cfg(test)]
#[path = "approval_tests.rs"]
mod tests;
