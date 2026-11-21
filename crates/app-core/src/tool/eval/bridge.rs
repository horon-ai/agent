use serde::Serialize;
use serde_json::Value;

use crate::tool::{Registry, ToolContext, toolbox};

pub(super) const MAX_CALLS: u32 = 64;
const SUBCALL_LABEL: &str = "eval-bridge-";
const MAX_RETURN_BYTES: usize = 32 * 1024;
const OUTPUT_CLIPPED: &str = "\n[Python bridge response truncated.]";

tokio::task_local! {
    static BRIDGE_CALL: ();
}

pub(in crate::tool) fn owns_subcall() -> bool {
    BRIDGE_CALL.try_with(|_| ()).is_ok()
}

pub(super) struct ToolRequest {
    pub(super) name: String,
    pub(super) args: Value,
}

#[derive(Serialize)]
pub(super) struct Reply {
    #[serde(rename = "type")]
    kind: &'static str,
    id: u64,
    seq: u32,
    ok: bool,
    text: String,
}

pub(super) struct Bridge<'a> {
    pub(super) registry: Registry,
    pub(super) context: &'a ToolContext,
}

impl Bridge<'_> {
    pub(super) async fn dispatch(&self, id: u64, seq: u32, request: ToolRequest) -> Reply {
        let name = Registry::resolve_tool_name_for_session(&self.context.session_id, &request.name);
        let (ok, text) = if !request.args.is_object() {
            (false, "Python tool calls require a JSON object of arguments.".into())
        } else if matches!(
            name,
            "eval" | "toolbox" | "batch" | "codemode" | "mcp" | "swarm" | "bg" | "schedule"
        ) {
            (false, format!("Tool '{name}' cannot be called from a Python cell."))
        } else if !toolbox::session_can_reach(&self.registry, &self.context.session_id, name) {
            (false, format!("Tool '{name}' is not available in this session."))
        } else {
            let ctx = self.context.for_subcall(format!("{SUBCALL_LABEL}{seq}"));
            match BRIDGE_CALL
                .scope((), self.registry.execute(name, request.args, ctx))
                .await
            {
                Ok(output) => (
                    !output.verdict.is_some_and(|verdict| verdict.is_failure()),
                    output.output,
                ),
                Err(error) => (false, format!("{error:#}")),
            }
        };
        let mode = crate::config::config().tools.secrets_mode;
        let mut text = if mode == crate::config::SecretsMode::Off {
            text
        } else {
            crate::secrets::mask_for_history(&text, &self.context.session_id, mode)
        };
        if text.len() > MAX_RETURN_BYTES {
            let boundary = text.floor_char_boundary(MAX_RETURN_BYTES - OUTPUT_CLIPPED.len());
            text.truncate(boundary);
            text.push_str(OUTPUT_CLIPPED);
        }
        Reply {
            kind: "tool_result",
            id,
            seq,
            ok,
            text,
        }
    }
}

/// `for_subcall` repeats a byte-length-delimited parent envelope at each depth.
pub(in crate::tool) fn inside_bridge(mut call: &str) -> bool {
    while let Some((length, rest)) = call.split_once(':') {
        let Ok(length) = length.parse::<usize>() else {
            return false;
        };
        let Some((parent, suffix)) = rest.split_at_checked(length) else {
            return false;
        };
        let Some(label) = suffix.strip_prefix(':') else {
            return false;
        };
        if label.starts_with(SUBCALL_LABEL) {
            return true;
        }
        call = parent;
    }
    false
}

/// The kernel owns the foreground shell task for the duration of this subcall.
pub(in crate::tool) struct AbortCommandOnDrop(pub(crate) tokio::task::AbortHandle);

impl Drop for AbortCommandOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
