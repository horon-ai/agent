//! The `prefix` measurement: the tool catalog one request sends.
//!
//! The catalog is the part of a request that every turn pays for and no turn
//! changes, so it is the first place a schema regression shows up. The number
//! is printed here, next to the profile logic that produces it, because the
//! gate must read the catalog the agent actually holds rather than a second
//! copy of the filtering rules.

use super::tool_tiers::{ProfileEnv, agent_with_mcp_management_tool};
use crate::config::CORE_TOOL_PROFILE;

/// The bytes one `core`-profile request spends on its tool catalog.
///
/// `build_tools` is the function the provider request body uses, so this is the
/// payload and not an estimate of it. The catalog size is checked against the
/// eager definitions as well: a ceiling that holds only because the catalog
/// lost a tool is a regression the gate must not read as a pass.
#[tokio::test]
async fn core_profile_request_prefix_is_measured() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let agent = agent_with_mcp_management_tool().await;

    let definitions = agent.tool_definitions_for_debug().await;
    let eager = definitions
        .iter()
        .filter(|definition| !definition.defer_loading)
        .count();
    let catalog = crate::provider::openai_request::build_tools(&definitions);
    let bytes = serde_json::to_vec(&catalog).expect("the tool catalog serializes");

    assert_eq!(catalog.len(), eager, "the catalog is exactly the eager definitions");
    assert!(!bytes.is_empty(), "an empty catalog measures nothing");

    println!(
        "STRUCTURAL_PERF section=prefix tools_bytes={} tool_count={} eager_count={}",
        bytes.len(),
        catalog.len(),
        eager,
    );
}
