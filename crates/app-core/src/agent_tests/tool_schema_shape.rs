//! The eager tool schemas with their prose removed.
//!
//! A tool description may be shortened to save request bytes; what the model
//! is allowed to send may not change with it. Each eager catalog entry is
//! pinned here without its `description` strings, so an edit that touches a
//! name, a type, an enum, a default or a required list fails this test, and an
//! edit that only rewrites prose does not.

use serde_json::Value;

use super::tool_tiers::{ProfileEnv, agent_with_mcp_management_tool};
use crate::config::CORE_TOOL_PROFILE;

/// One line per eager tool, in catalog order: the entry `build_tools` sends,
/// minus every `description` string.
const PINNED_SHAPES: &[&str] = &[
    r#"{"name":"agentgrep","parameters":{"additionalProperties":false,"properties":{"accept_large_output":{"type":["boolean","null"]},"after":{"type":["integer","null"]},"before":{"type":["integer","null"]},"file":{"type":["string","null"]},"glob":{"type":["string","null"]},"intent":{"type":"string"},"max_files":{"type":["integer","null"]},"max_regions":{"type":["integer","null"]},"mode":{"enum":["grep","find","outline","trace","locate"],"type":["string","null"]},"path":{"type":["string","null"]},"paths_only":{"type":["boolean","null"]},"query":{"type":["string","null"]},"regex":{"type":["boolean","null"]},"terms":{"items":{"type":"string"},"type":["array","null"]},"type":{"type":["string","null"]}},"required":["accept_large_output","after","before","file","glob","intent","max_files","max_regions","mode","path","paths_only","query","regex","terms","type"],"type":"object"},"strict":true,"type":"function"}"#,
    r#"{"name":"apply_patch","parameters":{"additionalProperties":false,"properties":{"accept_large_output":{"type":["boolean","null"]},"intent":{"type":"string"},"patch_text":{"type":"string"}},"required":["accept_large_output","intent","patch_text"],"type":"object"},"strict":true,"type":"function"}"#,
    r#"{"name":"bash","parameters":{"additionalProperties":false,"properties":{"accept_large_output":{"type":["boolean","null"]},"command":{"type":"string"},"cwd":{"type":["string","null"]},"intent":{"type":"string"},"justification":{"type":["string","null"]},"name":{"type":["string","null"]},"notify":{"type":["boolean","null"]},"notify_pattern":{"type":["string","null"]},"pty":{"enum":["off","auto","always"],"type":["string","null"]},"ready":{"additionalProperties":false,"properties":{"host":{"type":["string","null"]},"log":{"type":["string","null"]},"port":{"type":["integer","null"]},"timeout_secs":{"type":["number","null"]}},"required":["host","log","port","timeout_secs"],"type":["object","null"]},"run_in_background":{"type":["boolean","null"]},"stall_wake_seconds":{"type":["integer","null"]},"timeout":{"type":["integer","null"]},"wake":{"type":["boolean","null"]}},"required":["accept_large_output","command","cwd","intent","justification","name","notify","notify_pattern","pty","ready","run_in_background","stall_wake_seconds","timeout","wake"],"type":"object"},"strict":true,"type":"function"}"#,
    r#"{"name":"bg","parameters":{"additionalProperties":false,"properties":{"accept_large_output":{"type":["boolean","null"]},"action":{"enum":["list","status","output","tail","cancel","cleanup","retention","watch","delivery","subscribe","wait"],"type":"string"},"children":{"type":["boolean","null"]},"dry_run":{"type":["boolean","null"]},"graceful_timeout_ms":{"type":["integer","null"]},"include_output_preview":{"type":["boolean","null"]},"intent":{"type":"string"},"latest":{"type":["boolean","null"]},"lines":{"type":["integer","null"]},"max_age_hours":{"type":["integer","null"]},"max_wait_seconds":{"type":["integer","null"]},"messages":{"type":["boolean","null"]},"notify":{"type":["boolean","null"]},"return_on_progress":{"type":["boolean","null"]},"session_only":{"type":["boolean","null"]},"stall_wake_seconds":{"type":["integer","null"]},"status_filter":{"anyOf":[{"type":"string"},{"items":{"type":"string"},"type":"array"},{"type":"null"}]},"tail_lines":{"type":["integer","null"]},"task_id":{"type":["string","null"]},"task_ids":{"items":{"type":"string"},"type":["array","null"]},"wait_mode":{"enum":["any","all","first_failure"],"type":["string","null"]},"wake":{"type":["boolean","null"]}},"required":["accept_large_output","action","children","dry_run","graceful_timeout_ms","include_output_preview","intent","latest","lines","max_age_hours","max_wait_seconds","messages","notify","return_on_progress","session_only","stall_wake_seconds","status_filter","tail_lines","task_id","task_ids","wait_mode","wake"],"type":"object"},"strict":true,"type":"function"}"#,
    r#"{"name":"edit","parameters":{"additionalProperties":false,"properties":{"accept_large_output":{"type":["boolean","null"]},"edits":{"items":{"additionalProperties":false,"properties":{"new_string":{"type":"string"},"old_string":{"type":"string"},"replace_all":{"type":["boolean","null"]}},"required":["new_string","old_string","replace_all"],"type":"object"},"type":["array","null"]},"file_path":{"type":"string"},"intent":{"type":"string"},"new_string":{"type":["string","null"]},"old_string":{"type":["string","null"]},"replace_all":{"type":["boolean","null"]}},"required":["accept_large_output","edits","file_path","intent","new_string","old_string","replace_all"],"type":"object"},"strict":true,"type":"function"}"#,
    r#"{"name":"eval","parameters":{"additionalProperties":false,"properties":{"accept_large_output":{"type":["boolean","null"]},"code":{"type":"string"},"intent":{"type":"string"},"reset":{"type":["boolean","null"]},"timeout":{"type":["integer","null"]}},"required":["accept_large_output","code","intent","reset","timeout"],"type":"object"},"strict":true,"type":"function"}"#,
    r#"{"name":"ls","parameters":{"additionalProperties":false,"properties":{"accept_large_output":{"type":["boolean","null"]},"ignore":{"items":{"type":"string"},"type":["array","null"]},"intent":{"type":"string"},"path":{"type":["string","null"]}},"required":["accept_large_output","ignore","intent","path"],"type":"object"},"strict":true,"type":"function"}"#,
    r#"{"name":"read","parameters":{"additionalProperties":false,"properties":{"accept_large_output":{"type":["boolean","null"]},"file_path":{"type":"string"},"full":{"type":["boolean","null"]},"hashline":{"type":["boolean","null"]},"intent":{"type":"string"},"limit":{"type":["integer","null"]},"start_line":{"type":["integer","null"]}},"required":["accept_large_output","file_path","full","hashline","intent","limit","start_line"],"type":"object"},"strict":true,"type":"function"}"#,
    r#"{"name":"todo","parameters":{"additionalProperties":false,"properties":{"accept_large_output":{"type":["boolean","null"]},"goals":{"items":{"additionalProperties":false,"properties":{"autonomy":{"enum":["requested_only","necessary_followthrough","proactive","stewardship"],"type":["string","null"]},"closed_feedback_loop":{"enum":["absent","weak","usable","strong","closed"],"type":"string"},"delivery_state":{"enum":["change_made","integrated","workflow_validated","outcome_delivered"],"type":["string","null"]},"difficulty":{"enum":["trivial","routine","involved","complex","hard","expert","research","open_ended"],"type":["string","null"]},"feedback_loop":{"type":"string"},"feedback_loop_coverage":{"enum":["narrow","main_paths","edge_and_integration_paths"],"type":"string"},"feedback_loop_relevance":{"enum":["indirect","synthetic","representative","acceptance_blocked","acceptance_aligned"],"type":"string"},"feedback_loop_traceability":{"enum":["unmapped","partial","complete"],"type":"string"},"group":{"type":["string","null"]},"iteration_maturity":{"enum":["not_started","exploring","improving","plateau_unproven","outcome_reached","constraints_exhausted","plateau_confirmed","budget_exhausted"],"type":["string","null"]},"stopping_evidence":{"type":["string","null"]}},"required":["autonomy","closed_feedback_loop","delivery_state","difficulty","feedback_loop","feedback_loop_coverage","feedback_loop_relevance","feedback_loop_traceability","group","iteration_maturity","stopping_evidence"],"type":"object"},"type":["array","null"]},"intent":{"type":"string"},"plan":{"additionalProperties":false,"properties":{"understands_user_intent":{"enum":["uncertain","partial","clear","complete"],"type":"string"},"user_intention":{"type":"string"}},"required":["understands_user_intent","user_intention"],"type":["object","null"]},"todos":{"items":{"additionalProperties":false,"properties":{"blocker":{"type":["string","null"]},"completion_confidence":{"enum":["speculative","plausible","validated","verified"],"type":["string","null"]},"confidence":{"enum":["speculative","plausible","validated","verified"],"type":"string"},"content":{"type":"string"},"group":{"type":["string","null"]},"id":{"type":"string"},"priority":{"type":"string"},"status":{"enum":["pending","in_progress","blocked","completed","cancelled"],"type":"string"}},"required":["blocker","completion_confidence","confidence","content","group","id","priority","status"],"type":"object"},"type":["array","null"]}},"required":["accept_large_output","goals","intent","plan","todos"],"type":"object"},"strict":true,"type":"function"}"#,
    r#"{"name":"toolbox","parameters":{"properties":{"accept_large_output":{"type":"boolean"},"action":{"enum":["list","describe","run"],"type":"string"},"args":{"type":"object"},"intent":{"type":"string"},"tool":{"type":"string"}},"required":["action","intent"],"type":"object"},"strict":false,"type":"function"}"#,
    r#"{"name":"webfetch","parameters":{"additionalProperties":false,"properties":{"accept_large_output":{"type":["boolean","null"]},"format":{"enum":["text","markdown","html"],"type":["string","null"]},"intent":{"type":"string"},"timeout":{"type":["integer","null"]},"url":{"type":"string"}},"required":["accept_large_output","format","intent","timeout","url"],"type":"object"},"strict":true,"type":"function"}"#,
    r#"{"name":"websearch","parameters":{"additionalProperties":false,"properties":{"accept_large_output":{"type":["boolean","null"]},"bing_market":{"type":["string","null"]},"engine":{"enum":["duckduckgo","bing","searxng","custom"],"type":["string","null"]},"intent":{"type":"string"},"num_results":{"type":["integer","null"]},"query":{"type":"string"}},"required":["accept_large_output","bing_market","engine","intent","num_results","query"],"type":"object"},"strict":true,"type":"function"}"#,
    r#"{"name":"write","parameters":{"additionalProperties":false,"properties":{"accept_large_output":{"type":["boolean","null"]},"content":{"type":"string"},"file_path":{"type":"string"},"intent":{"type":"string"}},"required":["accept_large_output","content","file_path","intent"],"type":"object"},"strict":true,"type":"function"}"#,
];

/// Remove every `description` whose value is a string. A property that is
/// itself named `description` holds a schema object and stays.
fn strip_descriptions(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if map.get("description").is_some_and(Value::is_string) {
                map.remove("description");
            }
            map.values_mut().for_each(strip_descriptions);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_descriptions),
        _ => {}
    }
}

/// The `core` profile's eager schemas keep their shape.
///
/// The observed lines are printed before they are compared, so a deliberate
/// schema change is re-pinned from the test's own output.
#[tokio::test]
async fn eager_tool_schema_shapes_are_pinned() {
    let _guard = crate::storage::lock_test_env();
    // `eval` is eager only where Python is on PATH; pinning it eager keeps the
    // catalog the same on every machine.
    let _env = ProfileEnv::new(
        CORE_TOOL_PROFILE,
        Some("[tools]\nprofile = \"core\"\n\n[tools.load_mode]\neval = \"essential\"\n"),
    );
    let agent = agent_with_mcp_management_tool().await;

    let definitions = agent.tool_definitions_for_debug().await;
    let shapes: Vec<String> = crate::provider::openai_request::build_tools(&definitions)
        .into_iter()
        .map(|mut tool| {
            strip_descriptions(&mut tool);
            tool.to_string()
        })
        .collect();
    for shape in &shapes {
        println!("TOOL_SCHEMA_SHAPE {shape}");
    }

    assert_eq!(shapes.len(), PINNED_SHAPES.len(), "the eager catalog changed size");
    for (shape, pinned) in shapes.iter().zip(PINNED_SHAPES) {
        assert_eq!(
            shape.as_str(),
            *pinned,
            "an eager tool schema changed outside its descriptions"
        );
    }
}
