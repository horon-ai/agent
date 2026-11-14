//! The `prompt` measurement: the static system prompt one request sends.
//!
//! The static part is the other half of the cached request prefix, beside the
//! tool catalog. It is measured through the agent's own builder, after the tool
//! list is locked, because that is the text a provider receives; a second copy
//! of the assembly rules would measure something else.

use sha2::{Digest, Sha256};

use super::tool_tiers::{ProfileEnv, agent_with_mcp_management_tool};
use crate::config::CORE_TOOL_PROFILE;

/// Requests built after the first one, with nothing changed between them.
const UNCHANGED_REQUESTS: usize = 9;

/// The static part without its clock line, the one line allowed to differ
/// between two builds of a session that changed nothing.
fn without_clock(static_part: &str) -> String {
    static_part
        .lines()
        .filter(|line| !line.starts_with("Time: "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// What one `core`-profile request spends on its static prompt, and what it
/// costs to build that prompt again when nothing changed.
///
/// `static_bytes` and `static_tokens` describe the text as sent.
/// `static_sha256` is taken over the text without its clock line, so it names
/// the prefix and not the minute it was built in. `warm_source_reads` is the
/// source-file probes of the first build; `unchanged_request_source_reads` is
/// the probes of the next [`UNCHANGED_REQUESTS`] builds together. Probes count
/// missing files too, so a session with no prompt files still measures above
/// zero and a line of zeros means the counters were off.
///
/// The numbers are reported, not asserted: the structural gate owns the
/// bounds. What is asserted is that the prefix is byte-stable across requests.
#[tokio::test]
async fn core_profile_static_prompt_is_measured() {
    let _guard = crate::storage::lock_test_env();
    let _counters = crate::perf::lock_perf_counters().await;
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let mut agent = agent_with_mcp_management_tool().await;
    // The index is fixed when the tool list locks, so the prompt is read after it.
    agent.tool_definitions().await;

    let cold = horon_agent_perf_counters::snapshot();
    let first = agent.build_system_prompt_split(None).static_part;
    let warm = horon_agent_perf_counters::snapshot();
    let stable = without_clock(&first);
    for request in 0..UNCHANGED_REQUESTS {
        let again = agent.build_system_prompt_split(None).static_part;
        assert_eq!(
            without_clock(&again),
            stable,
            "unchanged request {request} moved the static prefix"
        );
    }
    let after = horon_agent_perf_counters::snapshot();

    assert!(
        after.enabled,
        "the counters are off, so the reads below measure nothing"
    );
    assert!(!first.is_empty(), "an empty static prompt measures nothing");

    let index = agent.listed_tools_section().unwrap_or_default();
    let clipped_rows = agent
        .build_listed_tools_index()
        .await
        .iter()
        .filter(|(_, description)| {
            description.ends_with("...") && description.chars().count() == crate::prompt::ONE_LINE_DESCRIPTION_MAX_CHARS
        })
        .count();

    println!(
        "STRUCTURAL_PERF section=prompt static_bytes={} static_sha256={:x} static_tokens={} index_bytes={} \
         index_clipped_rows={} warm_source_reads={} unchanged_request_source_reads={}",
        first.len(),
        Sha256::digest(stable.as_bytes()),
        crate::util::estimate_tokens(&first),
        index.len(),
        clipped_rows,
        warm.prompt_source_reads - cold.prompt_source_reads,
        after.prompt_source_reads - warm.prompt_source_reads,
    );
}
