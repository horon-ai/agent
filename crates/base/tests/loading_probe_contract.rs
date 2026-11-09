//! Contract for `loading_probe`: the steps run in their declared order, the
//! loading counters are present and non-zero where the real work happens, a warm
//! load reports the same bytes on every run, and the exit codes say which kind
//! of failure happened.
//!
//! Loading a session is not read-only: it publishes `checkpoint.json`,
//! `sparse-index.jsonl`, `summary.json`, `facade-cache.json` and `.writer.lock`
//! into the measured session directory and the catalog files into
//! `<home>/sessions/`. `events.jsonl` itself is never rewritten. So the first
//! load of a fixture is a different sample from the second: the fixture here is
//! stripped of every derived file first (COLD), then three runs rebuild the
//! caches (WARM, discarded), and the two runs after that must agree byte for
//! byte on the fixture stages. The save step is left out of that claim because it
//! recreates the scratch session, whose log carries timestamps. A cold load is
//! not byte-identical across runs either, because the catalog index it writes
//! carries timestamps; only the warm steady state is exact.
//!
//! ```text
//! cargo test --locked -p horon-agent-base loading_probe_contract -- --nocapture --test-threads=1
//! ```

#[path = "../examples/loading_probe/steps.rs"]
mod steps;

use horon_agent_base::message::{ContentBlock, Role};
use horon_agent_base::session::Session;
use horon_agent_session_store as store;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Mutex;

const FIXTURE_SESSION: &str = "loading_probe_fixture";
const FIXTURE_MESSAGES: usize = 40;
const FIXTURE_SKILLS: usize = 3;

/// Serializes tests that mutate `HORON_HOME`. This test binary has its own
/// address space, so a file-local mutex is sufficient (and avoids depending on
/// the `test-support`-gated `storage::lock_test_env`).
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set_path(key: &'static str, value: &Path) -> Self {
        let previous = std::env::var_os(key);
        horon_agent_base::env::set_var(key, value);
        Self { key, previous }
    }

    fn unset(key: &'static str) -> Self {
        let previous = std::env::var_os(key);
        horon_agent_base::env::remove_var(key);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => horon_agent_base::env::set_var(self.key, value),
            None => horon_agent_base::env::remove_var(self.key),
        }
    }
}

fn text(value: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: value.into(),
        cache_control: None,
    }]
}

fn write_skills(home: &Path, count: usize) {
    let root = home.join("skills");
    for index in 0..count {
        let name = format!("fixture-skill-{index:04}");
        let directory = root.join(&name);
        std::fs::create_dir_all(&directory).expect("skill dir");
        std::fs::write(
            directory.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: fixture skill {index} for the loading probe\n---\n\nBody of {name}.\n"
            ),
        )
        .expect("skill file");
    }
}

/// Strip every file the loading path published, so the next load is a first load.
fn strip_derived(session_dir: &Path) {
    for entry in std::fs::read_dir(session_dir).expect("session dir") {
        let path = entry.expect("session entry").path();
        if path.file_name().and_then(|name| name.to_str()) == Some("events.jsonl") {
            continue;
        }
        if path.is_file() {
            std::fs::remove_file(&path).expect("remove derived file");
        }
    }
}

fn write_fixture(home: &Path) -> (usize, String) {
    // The MCP config stage reads a user-level Claude file, and the future MCP
    // source table gates that read on `[mcp] claude_user`. Both belong to the
    // fixture so the stage has something real to parse.
    std::fs::write(home.join("config.toml"), "[mcp]\nclaude_user = true\n").expect("config file");
    // `user_home_path` sandboxes user-level files under `<HORON_HOME>/external`.
    let external = home.join("external");
    std::fs::create_dir_all(&external).expect("external home");
    std::fs::write(
        external.join(".claude.json"),
        r#"{"mcpServers":{"fixture-claude":{"command":"true"}}}"#,
    )
    .expect("claude json");

    let mut session = Session::create_with_id(FIXTURE_SESSION.into(), None, None);
    for index in 0..FIXTURE_MESSAGES {
        let role = if index % 2 == 0 { Role::User } else { Role::Assistant };
        session.add_message(role, text(&format!("fixture message {index}")));
    }
    session.save().expect("fixture save");

    let session_dir = home.join("sessions").join(FIXTURE_SESSION);
    strip_derived(&session_dir);
    let log = session_dir.join("events.jsonl");
    let bytes = std::fs::read(&log).expect("fixture log");
    let records = bytes.iter().filter(|byte| **byte == b'\n').count();
    assert!(
        records > FIXTURE_MESSAGES,
        "one session_created record plus one record per message, saw {records}"
    );
    (records, store::sha256_hex(&bytes))
}

fn stage<'a>(report: &'a Value, name: &str) -> &'a Value {
    report
        .get("stages")
        .and_then(Value::as_array)
        .and_then(|stages| {
            stages
                .iter()
                .find(|stage| stage.get("name").and_then(Value::as_str) == Some(name))
        })
        .unwrap_or_else(|| panic!("missing stage {name}"))
}

fn delta(stage: &Value, counter: &str) -> u64 {
    stage
        .get("delta")
        .and_then(|delta| delta.get(counter))
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("missing counter {counter} in {stage}"))
}

fn options(home: &Path, records: usize, skill_count: usize) -> steps::Options {
    steps::Options {
        home: home.to_path_buf(),
        session: FIXTURE_SESSION.into(),
        expect_events: records,
        expect_messages: FIXTURE_MESSAGES,
        skill_count,
        scratch_messages: 8,
    }
}

#[test]
fn loading_probe_contract() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let temp = tempfile::TempDir::new().expect("temp home");
    let _home = EnvVarGuard::set_path("HORON_HOME", temp.path());
    let _real_home = EnvVarGuard::set_path("HOME", temp.path());
    let _claude_switch = EnvVarGuard::unset("HORON_AGENT_DISABLE_CLAUDE_MCP");
    write_skills(temp.path(), FIXTURE_SKILLS);
    let (records, log_sha) = write_fixture(temp.path());

    horon_agent_perf_counters::reset();
    let options = options(temp.path(), records, FIXTURE_SKILLS);
    let report = steps::run(&options).expect("probe runs");

    assert_eq!(
        report.get("outcome").and_then(Value::as_str),
        Some("ok"),
        "probe failed: {}",
        report.get("failures").cloned().unwrap_or(Value::Null)
    );
    assert_eq!(
        report.get("exit_code").and_then(Value::as_i64),
        Some(0),
        "a clean run must exit 0"
    );
    let order: Vec<&str> = report
        .get("order")
        .and_then(Value::as_array)
        .expect("order list")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(order, steps::ORDER, "steps must run in their declared order");
    let ran: Vec<&str> = report
        .get("stages")
        .and_then(Value::as_array)
        .expect("stages")
        .iter()
        .filter_map(|stage| stage.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(ran, steps::ORDER, "recorded steps must equal the declared order");

    let counters = report.get("counters").expect("counters");
    assert_eq!(counters.get("enabled").and_then(Value::as_bool), Some(true));
    for counter in [
        "store_read_bytes",
        "full_source_hashes",
        "full_source_hash_bytes",
        "request_deep_copies",
        "request_deep_copy_bytes",
        "request_messages_copied",
        "skill_dir_reads",
        "skill_file_reads",
        "skill_bytes_read",
        "mcp_config_reads",
        "mcp_config_bytes",
    ] {
        assert!(
            counters.get(counter).and_then(Value::as_u64).is_some(),
            "snapshot must carry {counter}"
        );
    }

    let skills = stage(&report, "skill_catalog");
    assert_eq!(
        skills.pointer("/detail/skills").and_then(Value::as_u64),
        Some(FIXTURE_SKILLS as u64)
    );
    assert!(
        delta(skills, "skill_dir_reads") >= 1,
        "catalog discovery must enumerate a skill directory"
    );

    let overlay = stage(&report, "skill_request_overlay");
    assert_eq!(
        overlay.pointer("/detail/requests").and_then(Value::as_u64),
        Some(steps::REPEAT_REQUESTS as u64)
    );
    assert!(
        overlay.pointer("/detail/skills").and_then(Value::as_u64).unwrap_or(0) >= (FIXTURE_SKILLS * 2) as u64,
        "the composed request registry must carry both the global and the project skills"
    );
    for counter in [
        "skill_file_reads",
        "skill_bytes_read",
        "mcp_config_reads",
        "mcp_config_bytes",
    ] {
        assert!(
            overlay
                .get("delta")
                .and_then(|delta| delta.get(counter))
                .and_then(Value::as_u64)
                .is_some(),
            "the repeated request stage must report {counter}"
        );
    }

    let mcp = stage(&report, "mcp_config_load");
    assert_eq!(
        mcp.pointer("/detail/requests").and_then(Value::as_u64),
        Some(steps::REPEAT_REQUESTS as u64)
    );
    assert_eq!(
        mcp.pointer("/detail/servers").and_then(Value::as_u64),
        Some(1),
        "the ~/.claude.json fixture server must survive the merge"
    );

    let attach = stage(&report, "attach_history");
    assert_eq!(
        attach.pointer("/detail/history").and_then(Value::as_u64),
        Some(FIXTURE_MESSAGES as u64)
    );
    assert!(delta(attach, "full_source_hashes") >= 1, "a full load digests the log");
    let log_bytes = report
        .pointer("/fixture/log_bytes")
        .and_then(Value::as_u64)
        .expect("fixture log bytes");
    assert!(
        delta(attach, "store_read_bytes") >= log_bytes,
        "a full load reads at least the whole log"
    );
    let cold_attach_read = delta(attach, "store_read_bytes");
    let cold_attach_hashes = delta(attach, "full_source_hashes");

    let resume = stage(&report, "resume_ready");
    assert!(
        delta(resume, "store_read_bytes") > 0,
        "the startup stub still reads the store"
    );

    let dispatch = stage(&report, "provider_dispatch");
    assert_eq!(
        dispatch.pointer("/detail/messages").and_then(Value::as_u64),
        Some(FIXTURE_MESSAGES as u64)
    );
    assert_eq!(delta(dispatch, "request_deep_copies"), 1, "one owned request snapshot");
    assert_eq!(delta(dispatch, "request_messages_copied"), FIXTURE_MESSAGES as u64);
    assert!(
        delta(dispatch, "request_deep_copy_bytes") > 0,
        "the owned snapshot duplicates payload bytes"
    );

    let save = stage(&report, "session_save");
    assert_eq!(save.pointer("/detail/reloaded").and_then(Value::as_u64), Some(8));
    assert!(
        delta(save, "store_read_bytes") > 0 && delta(save, "full_source_hashes") >= 1,
        "a real save validates through the store"
    );

    assert_eq!(
        report.pointer("/fixture/log_sha256").and_then(Value::as_str),
        Some(log_sha.as_str())
    );
    assert_eq!(
        report.pointer("/fixture/unchanged").and_then(Value::as_bool),
        Some(true),
        "the measured fixture log must not be rewritten"
    );

    let session = Session::load(FIXTURE_SESSION).expect("fixture reloads");
    let mut session = session;
    let first = serde_json::to_vec(&session.messages_for_provider()).expect("snapshot serializes");
    session.release_provider_messages_cache();
    let second = serde_json::to_vec(&session.messages_for_provider()).expect("snapshot serializes");
    assert_eq!(
        first, second,
        "the request snapshot bytes must not change between materializations"
    );

    let second_report = steps::run(&options).expect("probe reruns");
    assert_eq!(
        second_report.pointer("/fixture/log_sha256").and_then(Value::as_str),
        Some(log_sha.as_str())
    );
    assert_eq!(
        second_report
            .pointer("/fixture/log_sha256_after")
            .and_then(Value::as_str),
        Some(log_sha.as_str()),
        "a second run reads the same fixture bytes"
    );

    let warmed = steps::run(&options).expect("probe reruns");
    let warm_attach = stage(&warmed, "attach_history");
    assert!(
        delta(warm_attach, "store_read_bytes") < cold_attach_read,
        "after the caches exist the load reads less than the first load"
    );
    assert!(
        delta(warm_attach, "full_source_hashes") < cold_attach_hashes,
        "the first load digests the log; a cached load does not"
    );

    let warm_a = steps::run(&options).expect("probe reruns");
    let warm_b = steps::run(&options).expect("probe reruns");
    for name in ["attach_history", "resume_ready", "provider_dispatch"] {
        for counter in [
            "store_read_bytes",
            "full_source_hashes",
            "full_source_hash_bytes",
            "hash_bytes_read",
            "io_read_bytes",
        ] {
            assert_eq!(
                delta(stage(&warm_a, name), counter),
                delta(stage(&warm_b, name), counter),
                "{name} {counter} must be identical on two warm runs; only then is a \
                 warm sample a stable measurement"
            );
        }
    }
    assert_eq!(
        delta(stage(&warm_a, "attach_history"), "store_read_bytes"),
        delta(warm_attach, "store_read_bytes"),
        "the first warm run after the caches exist is already the steady state"
    );
}

#[test]
fn loading_probe_contract_exit_codes() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    assert_eq!(steps::exit_code_for(&[]), steps::EXIT_OK);
    assert_eq!(
        steps::exit_code_for(&[steps::FailureKind::Mismatch]),
        steps::EXIT_MISMATCH
    );
    assert_eq!(
        steps::exit_code_for(&[steps::FailureKind::Mismatch, steps::FailureKind::Preflight]),
        steps::EXIT_PREFLIGHT,
        "preflight outranks mismatch: there was nothing to measure"
    );

    let temp = tempfile::TempDir::new().expect("temp home");
    let _home = EnvVarGuard::set_path("HORON_HOME", temp.path());
    let _real_home = EnvVarGuard::set_path("HOME", temp.path());
    write_skills(temp.path(), FIXTURE_SKILLS);
    let (records, _log_sha) = write_fixture(temp.path());

    let mismatch = steps::run(&options(temp.path(), records, FIXTURE_SKILLS + 1)).expect("probe runs");
    assert_eq!(mismatch.get("outcome").and_then(Value::as_str), Some("failure"));
    assert_eq!(
        mismatch.get("exit_code").and_then(Value::as_i64),
        Some(i64::from(steps::EXIT_MISMATCH)),
        "a count that does not match is a mismatch, not a preflight failure"
    );
    assert_eq!(mismatch.get("failure_kinds").cloned(), Some(json!(["mismatch"])));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let sessions = temp.path().join("sessions");
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755)).expect("widen home");
        std::fs::set_permissions(&sessions, std::fs::Permissions::from_mode(0o755)).expect("widen sessions");

        let preflight = steps::run(&options(temp.path(), records, FIXTURE_SKILLS)).expect("probe runs");
        assert_eq!(preflight.get("outcome").and_then(Value::as_str), Some("failure"));
        assert_eq!(
            preflight.get("exit_code").and_then(Value::as_i64),
            Some(i64::from(steps::EXIT_PREFLIGHT)),
            "a non-private path is a preflight failure"
        );
        assert_eq!(preflight.get("failure_kinds").cloned(), Some(json!(["preflight"])));
        let failures = preflight.get("failures").and_then(Value::as_array).expect("failures");
        assert!(
            failures
                .iter()
                .any(|failure| failure.as_str().is_some_and(|text| text.contains("unsafe"))),
            "the store refusing the path or the file identity is what makes this a preflight failure"
        );
    }
}
