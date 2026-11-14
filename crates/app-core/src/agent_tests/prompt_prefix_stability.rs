//! The cached static prefix across the life of one session.
//!
//! A provider caches the static system prompt as the head of every request, so
//! two things have to hold. Inside a session the prefix never moves and costs
//! no file read to rebuild, whatever the transcript does and whatever is
//! edited on disk meanwhile. At a session boundary (a clear, another working
//! directory, a restored session) the sources are read again, so an edit is
//! never lost, only deferred to the next boundary.

use sha2::{Digest, Sha256};

use super::prompt_request_bytes::without_clock;
use super::tool_tiers::{ProfileEnv, agent_with_mcp_management_tool};
use super::*;
use crate::config::CORE_TOOL_PROFILE;

/// The `core`-profile static prompt of a session with no prompt files, as the
/// `prompt` measurement reported it before the sources were captured per
/// session. Capturing them must not change one byte of what is sent.
const CORE_STATIC_BYTES: usize = 1367;
const CORE_STATIC_SHA256: &str = "3275b88c172e254adda71a824b51ef1ac25efe4a0f849fcb3fe95e509e3bafbf";

/// Tool calls answered before the prefix is compared for the last time.
const TOOL_ROUND_TRIPS: usize = 3;

/// The pinned prefix is still the prefix after tool calls came back and the
/// transcript was reassembled for the provider, and rebuilding it read no
/// prompt source.
#[tokio::test]
async fn core_static_prefix_is_pinned_across_tool_round_trips() {
    let _guard = crate::storage::lock_test_env();
    let _counters = crate::perf::lock_perf_counters().await;
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let mut agent = agent_with_mcp_management_tool().await;
    agent.tool_definitions().await;

    let first = agent.build_system_prompt_split(None).static_part;
    let pinned = without_clock(&first);
    assert_eq!(
        first.len(),
        CORE_STATIC_BYTES,
        "the static prompt changed size: {first}"
    );
    assert_eq!(
        format!("{:x}", Sha256::digest(pinned.as_bytes())),
        CORE_STATIC_SHA256,
        "the static prompt changed: {first}"
    );

    let before = horon_agent_perf_counters::snapshot();
    agent.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "read the parser".to_string(),
            cache_control: None,
        }],
    );
    for round in 0..TOOL_ROUND_TRIPS {
        let call = format!("call-{round}");
        agent.add_message(
            Role::Assistant,
            vec![ContentBlock::ToolUse {
                id: call.clone(),
                name: "read".to_string(),
                input: serde_json::json!({"file_path": "src/lib.rs"}),
                thought_signature: None,
            }],
        );
        agent.add_message(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: call,
                content: "fn main() {}".to_string(),
                is_error: None,
                verdict: None,
            }],
        );
        agent.tool_definitions().await;
        let again = agent.build_system_prompt_split(None).static_part;
        assert_eq!(
            without_clock(&again),
            pinned,
            "tool round trip {round} moved the static prefix"
        );
    }

    // The path every request takes to rebuild its transcript, with the
    // compaction manager's fit check in it.
    let (messages, _) = agent.messages_for_provider();
    assert!(!messages.is_empty(), "the reassembled transcript carries the turns");
    let reassembled = agent.build_system_prompt_split(None).static_part;
    assert_eq!(
        without_clock(&reassembled),
        pinned,
        "reassembling the transcript moved the static prefix"
    );

    let after = horon_agent_perf_counters::snapshot();
    assert!(
        after.enabled,
        "the counters are off, so the read count measures nothing"
    );
    assert_eq!(
        after.prompt_source_reads - before.prompt_source_reads,
        0,
        "a request of a running session went to disk for a prompt source"
    );
}

/// Puts `HORON_HOME` back when the test ends.
struct RestoreHome(Option<std::ffi::OsString>);

impl Drop for RestoreHome {
    fn drop(&mut self) {
        match self.0.take() {
            Some(home) => crate::env::set_var("HORON_HOME", home),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

fn write_overlay(dir: &std::path::Path, text: &str) {
    std::fs::create_dir_all(dir).expect("create overlay dir");
    std::fs::write(dir.join("prompt-overlay.md"), text).expect("write prompt-overlay.md");
}

fn static_prefix(agent: &Agent) -> String {
    without_clock(&agent.build_system_prompt_split(None).static_part)
}

/// An overlay edited while a session runs does not reach that session's
/// requests, and does reach them after each kind of session boundary.
#[tokio::test]
async fn overlay_edit_applies_at_the_next_session_boundary() {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().expect("temp home");
    let _restore = RestoreHome(std::env::var_os("HORON_HOME"));
    crate::env::set_var("HORON_HOME", home.path());
    crate::config::Config::invalidate_cache();

    let project = tempfile::tempdir().expect("project dir");
    let elsewhere = tempfile::tempdir().expect("second project dir");
    let project_overlay = project.path().join(".horon");
    write_overlay(&project_overlay, "overlay-at-start");
    write_overlay(&elsewhere.path().join(".horon"), "overlay-elsewhere");

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let mut agent = Agent::new_with_initial_working_dir(
        provider,
        Registry::empty(),
        Some(project.path().to_str().expect("utf-8 project path")),
    );
    let started = static_prefix(&agent);
    assert!(started.contains("overlay-at-start"), "{started}");

    // Mid-session edit: the running session keeps the prefix it captured.
    write_overlay(&project_overlay, "overlay-edited-mid-session");
    assert_eq!(static_prefix(&agent), started, "a mid-session edit moved the prefix");

    // Boundary: the conversation is cleared.
    agent.clear();
    let cleared = static_prefix(&agent);
    assert!(cleared.contains("overlay-edited-mid-session"), "{cleared}");
    assert!(!cleared.contains("overlay-at-start"), "{cleared}");

    // Boundary: the session moves to another working directory.
    write_overlay(&project_overlay, "overlay-edited-again");
    assert_eq!(static_prefix(&agent), cleared, "a mid-session edit moved the prefix");
    agent.set_working_dir(elsewhere.path().to_str().expect("utf-8 second project path"));
    let moved = static_prefix(&agent);
    assert!(moved.contains("overlay-elsewhere"), "{moved}");
    assert!(!moved.contains("overlay-edited"), "{moved}");

    // Boundary: another session is restored into this agent. The global
    // overlay applies in every working directory, so the restored session
    // shows it wherever it is bound.
    write_overlay(home.path(), "overlay-global-late");
    assert_eq!(static_prefix(&agent), moved, "a mid-session edit moved the prefix");
    let mut other = Session::create(None, Some("restored session".into()));
    other.save().expect("save the session to restore");
    agent.restore_session(&other.id).expect("restore the saved session");
    let restored = static_prefix(&agent);
    assert!(restored.contains("overlay-global-late"), "{restored}");
}

/// Points `HORON_HOME` at a fresh directory for the rest of the test.
fn isolated_home() -> (tempfile::TempDir, RestoreHome) {
    let home = tempfile::tempdir().expect("temp home");
    let restore = RestoreHome(std::env::var_os("HORON_HOME"));
    crate::env::set_var("HORON_HOME", home.path());
    crate::config::Config::invalidate_cache();
    (home, restore)
}

/// Two requests of one session copy the captured AGENTS.md text and the
/// skills section rendered at the boundary: neither is captured, rendered or
/// replaced again, so both requests read the same buffers.
#[tokio::test]
async fn snapshot_sections_are_shared_not_copied() {
    let _guard = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let project = tempfile::tempdir().expect("project dir");
    std::fs::write(project.path().join("AGENTS.md"), "agents-md-captured-once").expect("write AGENTS.md");

    let provider: Arc<dyn Provider> = Arc::new(NativeAutoCompactionProvider);
    let mut agent = Agent::new_with_initial_working_dir(
        provider,
        Registry::empty(),
        Some(project.path().to_str().expect("utf-8 project path")),
    );
    let skills = [crate::prompt::SkillInfo {
        name: "captured-skill".to_string(),
        description: "Rendered once at the session boundary.".to_string(),
    }];
    agent.prompt_skills_section = crate::prompt::build_available_skills_section(&skills);
    let agents_md = agent.agents_md_snapshot.0.as_deref().map(str::as_ptr);
    let section = agent.prompt_skills_section.as_deref().map(str::as_ptr);
    assert!(agents_md.is_some() && section.is_some(), "both sections are captured");

    let first = agent.build_system_prompt_split(None).static_part;
    let second = agent.build_system_prompt_split(None).static_part;
    assert_eq!(
        without_clock(&first),
        without_clock(&second),
        "the prefix moved between requests"
    );
    assert!(first.contains("agents-md-captured-once"), "{first}");
    assert!(first.contains("captured-skill"), "{first}");
    assert_eq!(agent.agents_md_snapshot.0.as_deref().map(str::as_ptr), agents_md);
    assert_eq!(agent.prompt_skills_section.as_deref().map(str::as_ptr), section);
}

/// A skills list too large for its budget reaches the prompt exactly as the
/// section renderer cut it, after the base prompt and the one `\n\n`
/// separator a join of the parts would have put there.
#[test]
fn oversized_skills_section_is_unchanged() {
    let _guard = crate::storage::lock_test_env();
    let (_home, _restore) = isolated_home();
    let project = tempfile::tempdir().expect("project dir");
    let dir = Some(project.path());

    let skills: Vec<crate::prompt::SkillInfo> = (0..200)
        .map(|index| crate::prompt::SkillInfo {
            name: format!("skill-{index:03}-{}", "n".repeat(300)),
            description: "d".repeat(200),
        })
        .collect();
    let uncapped: usize = skills.iter().map(|skill| skill.name.len()).sum();
    assert!(
        uncapped > crate::prompt::SKILLS_SECTION_MAX_BYTES,
        "fixture must exceed the budget"
    );
    let section = crate::prompt::build_available_skills_section(&skills).expect("skills section");
    assert!(
        section.len() <= crate::prompt::SKILLS_SECTION_MAX_BYTES,
        "{}",
        section.len()
    );

    let sources = crate::prompt::StaticPromptSources::load(dir);
    let (split, info) = crate::prompt::build_system_prompt_split_with_snapshot(
        Some("skill body"),
        Some(&section),
        Some("memory"),
        dir,
        &(None, crate::prompt::ContextInfo::default()),
        &sources,
    );
    let expected = format!("{}\n\n{section}", crate::prompt::load_base_system_prompt(dir));
    assert_eq!(without_clock(&split.static_part), without_clock(&expected));
    assert_eq!(split.dynamic_part, "memory\n\n# Active Skill\n\nskill body");
    assert_eq!(info.skills_chars, section.len());
    assert_eq!(info.total_chars, split.static_part.len() + split.dynamic_part.len());
}

/// Where the stored index section and the cached static estimate live, so a
/// test can tell a reused buffer from a new render or a new estimate.
fn index_and_estimate_buffers(agent: &Agent) -> (Option<*const u8>, Option<*const u8>) {
    let index = agent
        .listed_tools_index
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .and_then(|index| index.section.as_deref().map(str::as_ptr));
    let estimate = agent
        .static_token_estimate
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .map(|(text, _)| text.as_ptr());
    (index, estimate)
}

/// Requests after the index froze reuse the section rendered when it was
/// stored and the static estimate made for the first of them.
#[tokio::test]
async fn index_section_renders_once_after_freeze() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let mut agent = agent_with_mcp_management_tool().await;
    let tools = agent.tool_definitions().await;
    let first = agent.build_system_prompt_split(None);
    agent.log_prompt_prefix_accounting(&first, &tools);
    let rendered = index_and_estimate_buffers(&agent);
    assert!(
        rendered.0.is_some() && rendered.1.is_some(),
        "a core session lists tools"
    );
    let section = agent.listed_tools_section().expect("a core session lists its tools");
    assert!(first.static_part.contains(&section), "{}", first.static_part);

    for request in 0..5 {
        let tools = agent.tool_definitions().await;
        let split = agent.build_system_prompt_split(None);
        agent.log_prompt_prefix_accounting(&split, &tools);
        assert_eq!(
            split.static_part, first.static_part,
            "request {request} moved the prefix"
        );
        assert_eq!(
            index_and_estimate_buffers(&agent),
            rendered,
            "request {request} rendered or estimated again"
        );
    }
}

/// A restored session drops the stored index with its rendered section, and
/// the next request stores and renders a new one.
#[tokio::test]
async fn restored_session_rebuilds_the_index_cache() {
    let _guard = crate::storage::lock_test_env();
    let _env = ProfileEnv::new(CORE_TOOL_PROFILE, None);
    let mut agent = agent_with_mcp_management_tool().await;
    agent.tool_definitions().await;
    let before = agent.listed_tools_section().expect("a core session lists its tools");

    let mut other = Session::create(None, Some("restored session".into()));
    other.save().expect("save the session to restore");
    agent.restore_session(&other.id).expect("restore the saved session");
    assert!(!agent.listed_tools_index_built(), "the restore kept the old index");
    assert_eq!(index_and_estimate_buffers(&agent).0, None);

    agent.tool_definitions().await;
    assert!(agent.listed_tools_index_built(), "the next request stored no index");
    assert_eq!(agent.listed_tools_section().as_deref(), Some(before.as_str()));
}
