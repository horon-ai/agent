//! Recording what a schema repair changed, kept out of the dispatch body so the
//! dispatch path stays readable and this crate's largest file does not grow.

use serde_json::Value;

use super::arg_repair;
use horon_agent_session_types::repair_record::{ArgRepairRecord, remember};

/// Log, count and remember one repaired call, and give back the arguments that
/// will run.
///
/// An unrepaired call takes the first branch and nothing else: no log line, no
/// counter touch, no allocation.
pub(crate) fn record(tool_call_id: &str, tool_name: &str, repaired: arg_repair::Repaired) -> Value {
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
            tool_call_id,
            ArgRepairRecord {
                original_input: original,
                changes: repaired.changes,
            },
        );
    }
    repaired.input
}
