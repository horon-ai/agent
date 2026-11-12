//! Recording what a schema repair changed, kept out of the dispatch body so the
//! dispatch path stays readable and this crate's largest file does not grow.

use serde_json::Value;

use super::ToolContext;
use super::arg_repair;
use horon_agent_session_types::repair_record::{ArgRepairRecord, remember};

/// Log, count and remember one repaired call, and give back the arguments that
/// will run.
///
/// An unrepaired call takes the first branch and nothing else: no log line, no
/// counter touch, no allocation.
pub(crate) fn record(ctx: &ToolContext, tool_name: &str, repaired: arg_repair::Repaired) -> Value {
    if repaired.changes.is_empty() {
        return repaired.input;
    }
    crate::logging::debug(&format!(
        "[tool:{tool_name}] repaired arguments: {:?}",
        repaired.changes
    ));
    horon_agent_perf_counters::record_tool_arg_repair(tool_name);
    if let Some(original) = repaired.original {
        remember(
            &ctx.session_id,
            &ctx.tool_call_id,
            ArgRepairRecord {
                original_input: original,
                changes: repaired.changes,
            },
        );
    }
    repaired.input
}

#[cfg(test)]
mod tests {
    use super::*;
    use horon_agent_message_types::ContentBlock;
    use horon_agent_session_types::repair_record;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {"command": {"type": "string"}, "timeout": {"type": "integer"}},
            "required": ["command"]
        })
    }

    const SESSION: &str = "repair-record-tests";

    fn ctx(call_id: &str) -> ToolContext {
        ToolContext {
            session_id: SESSION.to_string(),
            message_id: "repair-record-tests-message".to_string(),
            tool_call_id: call_id.to_string(),
            working_dir: None,
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: crate::tool::ToolExecutionMode::AgentTurn,
        }
    }

    fn tool_result(call_id: &str) -> ContentBlock {
        ContentBlock::ToolResult {
            tool_use_id: call_id.to_string(),
            content: "done".to_string(),
            is_error: None,
            verdict: None,
        }
    }

    #[test]
    fn a_repaired_call_leaves_its_record_for_the_session_to_take_once() {
        // Given a call whose numeric argument arrived as a string.
        let input = json!({"command": "ls", "timeout": "5000"});
        // When it is repaired and recorded.
        let stored = record(
            &ctx("call-repaired"),
            "bash",
            arg_repair::repair_recorded(&schema(), input.clone()),
        );
        // Then the tool ran on the coerced value...
        assert_eq!(stored["timeout"], json!(5000));
        // ...and the session takes the original and the change list exactly once.
        let taken = repair_record::take_in(SESSION, &[tool_result("call-repaired")]).expect("record");
        assert_eq!(taken.original_input, input);
        assert_eq!(taken.changes.len(), 1);
        assert!(taken.changes[0].contains("timeout"));
        assert!(repair_record::take_in(SESSION, &[tool_result("call-repaired")]).is_none());
    }

    #[test]
    fn a_call_that_needed_no_repair_leaves_nothing_behind() {
        // Given arguments that already match the schema.
        let input = json!({"command": "ls", "timeout": 5000});
        // When they pass through the same path.
        let stored = record(
            &ctx("call-clean"),
            "bash",
            arg_repair::repair_recorded(&schema(), input.clone()),
        );
        // Then the value is unchanged and no record is left for the session.
        assert_eq!(stored, input);
        assert!(repair_record::peek(SESSION, "call-clean").is_none());
    }

    #[test]
    fn a_result_bundling_several_calls_takes_the_repaired_one() {
        // Given a message that bundles a clean result and a repaired one.
        let input = json!({"command": "ls", "timeout": "5000"});
        record(
            &ctx("call-second"),
            "bash",
            arg_repair::repair_recorded(&schema(), input.clone()),
        );
        // When the session looks through the blocks.
        let blocks = [tool_result("call-first"), tool_result("call-second")];
        let taken = repair_record::take_in(SESSION, &blocks).expect("record");
        // Then the repaired call answers for the message.
        assert_eq!(taken.original_input, input);
    }
}
