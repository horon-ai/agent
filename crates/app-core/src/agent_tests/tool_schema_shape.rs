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
    r#"{"name":"edit","parameters":{"additionalProperties":false,"properties":{"accept_large_output":{"type":["boolean","null"]},"edits":{"items":{"additionalProperties":false,"properties":{"new_string":{"type":"string"},"old_string":{"type":"string"},"replace_all":{"type":["boolean","null"]}},"required":["new_string","old_string","replace_all"],"type":"object"},"type":["array","null"]},"file_path":{"type":"string"},"intent":{"type":"string"},"new_string":{"type":["string","null"]},"old_string":{"type":["string","null"]},"replace_all":{"type":["boolean","null"]}},"required":["accept_large_output","edits","file_path","intent","new_string","old_string","replace_all"],"type":"object"},"strict":true,"type":"function"}"#,
    r#"{"name":"ls","parameters":{"additionalProperties":false,"properties":{"accept_large_output":{"type":["boolean","null"]},"ignore":{"items":{"type":"string"},"type":["array","null"]},"intent":{"type":"string"},"path":{"type":["string","null"]}},"required":["accept_large_output","ignore","intent","path"],"type":"object"},"strict":true,"type":"function"}"#,
    r#"{"name":"read","parameters":{"additionalProperties":false,"properties":{"accept_large_output":{"type":["boolean","null"]},"file_path":{"type":"string"},"full":{"type":["boolean","null"]},"hashline":{"type":["boolean","null"]},"intent":{"type":"string"},"limit":{"type":["integer","null"]},"start_line":{"type":["integer","null"]}},"required":["accept_large_output","file_path","full","hashline","intent","limit","start_line"],"type":"object"},"strict":true,"type":"function"}"#,
    r#"{"name":"toolbox","parameters":{"properties":{"accept_large_output":{"type":"boolean"},"action":{"enum":["list","describe","run"],"type":"string"},"args":{"type":"object"},"intent":{"type":"string"},"tool":{"type":"string"}},"required":["action","intent"],"type":"object"},"strict":false,"type":"function"}"#,
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
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
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
