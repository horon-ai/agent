//! Micro-benchmarks for round-3 config, transcript import, and session scan paths in `horon-agent-base`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use horon_agent_base::config::{Config, config, invalidate_config_cache};
use horon_agent_base::import::{TranscriptFormat, preview_and_import_transcript};
use horon_agent_base::message::{ContentBlock, Role};
use horon_agent_base::session::Session;
use horon_agent_session_store::{EventGraph, SessionPaths, StoreErrorCode, reader};

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

struct CountingAlloc;

impl CountingAlloc {
    fn record(bytes: usize) {
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
        }
    }
}

// SAFETY: forwards every call to `System` and updates only atomics/thread-local bool.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

const RUNS: usize = 7;

fn measure_with_setup<S, F>(name: &str, mut setup: S, mut work: F) -> Result<(), Box<dyn Error>>
where
    S: FnMut() -> Result<(), Box<dyn Error>>,
    F: FnMut() -> Result<(), Box<dyn Error>>,
{
    setup()?;
    work()?;
    let mut times_us = Vec::with_capacity(RUNS);
    let mut last_allocs = 0u64;
    let mut last_bytes = 0u64;
    for _ in 0..RUNS {
        setup()?;
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
        COUNTING.with(|c| c.set(true));
        let start = Instant::now();
        let res = work();
        let elapsed = start.elapsed();
        COUNTING.with(|c| c.set(false));
        res?;
        last_allocs = ALLOCS.load(Ordering::Relaxed) - a0;
        last_bytes = ALLOC_BYTES.load(Ordering::Relaxed) - b0;
        times_us.push(elapsed.as_micros());
    }
    times_us.sort_unstable();
    println!(
        "BENCH {name}: runs={RUNS} min_us={} med_us={} max_us={} allocs={last_allocs} alloc_bytes={last_bytes}",
        times_us[0],
        times_us[RUNS / 2],
        times_us[RUNS - 1]
    );
    Ok(())
}

fn build_config_fixture(home: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut base = String::from(
        "[provider]\ndefault_provider = \"alpha\"\ndefault_model = \"alpha-1\"\n\
         max_retries = 3\nstream_idle_timeout_secs = 120\n\n\
         [tools]\nirc = false\nplan_mode = false\n\n\
         [compaction]\nbackground = false\nmax_context_tokens = 128000\nrecent_tokens = 4096\n\n",
    );
    for p in ["alpha", "beta", "gamma", "delta"] {
        writeln!(
            base,
            "[providers.{p}]\ntype = \"openai-compatible\"\nbase_url = \"https://api.{p}.example/v1\"\n\
             auth = \"none\"\ndefault_model = \"{p}-1\"\n\
             models = [{{ id = \"{p}-1\", context_window = 128000 }}, {{ id = \"{p}-2\", context_window = 200000 }}]\n"
        )?;
    }
    fs::write(home.join("config.toml"), base)?;

    let ov1 = home.join("overlay-1.toml");
    let ov2 = home.join("overlay-2.toml");
    let ov3 = home.join("overlay-3.toml");
    fs::write(
        &ov1,
        "[tools]\nprofile = \"full\"\nirc = true\n[provider]\nmax_retries = 4\n",
    )?;
    fs::write(
        &ov2,
        "[tools]\nplan_mode = true\n[providers.alpha]\ndefault_model = \"alpha-2\"\n",
    )?;
    fs::write(
        &ov3,
        "[prewalk]\nenabled = true\ninto = \"alpha-2\"\n[features]\nrepeat_guard = true\n",
    )?;
    Ok(vec![ov1, ov2, ov3])
}

fn build_claude_transcript(path: &Path, turns: usize) -> Result<(), Box<dyn Error>> {
    let mut body = String::with_capacity(turns * 512);
    let mut prev_uuid: Option<String> = None;
    for i in 0..turns {
        let u_id = format!("u-{i}");
        let a_id = format!("a-{i}");
        let parent = prev_uuid
            .as_deref()
            .map_or_else(|| "null".to_string(), |p| format!("\"{p}\""));
        writeln!(
            body,
            "{{\"type\":\"user\",\"uuid\":\"{u_id}\",\"parentUuid\":{parent},\"sessionId\":\"bench-cc-1\",\
             \"cwd\":\"/work/app\",\"timestamp\":\"2026-11-01T10:00:00Z\",\
             \"message\":{{\"role\":\"user\",\"content\":\"Analyze module {i} and check config parsing paths\"}}}}"
        )?;
        writeln!(
            body,
            "{{\"type\":\"assistant\",\"uuid\":\"{a_id}\",\"parentUuid\":\"{u_id}\",\"sessionId\":\"bench-cc-1\",\
             \"cwd\":\"/work/app\",\"timestamp\":\"2026-11-01T10:00:05Z\",\
             \"message\":{{\"role\":\"assistant\",\"model\":\"claude-x\",\
             \"content\":[{{\"type\":\"text\",\"text\":\"Checked module {i}: config parsing and overlay merging succeeded cleanly across all entries.\"}}]}}}}"
        )?;
        prev_uuid = Some(a_id);
    }
    fs::write(path, body)?;
    Ok(())
}

fn build_codex_transcript(path: &Path, turns: usize) -> Result<(), Box<dyn Error>> {
    let mut body = String::with_capacity(turns * 1024);
    writeln!(
        body,
        "{{\"timestamp\":\"2026-11-01T10:00:00Z\",\"type\":\"session_meta\",\
         \"payload\":{{\"id\":\"bench-cx-1\",\"timestamp\":\"2026-11-01T10:00:00Z\",\"cwd\":\"/work/app\"}}}}"
    )?;
    for i in 0..turns {
        writeln!(
            body,
            "{{\"timestamp\":\"2026-11-01T10:00:01Z\",\"type\":\"response_item\",\
             \"payload\":{{\"type\":\"reasoning\",\"summary\":[{{\"text\":\"inspecting {i}\"}}]}}}}"
        )?;
        writeln!(
            body,
            "{{\"timestamp\":\"2026-11-01T10:00:02Z\",\"type\":\"response_item\",\
             \"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"Run check {i}\"}}]}}}}"
        )?;
        writeln!(
            body,
            "{{\"timestamp\":\"2026-11-01T10:00:03Z\",\"type\":\"response_item\",\
             \"payload\":{{\"type\":\"function_call\",\"name\":\"bash\",\"arguments\":\"ls -la src/{i}\"}}}}"
        )?;
        writeln!(
            body,
            "{{\"timestamp\":\"2026-11-01T10:00:04Z\",\"type\":\"response_item\",\
             \"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"Completed check {i} without warnings or errors.\"}}]}}}}"
        )?;
    }
    fs::write(path, body)?;
    Ok(())
}

fn build_session_store_fixture(_home: &Path, count: usize) -> Result<String, Box<dyn Error>> {
    let root_id = "session_bench_root".to_string();
    let mut root = Session::create_with_id(root_id.clone(), None, Some("root".to_string()));
    root.add_message(
        Role::User,
        vec![ContentBlock::Text {
            text: "root task".to_string(),
            cache_control: None,
        }],
    );
    root.save()?;

    for i in 0..count {
        let id = format!("session_bench_{i:04}");
        let parent = if i < 3 { Some(root_id.clone()) } else { None };
        let mut session = Session::create_with_id(id, parent, Some(format!("session {i}")));
        for m in 0..8 {
            session.add_message(
                Role::User,
                vec![ContentBlock::Text {
                    text: format!(
                        "message {m} payload with enough text to mimic real logs {}",
                        "x".repeat(120)
                    ),
                    cache_control: None,
                }],
            );
            session.add_message(
                Role::Assistant,
                vec![ContentBlock::ToolUse {
                    id: format!("call_{i}_{m}"),
                    name: "read".to_string(),
                    input: serde_json::json!({ "file_path": format!("src/mod_{m}.rs"), "limit": 200 }),
                    thought_signature: None,
                }],
            );
            session.add_message(
                Role::User,
                vec![ContentBlock::ToolResult {
                    tool_use_id: format!("call_{i}_{m}"),
                    content: "fn sample() { println!(\"ok\"); }\n".repeat(4),
                    is_error: None,
                    verdict: None,
                }],
            );
        }
        session.save()?;
    }
    Ok(root_id)
}

fn clear_imported_sessions(home: &Path) -> Result<(), Box<dyn Error>> {
    let sessions_dir = home.join("sessions");
    if sessions_dir.exists() {
        fs::remove_dir_all(&sessions_dir)?;
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let home = tempfile::tempdir()?;
    let home_path = home.path().canonicalize()?;
    horon_agent_base::env::set_var("HORON_HOME", &home_path);
    horon_agent_base::env::set_var("HOME", &home_path);
    invalidate_config_cache();

    let overlays = build_config_fixture(&home_path)?;
    // Prime config cache once like startup.rs does before set_run_overlays.
    black_box(config());

    measure_with_setup(
        "config_set_run_overlays_and_read",
        || {
            Config::set_run_overlays(&[])?;
            invalidate_config_cache();
            Ok(())
        },
        || {
            Config::set_run_overlays(&overlays)?;
            let cfg = config();
            black_box((&cfg.tools.profile, &cfg.provider.default_model, cfg.prewalk.enabled));
            Ok(())
        },
    )?;

    Config::set_run_overlays(&overlays)?;
    measure_with_setup(
        "config_overlay_load_strict",
        || Ok(()),
        || {
            let cfg = Config::load_strict()?;
            black_box((&cfg.tools.profile, &cfg.provider.default_model));
            Ok(())
        },
    )?;
    Config::set_run_overlays(&[])?;

    let transcript_dir = tempfile::tempdir()?;
    let claude_path = transcript_dir.path().join("claude.jsonl");
    let codex_path = transcript_dir.path().join("codex.jsonl");
    build_claude_transcript(&claude_path, 400)?;
    build_codex_transcript(&codex_path, 400)?;

    measure_with_setup(
        "transcript_claude_preview_and_import_400t",
        || clear_imported_sessions(&home_path),
        || {
            let (preview, session) = preview_and_import_transcript(&claude_path, Some(TranscriptFormat::Claude))?;
            black_box((preview.user_messages, session.id));
            Ok(())
        },
    )?;

    measure_with_setup(
        "transcript_codex_preview_and_import_400t",
        || clear_imported_sessions(&home_path),
        || {
            let (preview, session) = preview_and_import_transcript(&codex_path, Some(TranscriptFormat::Codex))?;
            black_box((preview.user_messages, session.id));
            Ok(())
        },
    )?;

    let store_home = tempfile::tempdir()?;
    let store_home_path = store_home.path().canonicalize()?;
    horon_agent_base::env::set_var("HORON_HOME", &store_home_path);
    let root_id = build_session_store_fixture(&store_home_path, 120)?;
    let paths = SessionPaths::new(store_home_path.join("sessions"));

    measure_with_setup(
        "session_child_discovery_120_sessions",
        || Ok(()),
        || {
            let mut matched = 0usize;
            let mut scanner = reader::ParentScanner::new();
            for source in reader::sources(&paths)? {
                if source.layout != reader::SourceLayout::Canonical || source.session_id.as_str() == root_id {
                    continue;
                }
                let is_child = match scanner.scan_canonical_parent(&paths, &source.session_id)? {
                    reader::CanonicalParent::Known(parent) => parent.as_deref() == Some(root_id.as_str()),
                    reader::CanonicalParent::NeedsReplay => {
                        let graph = EventGraph::open(&paths, &source.session_id)?;
                        let view = match graph.replay(None) {
                            Ok(view) => view,
                            Err(error) if error.code == StoreErrorCode::ConflictHeads => {
                                graph.replay(Some(&graph.shared_head()?))?
                            }
                            Err(error) => return Err(error.into()),
                        };
                        view.metadata().parent_id.as_deref() == Some(root_id.as_str())
                    }
                };
                if is_child {
                    let graph = EventGraph::open(&paths, &source.session_id)?;
                    let view = match graph.replay(None) {
                        Ok(view) => view,
                        Err(error) if error.code == StoreErrorCode::ConflictHeads => {
                            graph.replay(Some(&graph.shared_head()?))?
                        }
                        Err(error) => return Err(error.into()),
                    };
                    if view.metadata().parent_id.as_deref() == Some(root_id.as_str()) {
                        matched += 1;
                    }
                }
            }
            black_box(matched);
            Ok(())
        },
    )?;

    Ok(())
}
