//! Tests for the `generate_image` tool adapter.
//!
//! The transport and the storage layer are covered in their own crates. What is
//! tested here is the adapter's own contract: the schema the model sees, the
//! effects that decide approval and concurrency, the input coercion, and the
//! registration gate.

use super::*;
use horon_agent_tool_core::ToolEffects;
use serde_json::json;

fn tool() -> GenerateImageTool {
    GenerateImageTool::new()
}

#[test]
fn the_tool_declares_write_and_network() {
    let effects = tool().effects();
    // `WRITE` is the approval tier (a call spends money and writes a file);
    // `NETWORK` is the outbound request. Both must be present, because either
    // alone would let the call through a tier that was meant to stop it.
    assert_eq!(effects, ToolEffects::write().union(ToolEffects::network()));
    assert!(effects.writes(), "the call writes an image");
    assert!(effects.networks(), "the call leaves the process");
    assert!(
        !effects.parallel_safe(),
        "a spending mutation must not run beside another mutating call"
    );
}

#[test]
fn the_schema_requires_a_prompt_and_bounds_the_rest() {
    let schema = tool().parameters_schema();
    assert_eq!(schema["required"], json!(["prompt"]));
    let properties = &schema["properties"];
    assert!(properties["prompt"]["description"].is_string());
    assert_eq!(properties["size"]["enum"], json!(image_tool::IMAGE_SIZES));
    assert_eq!(properties["count"]["minimum"], 1);
    assert_eq!(properties["count"]["maximum"], image_tool::MAX_IMAGES_PER_CALL);
    assert_eq!(properties["count"]["maximum"], 4);
    assert_eq!(
        properties["intent"]["type"], "string",
        "every tool exposes the shared intent field"
    );
}

#[test]
fn the_name_matches_the_registry_key() {
    assert_eq!(tool().name(), GENERATE_IMAGE_TOOL);
    assert_eq!(GENERATE_IMAGE_TOOL, "generate_image");
}

#[test]
fn an_input_defaults_to_one_square_image() {
    let call = serde_json::from_value::<GenerateImageInput>(json!({ "prompt": "a cat" }))
        .expect("the minimum input parses")
        .into_call()
        .expect("the call is valid");
    assert_eq!(call.size, "1024x1024");
    assert_eq!(call.count, 1);
    assert!(call.input.is_empty());
    assert!(call.model.is_none());
    assert_eq!(call.prompt, "a cat");
}

#[test]
fn a_count_above_the_limit_is_clamped_rather_than_refused() {
    // The schema already states the maximum, so a model that asks for more is
    // corrected instead of failed: the useful part of the request survives.
    let call = serde_json::from_value::<GenerateImageInput>(json!({ "prompt": "a cat", "count": 99 }))
        .expect("parses")
        .into_call()
        .expect("valid after clamping");
    assert_eq!(call.count, image_tool::MAX_IMAGES_PER_CALL);
}

#[test]
fn a_zero_count_becomes_one() {
    let call = serde_json::from_value::<GenerateImageInput>(json!({ "prompt": "a cat", "count": 0 }))
        .expect("parses")
        .into_call()
        .expect("valid after clamping");
    assert_eq!(call.count, 1);
}

#[test]
fn an_empty_prompt_is_refused() {
    let error = serde_json::from_value::<GenerateImageInput>(json!({ "prompt": "   " }))
        .expect("parses")
        .into_call()
        .expect_err("a blank prompt is refused");
    assert!(format!("{error:#}").contains("empty"));
}

#[test]
fn an_oversized_prompt_is_refused_before_a_request() {
    let prompt = "x".repeat(image_tool::MAX_PROMPT_BYTES + 1);
    let error = serde_json::from_value::<GenerateImageInput>(json!({ "prompt": prompt }))
        .expect("parses")
        .into_call()
        .expect_err("an oversized prompt is refused");
    let message = format!("{error:#}");
    assert!(message.contains("above"), "{message}");
    assert!(message.contains(&image_tool::MAX_PROMPT_BYTES.to_string()), "{message}");
}

#[test]
fn a_missing_prompt_does_not_parse() {
    let error = serde_json::from_value::<GenerateImageInput>(json!({}))
        .expect_err("prompt is required by the schema and by deserialization");
    assert!(error.to_string().contains("prompt"));
}

#[test]
fn an_input_path_list_travels_intact() {
    let call = serde_json::from_value::<GenerateImageInput>(json!({
        "prompt": "make it night",
        "input": ["a.png", "b.png"]
    }))
    .expect("parses")
    .into_call()
    .expect("valid");
    assert_eq!(call.input, vec!["a.png".to_string(), "b.png".to_string()]);
}

#[test]
fn an_explicit_model_overrides_the_role() {
    let call = serde_json::from_value::<GenerateImageInput>(json!({
        "prompt": "a cat",
        "model": "other-image-model"
    }))
    .expect("parses")
    .into_call()
    .expect("valid");
    assert_eq!(call.model.as_deref(), Some("other-image-model"));
}

#[test]
fn enabling_needs_both_the_switch_and_a_role() {
    // Neither condition is enough on its own. Asserting the two-part gate here
    // keeps a future change from registering a tool whose every call must fail
    // (switch on, no role) or one that was never asked for (role on, switch off).
    // `enabled()` reads the live config and the process environment, so the
    // assertion is on the shape of the gate: both helpers exist and `enabled`
    // is their conjunction.
    let switch = crate::config::config().tools.generate_image;
    let role = configured();
    assert_eq!(enabled(), switch && role);
}
