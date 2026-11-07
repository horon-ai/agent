//! Shared loading-probe steps, called by the `loading_probe` example and by the
//! `loading_probe_contract` integration test.
//!
//! The steps are the real loading and preparation paths, in a fixed order:
//! skill catalog discovery, repeated request overlays and an MCP config load,
//! a full session load, a startup-stub load, the owned request snapshot, and a
//! real save. Each step reports its own wall time and the process counters it
//! moved, so a caller can tell which part of a slow preparation paid.
//!
//! The load path is not read-only, so repeated runs of the same fixture are not
//! interchangeable samples. Loading the measured session publishes derived
//! caches into its directory (`checkpoint.json`, `sparse-index.jsonl`,
//! `summary.json`, `facade-cache.json`, `.writer.lock`) and the catalog files
//! (`.catalog.index.jsonl`, `.catalog.lock`, `.catalog.dirty`, `.origin-id`) into
//! `<home>/sessions/`; `events.jsonl` itself is never rewritten. A first load on
//! a fresh home therefore reads strictly more than a load that finds those
//! caches, which is why `scripts/perf/bench_loading.py` samples COLD (fresh home per
//! run) and WARM (discarded warmups, then one home) separately and never mixes
//! them. The scratch save is this probe's own session, never the fixture.

use anyhow::{Context, Result, bail, ensure};
use horon_agent_base::mcp::McpConfig;
use horon_agent_base::message::{ContentBlock, Message, Role};
use horon_agent_base::session::Session;
use horon_agent_base::skill::SkillRegistry;
use horon_agent_perf_counters::{Snapshot, snapshot};
use horon_agent_session_store as store;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Fixed step order. Callers assert it instead of trusting the JSON order.
pub const ORDER: [&str; 7] = [
    "skill_catalog",
    "skill_request_overlay",
    "mcp_config_load",
    "attach_history",
    "resume_ready",
    "provider_dispatch",
    "session_save",
];

/// Request boundaries the overlay and MCP stages measure after their warmup.
pub const REPEAT_REQUESTS: usize = 9;

/// Project workspace the overlay and MCP stages resolve against.
pub const PROJECT_DIR: &str = "loading_probe_project";

/// Scratch session the save step owns; never the measured fixture.
pub const SCRATCH_SESSION: &str = "loading_probe_scratch";

/// Every step ran and every counter was reported.
pub const EXIT_OK: i32 = 0;
/// A counter or fixture expectation did not match what the run produced.
pub const EXIT_MISMATCH: i32 = 1;
/// The probe could not measure: bad arguments, home mismatch, an unsafe or
/// non-private path, or counters disabled.
pub const EXIT_PREFLIGHT: i32 = 2;

/// Why a run is not `EXIT_OK`. The probe's exit code is the strongest kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// A measured value or a fixture expectation disagreed with the run.
    Mismatch,
    /// The probe never got a measurement to judge.
    Preflight,
}

impl FailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mismatch => "mismatch",
            Self::Preflight => "preflight",
        }
    }
}

/// Exit code for the failures a run recorded: preflight outranks mismatch.
pub fn exit_code_for(kinds: &[FailureKind]) -> i32 {
    if kinds.is_empty() {
        EXIT_OK
    } else if kinds.contains(&FailureKind::Preflight) {
        EXIT_PREFLIGHT
    } else {
        EXIT_MISMATCH
    }
}

/// A store error that says "unsafe" refused the path or the file identity itself
/// (`unsafe_path`, or "unsafe file identity or permissions" when a directory is
/// not exactly owner-private), so no measurement was taken. Every other step
/// failure is a value the run did produce and did not match.
fn classify_failure(message: &str) -> FailureKind {
    if message.contains("unsafe") {
        FailureKind::Preflight
    } else {
        FailureKind::Mismatch
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// The home reported in the output; the process environment selects the
    /// home the loading APIs actually read.
    pub home: PathBuf,
    pub session: String,
    /// Records (`session_created` included) in the fixture log; 0 means the
    /// fixture has no log at all.
    pub expect_events: usize,
    /// Messages the loading paths must return. A saved session log also carries
    /// metadata records, so this is never derived from `expect_events`.
    pub expect_messages: usize,
    pub skill_count: usize,
    pub scratch_messages: usize,
}

impl Options {
    pub fn expected_messages(&self) -> usize {
        self.expect_messages
    }
}

fn text(value: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text {
        text: value.into(),
        cache_control: None,
    }]
}

fn delta(before: &Snapshot, after: &Snapshot) -> Value {
    json!({
        "store_read_bytes": after.store_read_bytes.saturating_sub(before.store_read_bytes),
        "full_source_hashes": after.full_source_hashes.saturating_sub(before.full_source_hashes),
        "full_source_hash_bytes": after.full_source_hash_bytes.saturating_sub(before.full_source_hash_bytes),
        "request_deep_copies": after.request_deep_copies.saturating_sub(before.request_deep_copies),
        "request_deep_copy_bytes": after.request_deep_copy_bytes.saturating_sub(before.request_deep_copy_bytes),
        "request_messages_copied": after.request_messages_copied.saturating_sub(before.request_messages_copied),
        "skill_dir_reads": after.skill_dir_reads.saturating_sub(before.skill_dir_reads),
        "skill_file_reads": after.skill_file_reads.saturating_sub(before.skill_file_reads),
        "skill_bytes_read": after.skill_bytes_read.saturating_sub(before.skill_bytes_read),
        "mcp_config_reads": after.mcp_config_reads.saturating_sub(before.mcp_config_reads),
        "mcp_config_bytes": after.mcp_config_bytes.saturating_sub(before.mcp_config_bytes),
        "io_read_bytes": after.io_read_bytes.saturating_sub(before.io_read_bytes),
        "io_write_bytes": after.io_write_bytes.saturating_sub(before.io_write_bytes),
        "hash_bytes_read": after.hash_bytes_read.saturating_sub(before.hash_bytes_read),
        "blob_bytes_read": after.blob_bytes_read.saturating_sub(before.blob_bytes_read),
    })
}

fn build_identity() -> Value {
    let flags = [
        ("embeddings", cfg!(feature = "embeddings")),
        ("jemalloc", cfg!(feature = "jemalloc")),
        ("jemalloc-prof", cfg!(feature = "jemalloc-prof")),
        ("test-support", cfg!(feature = "test-support")),
    ];
    let features: Vec<&str> = flags
        .into_iter()
        .filter_map(|(name, enabled)| enabled.then_some(name))
        .collect();
    json!({
        "profile": horon_agent_build_meta::build_profile(),
        "features": features,
        "git_hash": horon_agent_build_meta::git_hash(),
        "release_build": horon_agent_build_meta::is_release_build(),
    })
}

fn log_path(home: &Path, session: &str) -> PathBuf {
    home.join("sessions").join(session).join("events.jsonl")
}

fn log_sha256(path: &Path) -> Result<Option<String>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(store::sha256_hex(&bytes))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn log_bytes(path: &Path) -> Result<Option<u64>> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata.len())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

struct Stages {
    entries: Vec<Value>,
    failures: Vec<String>,
    kinds: Vec<FailureKind>,
}

impl Stages {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            failures: Vec::new(),
            kinds: Vec::new(),
        }
    }

    fn fail(&mut self, kind: FailureKind, message: String) {
        self.failures.push(message);
        if !self.kinds.contains(&kind) {
            self.kinds.push(kind);
        }
    }

    /// Run one step, recording its wall time and the counters it moved.
    ///
    /// A failing step is recorded and the next step still runs: the caller gets
    /// every failure of the run rather than only the first.
    fn stage(&mut self, name: &'static str, body: impl FnOnce() -> Result<Value>) {
        let before = snapshot();
        let started = Instant::now();
        let outcome = body();
        let millis = started.elapsed().as_secs_f64() * 1000.0;
        let after = snapshot();
        let moved = delta(&before, &after);
        let entry = match outcome {
            Ok(detail) => json!({
                "name": name,
                "ms": millis,
                "ok": true,
                "detail": detail,
                "delta": moved,
            }),
            Err(error) => {
                let message = format!("{name}: {error:#}");
                self.fail(classify_failure(&message), message.clone());
                json!({
                    "name": name,
                    "ms": millis,
                    "ok": false,
                    "failure": message,
                    "delta": moved,
                })
            }
        };
        self.entries.push(entry);
    }
}

/// Run every step once and return the probe report.
///
/// The caller must have `HORON_AGENT_HOME` pointing at the fixture home; that is
/// what the loading APIs read.
pub fn run(options: &Options) -> Result<Value> {
    let home = horon_agent_base::storage::horon_agent_dir()?;
    ensure!(
        home == options.home,
        "HORON_AGENT_HOME resolves to {} but the probe was told {}",
        home.display(),
        options.home.display()
    );

    let log = log_path(&home, &options.session);
    let before_sha = log_sha256(&log)?;
    let before_bytes = log_bytes(&log)?;
    let fixture_present = before_sha.is_some();
    if fixture_present != (options.expect_messages > 0 || options.expect_events > 0) {
        bail!(
            "fixture {} is {} but {} messages in {} records were expected",
            log.display(),
            if fixture_present { "present" } else { "absent" },
            options.expect_messages,
            options.expect_events
        );
    }

    let project = home.join(PROJECT_DIR);
    prepare_overlay_project(&project, options.skill_count)?;
    // The first request boundary of each new stage is a warmup: whatever the
    // first call would populate (a freshly scanned overlay, a first parse of a
    // user config) is paid here, outside the measured stage, so the recorded
    // delta describes a repeat request rather than a cold start.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("build a Tokio runtime for the request overlay stage")?;
    let overlay_base = SkillRegistry::load_global()?;
    let overlay_skills = request_overlay(&runtime, &overlay_base, &project);
    let mcp_servers = McpConfig::load_for_dir(Some(&project)).servers.len();

    let mut stages = Stages::new();
    let mut loaded: Option<Session> = None;

    stages.stage("skill_catalog", || {
        let registry = SkillRegistry::load_global()?;
        let loaded = registry.list().len();
        ensure!(
            loaded == options.skill_count,
            "skill catalog loaded {loaded} skills, expected {}",
            options.skill_count
        );
        Ok(json!({"skills": loaded, "expected": options.skill_count}))
    });

    stages.stage("skill_request_overlay", || {
        for _ in 0..REPEAT_REQUESTS {
            request_overlay(&runtime, &overlay_base, &project);
        }
        Ok(json!({"skills": overlay_skills, "requests": REPEAT_REQUESTS}))
    });

    stages.stage("mcp_config_load", || {
        let mut servers = mcp_servers;
        for _ in 0..REPEAT_REQUESTS {
            servers = McpConfig::load_for_dir(Some(&project)).servers.len();
        }
        ensure!(
            servers == mcp_servers,
            "repeat MCP loads returned {servers} servers after the warmup reported {mcp_servers}"
        );
        Ok(json!({"servers": servers, "requests": REPEAT_REQUESTS}))
    });

    stages.stage("attach_history", || {
        if !fixture_present {
            return Ok(json!({"status": "absent", "history": 0}));
        }
        let session = Session::load(&options.session).context("session load")?;
        let history = session.history_messages()?.len();
        ensure!(
            history == options.expected_messages(),
            "history has {history} messages, expected {}",
            options.expected_messages()
        );
        let detail = json!({"status": "loaded", "history": history});
        loaded = Some(session);
        Ok(detail)
    });

    stages.stage("resume_ready", || {
        if !fixture_present {
            return Ok(json!({"status": "absent", "history": 0}));
        }
        let stub = Session::load_startup_stub(&options.session).context("startup stub load")?;
        ensure!(
            stub.active_context().is_empty(),
            "startup stub kept {} transcript messages",
            stub.active_context().len()
        );
        Ok(json!({"status": "stubbed", "active_context": 0}))
    });

    stages.stage("provider_dispatch", || {
        let Some(session) = loaded.as_mut() else {
            return Ok(json!({"status": "absent", "messages": 0}));
        };
        let messages: Vec<Message> = session.messages_for_provider();
        ensure!(
            messages.len() == options.expected_messages(),
            "request snapshot has {} messages, expected {}",
            messages.len(),
            options.expected_messages()
        );
        Ok(json!({
            "status": "snapshot",
            "messages": messages.len(),
            "projection_json_bytes": session.provider_projection_json_bytes(),
        }))
    });

    stages.stage("session_save", || save_scratch(&home, options.scratch_messages));

    let after_sha = log_sha256(&log)?;
    let fixture_unchanged = after_sha == before_sha;
    if !fixture_unchanged {
        stages.fail(
            FailureKind::Mismatch,
            "attach_history mutated the measured fixture log".to_string(),
        );
    }

    let counters = snapshot();
    if !counters.enabled {
        stages.fail(
            FailureKind::Preflight,
            format!(
                "counters are disabled by {}; zeros are not measurements",
                horon_agent_perf_counters::ENV_ENABLE
            ),
        );
    }

    let failure_kinds: Vec<&str> = stages.kinds.iter().map(|kind| kind.as_str()).collect();

    Ok(json!({
        "schema_version": 1,
        "probe": "loading_probe",
        "build": build_identity(),
        "home": home.display().to_string(),
        "session": options.session,
        "expect_events": options.expect_events,
        "expect_messages": options.expected_messages(),        "skill_count": options.skill_count,
        "fixture": {
            "log_path": log.display().to_string(),
            "present": fixture_present,
            "log_bytes": before_bytes,
            "log_sha256": before_sha,
            "log_sha256_after": after_sha,
            "unchanged": fixture_unchanged,
        },
        "order": ORDER,
        "stages": stages.entries,
        "counters": counters,
        "failures": stages.failures,
        "failure_kinds": failure_kinds,
        "exit_code": exit_code_for(&stages.kinds),
        "outcome": if stages.failures.is_empty() { "ok" } else { "failure" },
    }))
}

/// Write the project workspace the overlay stage resolves against.
///
/// The skill names differ from the catalog fixtures so the composed registry
/// carries both sets, and the directory is reset on every run so a stale file
/// from an earlier run cannot decide this run's measurements.
fn prepare_overlay_project(project: &Path, count: usize) -> Result<()> {
    let root = project.join(".horon-agent").join("skills");
    std::fs::create_dir_all(project).context("create the probe project directory")?;
    if root.exists() {
        std::fs::remove_dir_all(&root).context("reset the probe project skills")?;
    }
    std::fs::create_dir_all(&root).context("create the probe project skill root")?;
    for index in 0..count {
        let name = format!("overlay-skill-{index:04}");
        let directory = root.join(&name);
        std::fs::create_dir_all(&directory).context("create a probe project skill directory")?;
        std::fs::write(
            directory.join("SKILL.md"),
            format!(
                "---\nname: {name}\ndescription: project overlay skill {index} for the loading probe\n---\n\nBody of {name}.\n"
            ),
        )
        .context("write a probe project skill file")?;
    }
    Ok(())
}

/// One request boundary over the project overlay, as the daemon runs it.
fn request_overlay(runtime: &tokio::runtime::Runtime, base: &SkillRegistry, project: &Path) -> usize {
    runtime.block_on(SkillRegistry::with_request_overlay(Some(project), async move {
        SkillRegistry::effective_for_working_dir(base, Some(project))
            .list()
            .len()
    }))
}

fn save_scratch(home: &Path, messages: usize) -> Result<Value> {
    ensure!(messages > 0, "the save step needs at least one message");
    let directory = home.join("sessions").join(SCRATCH_SESSION);
    match std::fs::remove_dir_all(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }

    let mut session = Session::create_with_id(SCRATCH_SESSION.into(), None, None);
    for index in 0..messages {
        let role = if index % 2 == 0 { Role::User } else { Role::Assistant };
        session.add_message(role, text(&format!("loading probe scratch message {index}")));
    }
    session.save().context("scratch save")?;

    let log = session.event_log_path()?;
    let bytes = std::fs::read(&log)?;
    let sha = store::sha256_hex(&bytes);
    let reloaded = Session::load(SCRATCH_SESSION).context("scratch reload")?;
    ensure!(
        reloaded.active_context().len() == messages,
        "reloaded scratch session has {} messages, expected {messages}",
        reloaded.active_context().len()
    );

    Ok(json!({
        "messages": messages,
        "log_bytes": bytes.len(),
        "log_sha256": sha,
        "reloaded": reloaded.active_context().len(),
    }))
}
