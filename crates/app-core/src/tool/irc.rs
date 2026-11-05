//! `irc`: message peers in this swarm through the daemon's mailbox bus.
//!
//! The tool is a thin surface over [`horon_agent_irc_core::IrcBus`]. It never
//! owns session state; the daemon installs one bus process-wide and the tool
//! resolves the calling session from [`ToolContext::session_id`].
//!
//! Registration is gated on the profile's allow-list (`build_allows("irc")`),
//! not on `[tools] irc`. A `full` session therefore sees the definition in the
//! deferred index while the switch stays off, and every call is refused at the
//! first step. That keeps the operator switch authoritative at execution time.

use super::{Tool, ToolContext, ToolOutput};
use anyhow::{Result, bail};
use async_trait::async_trait;
use horon_agent_irc_core::{
    IrcBus, MessageId, SendReport, SendRequest, WaitFilter, WaitOutcome, format_envelope, global,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

const SWITCH_OFF: &str = "IRC is off for this runtime. Set irc = true under [tools] in config.toml.";
const NO_DAEMON: &str = "IRC is unavailable: this session is not hosted by a horon-agent daemon.";
const VALID_ACTIONS: &str = "send, wait, inbox, list";
const DEFAULT_WAIT_SECS: u64 = 30;

pub struct IrcTool;

impl IrcTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for IrcTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct IrcInput {
    action: String,
    to: Option<String>,
    message: Option<String>,
    reply_to: Option<u64>,
    progress: Option<bool>,
    from: Option<String>,
    timeout_secs: Option<u64>,
    direct_only: Option<bool>,
}

impl IrcInput {
    fn non_empty(field: Option<String>) -> Option<String> {
        field.filter(|value| !value.trim().is_empty())
    }
}

#[async_trait]
impl Tool for IrcTool {
    fn name(&self) -> &str {
        crate::config::IRC_TOOL
    }

    fn description(&self) -> &str {
        "Send IRC messages to peers in this swarm, wait for replies, read buffered mail, and \
         list who is reachable. Delivery is non-blocking and follows the target's state: a \
         waiting peer takes it at once, a busy one at the next step boundary, and an idle one in \
         a new turn. `to` accepts a session id, a unique handle, or `all` from a root session; \
         only the same swarm is visible. Answer with reply_to and mark interim acknowledgements \
         with progress. Call wait only when blocked with nothing else to do, never to poll. \
         Incoming messages arrive as `<irc ...>` envelopes."
    }

    fn parameters_schema(&self) -> Value {
        let wait_max = crate::config::config().irc.wait_max_secs.max(1);
        json!({
            "type": "object",
            "required": ["action"],
            "properties": {
                "intent": super::intent_schema_property(),
                "action": {
                    "type": "string",
                    "enum": ["send", "wait", "inbox", "list"],
                    "description": "Operation to run."
                },
                "to": {
                    "type": "string",
                    "description": "Session id, unique handle, or `all` from a root session."
                },
                "message": {
                    "type": "string",
                    "description": "Message body for send."
                },
                "reply_to": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Id of the message being answered."
                },
                "progress": {
                    "type": "boolean",
                    "description": "Interim acknowledgement; not a reply."
                },
                "from": {
                    "type": "string",
                    "description": "Only accept messages from this peer."
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": wait_max,
                    "description": "Wait deadline; defaults to 30 seconds."
                },
                "direct_only": {
                    "type": "boolean",
                    "description": "Skip broadcast and progress messages."
                }
            }
        })
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        if !crate::config::config().tools.irc {
            bail!("{SWITCH_OFF}");
        }
        let Some(bus) = global::get() else {
            bail!("{NO_DAEMON}");
        };
        let params: IrcInput = serde_json::from_value(input)?;
        match params.action.as_str() {
            "send" => send(&bus, &ctx.session_id, params).await,
            "wait" => wait(&bus, &ctx, params).await,
            "inbox" => Ok(inbox(&bus, &ctx.session_id, params)),
            "list" => Ok(list(&bus, &ctx.session_id).await),
            other => bail!("unknown irc action \"{other}\"; valid actions are {VALID_ACTIONS}"),
        }
    }
}

async fn send(bus: &IrcBus, session_id: &str, params: IrcInput) -> Result<ToolOutput> {
    let Some(to) = IrcInput::non_empty(params.to) else {
        bail!("`to` is required for the send action");
    };
    let Some(body) = params.message.filter(|value| !value.trim().is_empty()) else {
        bail!("`message` is required for the send action");
    };
    let report = bus
        .send(SendRequest {
            from: Arc::from(session_id),
            to,
            body,
            reply_to: params.reply_to.map(MessageId),
            progress: params.progress.unwrap_or(false),
            wake_relay: false,
        })
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    Ok(ToolOutput::new(format_report(&report)?))
}

fn format_report(report: &SendReport) -> Result<String> {
    // A broadcast with no visible peers produces no legs at all: there is no
    // message id worth reporting and no failed target to explain.
    if report.receipts.is_empty() {
        bail!("no recipients: there is no visible agent to broadcast to");
    }
    let mut lines = Vec::with_capacity(report.receipts.len() + 1);
    lines.push(format!("message {} sent", report.message_id));
    for receipt in &report.receipts {
        let outcome = match &receipt.outcome {
            horon_agent_irc_core::Outcome::Injected => "injected".to_string(),
            horon_agent_irc_core::Outcome::Woken => "woken".to_string(),
            horon_agent_irc_core::Outcome::Failed(reason) => format!("failed ({reason})"),
        };
        lines.push(format!("{}: {outcome}", receipt.to));
    }
    let text = lines.join("\n");
    if report.delivered() == 0 {
        bail!("{text}");
    }
    Ok(text)
}

async fn wait(bus: &IrcBus, ctx: &ToolContext, params: IrcInput) -> Result<ToolOutput> {
    let wait_max = crate::config::config().irc.wait_max_secs.max(1);
    let timeout = params.timeout_secs.unwrap_or(DEFAULT_WAIT_SECS).clamp(1, wait_max);
    let filter = WaitFilter {
        from: IrcInput::non_empty(params.from).map(|value| Arc::from(value.as_str())),
        direct_only: params.direct_only.unwrap_or(false),
    };
    let signal = ctx.graceful_shutdown_signal.clone();
    let cancel = async move {
        match signal {
            Some(signal) => {
                if !signal.is_set() {
                    signal.notified().await;
                }
            }
            None => std::future::pending::<()>().await,
        }
    };
    let outcome = bus
        .wait(&ctx.session_id, filter, Duration::from_secs(timeout), cancel)
        .await;
    Ok(ToolOutput::new(match outcome {
        WaitOutcome::Message(message) => format_envelope(&message, None),
        WaitOutcome::Timeout => format!("no message within {timeout}s"),
        WaitOutcome::NothingToWaitFor => {
            "nothing to wait for: no one who owes you a reply can still answer".to_string()
        }
        WaitOutcome::Unanswerable(targets) => {
            let ids: Vec<&str> = targets.iter().map(|id| id.as_ref()).collect();
            format!(
                "unanswerable: {} are waiting on you; send them what they need",
                ids.join(", ")
            )
        }
        WaitOutcome::PendingInput => "interrupted: new input is queued for this session".to_string(),
        WaitOutcome::Cancelled => "cancelled".to_string(),
    }))
}

fn inbox(bus: &IrcBus, session_id: &str, params: IrcInput) -> ToolOutput {
    let from = IrcInput::non_empty(params.from);
    let messages = bus.drain(session_id, from.as_deref());
    if messages.is_empty() {
        return ToolOutput::new("inbox is empty");
    }
    let mut lines = Vec::with_capacity(messages.len());
    for message in messages {
        lines.push(format_envelope(&message, None));
    }
    ToolOutput::new(lines.join("\n"))
}

async fn list(bus: &IrcBus, session_id: &str) -> ToolOutput {
    let endpoints = bus.directory().roster(session_id).await;
    let mut lines = Vec::with_capacity(endpoints.len() + 1);
    for endpoint in endpoints {
        lines.push(format!(
            "{}\t{}\t{}\t{}",
            endpoint.id,
            endpoint.handle.as_deref().unwrap_or("-"),
            endpoint.status,
            endpoint.parent.as_deref().unwrap_or("-")
        ));
    }
    lines.push(format!("unread: {}", bus.unread_count(session_id)));
    ToolOutput::new(lines.join("\n"))
}

#[cfg(test)]
#[path = "irc_tests.rs"]
mod irc_tests;
