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
const PINNED_SHAPES: &[&str] = &[];

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
