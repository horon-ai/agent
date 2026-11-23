//! Micro-benchmarks for round-3 core-tools optimization paths in `horon-agent-app-core`.
//!
//! Exercises the real public entrypoints for:
//! 1. `Registry::execute("agentgrep", {"mode": "locate", ...})` over 200 files
//! 2. `Agent::run_once_streaming_mpsc` with `[features] repeat_guard = true`
//! 3. `agent::tool_result_trim::{preview, apply}` & end-to-end `run_once_streaming_mpsc` with trim active
//! 4. `Agent::enter_package_generation` on a package-free home with workspace `.mcp.json`
//! 5. `Agent::system_prompt_for_debug` + `PackageTextCache::load`
//! 6. `Agent::run_once_streaming_mpsc` with `spend_cap` inactive
//! 7. `Agent::run_once_streaming_mpsc` across tool rounds with `[prewalk] enabled = true`
//! 8. `PackageStore::prepare_record_executable`

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::VecDeque;
use std::error::Error;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use horon_agent_app_core::agent::Agent;
use horon_agent_app_core::agent::tool_result_trim::{self, TrimPolicy};
use horon_agent_app_core::config;
use horon_agent_app_core::message::{ContentBlock, Message, Role, StreamEvent, ToolDefinition};
use horon_agent_app_core::packages::{InstallRequest, PackageRecord, PackageScope, PackageStore, PackageTextCache};
use horon_agent_app_core::provider::{EventStream, Provider};
use horon_agent_app_core::tool::{Registry, ToolContext, ToolExecutionMode, ToolOutput};
use serde_json::json;
use tokio::sync::mpsc;

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);
static COUNTING: AtomicBool = AtomicBool::new(false);

struct CountingAlloc;

impl CountingAlloc {
    fn record(bytes: usize) {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
        }
    }
}

// SAFETY: forwards every call to `System` and updates only lock-free atomics.
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

struct ScriptedProvider {
    scripts: Mutex<VecDeque<Vec<StreamEvent>>>,
    model_name: String,
}

impl ScriptedProvider {
    fn new(model_name: &str, scripts: Vec<Vec<StreamEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into_iter().collect()),
            model_name: model_name.to_string(),
        })
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        let events = {
            let mut guard = match self.scripts.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            guard.pop_front().unwrap_or_else(|| {
                vec![
                    StreamEvent::TextDelta("done".to_string()),
                    StreamEvent::MessageEnd {
                        stop_reason: Some("end_turn".to_string()),
                    },
                ]
            })
        };
        let iter = events.into_iter().map(Ok);
        Ok(Box::pin(futures::stream::iter(iter)))
    }

    fn name(&self) -> &str {
        "stub"
    }

    fn model(&self) -> String {
        self.model_name.clone()
    }

    fn available_models_for_switching(&self) -> Vec<String> {
        vec![self.model_name.clone(), "plan-m".to_string(), "exec-m".to_string()]
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            scripts: Mutex::new(VecDeque::new()),
            model_name: self.model_name.clone(),
        })
    }
}

fn measure_sync<F, T, E>(label: &str, iters: usize, mut run_iter: F) -> Result<(), Box<dyn Error>>
where
    F: FnMut(usize) -> Result<T, E>,
    E: Into<Box<dyn Error>>,
{
    let w = run_iter(0).map_err(Into::into)?;
    black_box(w);

    let mut times_us = Vec::with_capacity(RUNS);
    let mut last_allocs = 0u64;
    let mut last_bytes = 0u64;
    for run in 0..RUNS {
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
        COUNTING.store(true, Ordering::SeqCst);
        let start = Instant::now();
        for iter in 0..iters {
            let out = run_iter(run * iters + iter).map_err(Into::into)?;
            black_box(out);
        }
        let elapsed = start.elapsed();
        COUNTING.store(false, Ordering::SeqCst);
        times_us.push(elapsed.as_micros());
        last_allocs = ALLOCS.load(Ordering::Relaxed) - a0;
        last_bytes = ALLOC_BYTES.load(Ordering::Relaxed) - b0;
    }
    times_us.sort_unstable();
    println!(
        "BENCH {label}: runs={RUNS} min_us={} med_us={} max_us={} allocs={last_allocs} alloc_bytes={last_bytes}",
        times_us[0],
        times_us[RUNS / 2],
        times_us[RUNS - 1]
    );
    Ok(())
}

fn reset_home(home: &Path, config_toml: &str) -> Result<(), Box<dyn Error>> {
    horon_agent_app_core::env::set_var("HORON_HOME", home);
    horon_agent_app_core::env::set_var("HOME", home);
    fs::write(home.join("config.toml"), config_toml)?;
    config::invalidate_config_cache();
    Ok(())
}

fn host_target() -> Result<&'static str, Box<dyn Error>> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("windows", "aarch64") => Ok("aarch64-pc-windows-msvc"),
        ("windows", "x86_64") => Ok("x86_64-pc-windows-msvc"),
        (os, arch) => Err(format!("unsupported host {os}/{arch}").into()),
    }
}

fn stage_plugin_with_skill(dir: &Path, id: &str) -> Result<(), Box<dyn Error>> {
    let target = host_target()?;
    let bin_rel = if std::env::consts::OS == "windows" {
        "bin/example.exe"
    } else {
        "bin/example"
    };
    let bin_dst = dir.join("extension").join(bin_rel);
    if let Some(parent) = bin_dst.parent() {
        fs::create_dir_all(parent)?;
    }
    let exe_src = std::env::current_exe()?;
    fs::copy(&exe_src, &bin_dst)?;
    horon_agent_platform::set_permissions_executable(&bin_dst)?;
    fs::write(
        dir.join("extension/extension.toml"),
        format!(
            "schema_version = 1\nid = \"{id}\"\nversion = \"1.0.0\"\nname = \"Bench {id}\"\nprotocol = \"1.0\"\n[targets]\n{target} = \"{bin_rel}\"\n"
        ),
    )?;
    fs::create_dir_all(dir.join("resources/skills/bench-skill"))?;
    fs::create_dir_all(dir.join("resources/rules"))?;
    fs::write(
        dir.join("resources/skills/bench-skill/SKILL.md"),
        "---\nname: bench-skill\ndescription: Bench skill description\n---\nUse bench skill.\n",
    )?;
    fs::write(dir.join("resources/rules/bench.md"), "Keep responses concise.\n")?;
    fs::write(
        dir.join("plugin.toml"),
        format!(
            "schema_version = 1\nid = \"{id}\"\nversion = \"1.0.0\"\nname = \"Bench {id}\"\nextension = \"extension/extension.toml\"\n[resources]\nskills = [\"resources/skills/bench-skill/SKILL.md\"]\nrules = [\"resources/rules/bench.md\"]\n"
        ),
    )?;
    Ok(())
}

fn stage_extension_only(dir: &Path, id: &str) -> Result<(), Box<dyn Error>> {
    let target = host_target()?;
    let bin_rel = if std::env::consts::OS == "windows" {
        "bin/example.exe"
    } else {
        "bin/example"
    };
    let bin_dst = dir.join(bin_rel);
    if let Some(parent) = bin_dst.parent() {
        fs::create_dir_all(parent)?;
    }
    let exe_src = std::env::current_exe()?;
    fs::copy(&exe_src, &bin_dst)?;
    horon_agent_platform::set_permissions_executable(&bin_dst)?;
    fs::write(
        dir.join("extension.toml"),
        format!(
            "schema_version = 1\nid = \"{id}\"\nversion = \"1.0.0\"\nname = \"Bench {id}\"\nprotocol = \"1.0\"\n[targets]\n{target} = \"{bin_rel}\"\n"
        ),
    )?;
    Ok(())
}

fn drain_events(rx: &mut mpsc::UnboundedReceiver<horon_agent_app_core::protocol::ServerEvent>) {
    while let Ok(_ev) = rx.try_recv() {}
}

async fn bench_agentgrep_locate(work: &Path) -> Result<(), Box<dyn Error>> {
    let repo = work.join("locate_repo");
    fs::create_dir_all(repo.join("src"))?;
    for i in 0..200 {
        let mut body = String::with_capacity(8_000);
        for line in 0..200 {
            if i == 137 && line == 95 {
                body.push_str("    /// Drop every expired login token when session lifetime ends.\n");
            } else if i == 137 && line == 96 {
                body.push_str("    fn purge_old_session_tokens(&mut self, now_secs: u64) {\n");
            } else {
                body.push_str(&format!(
                    "    let accumulator_value_{line} = compute_module_delta_{i}(input_state, {line});\n"
                ));
            }
        }
        fs::write(repo.join(format!("src/module_{i:03}.rs")), body)?;
    }

    let provider: Arc<dyn Provider> = ScriptedProvider::new("stub", Vec::new());
    let registry = Registry::new(provider).await;

    let run_once = |tag: &str| {
        let reg = registry.clone();
        let dir = repo.clone();
        let call_id = tag.to_string();
        async move {
            let ctx = ToolContext {
                session_id: "bench-locate".to_string(),
                message_id: "bench-msg".to_string(),
                tool_call_id: call_id,
                working_dir: Some(dir),
                stdin_request_tx: None,
                graceful_shutdown_signal: None,
                execution_mode: ToolExecutionMode::Direct,
            };
            reg.execute(
                "agentgrep",
                json!({ "mode": "locate", "query": "remove old session token" }),
                ctx,
            )
            .await
        }
    };

    let warm = run_once("warmup").await?;
    black_box(warm.output.len());

    let mut times_us = Vec::with_capacity(RUNS);
    let mut last_allocs = 0u64;
    let mut last_bytes = 0u64;
    for run in 0..RUNS {
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
        COUNTING.store(true, Ordering::SeqCst);
        let start = Instant::now();
        let out = run_once(&format!("run-{run}")).await?;
        let elapsed = start.elapsed();
        COUNTING.store(false, Ordering::SeqCst);
        black_box(out.output.len());
        times_us.push(elapsed.as_micros());
        last_allocs = ALLOCS.load(Ordering::Relaxed) - a0;
        last_bytes = ALLOC_BYTES.load(Ordering::Relaxed) - b0;
    }
    times_us.sort_unstable();
    println!(
        "BENCH agentgrep_locate_200files_40klines: runs={RUNS} min_us={} med_us={} max_us={} allocs={last_allocs} alloc_bytes={last_bytes}",
        times_us[0],
        times_us[RUNS / 2],
        times_us[RUNS - 1]
    );
    Ok(())
}

async fn bench_prose_repeat(home: &Path, work: &Path) -> Result<(), Box<dyn Error>> {
    reset_home(home, "[tools]\nprofile = \"core\"\n\n[features]\nrepeat_guard = true\n")?;

    let mut full_text = String::with_capacity(20_000);
    for i in 0..160 {
        if i % 10 == 4 {
            full_text.push_str("| column_a | column_b | column_c | description |\n");
        } else if i % 10 == 7 {
            full_text.push_str("- Bullet item summarizing intermediate validation state.\n");
        } else {
            full_text.push_str(&format!(
                "Paragraph index {i:03} confirms that validation step number {i} completed cleanly without errors. "
            ));
        }
    }
    let chunk_bytes: Vec<String> = full_text
        .as_bytes()
        .chunks(8)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect();

    let build_events = || {
        let mut events: Vec<StreamEvent> = chunk_bytes.iter().cloned().map(StreamEvent::TextDelta).collect();
        events.push(StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".to_string()),
        });
        events
    };

    let make_agent = |events: Vec<StreamEvent>| async move {
        let provider: Arc<dyn Provider> = ScriptedProvider::new("stub", vec![events]);
        let registry = Registry::empty();
        let mut agent = Agent::new(provider, registry);
        agent.set_memory_enabled(false);
        agent.set_working_dir(&work.display().to_string());
        agent
    };

    // Warmup
    {
        let mut agent = make_agent(build_events()).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.run_once_streaming_mpsc("explain", Vec::new(), None, tx).await?;
        drain_events(&mut rx);
    }

    let mut times_us = Vec::with_capacity(RUNS);
    let mut last_allocs = 0u64;
    let mut last_bytes = 0u64;
    for _run in 0..RUNS {
        let mut agent = make_agent(build_events()).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
        COUNTING.store(true, Ordering::SeqCst);
        let start = Instant::now();
        agent.run_once_streaming_mpsc("explain", Vec::new(), None, tx).await?;
        let elapsed = start.elapsed();
        COUNTING.store(false, Ordering::SeqCst);
        drain_events(&mut rx);
        times_us.push(elapsed.as_micros());
        last_allocs = ALLOCS.load(Ordering::Relaxed) - a0;
        last_bytes = ALLOC_BYTES.load(Ordering::Relaxed) - b0;
    }
    times_us.sort_unstable();
    println!(
        "BENCH prose_repeat_streaming_1600deltas: runs={RUNS} min_us={} med_us={} max_us={} allocs={last_allocs} alloc_bytes={last_bytes}",
        times_us[0],
        times_us[RUNS / 2],
        times_us[RUNS - 1]
    );

    // Long single sentence streamed in 2-byte deltas (exposes O(S^2) tail rescanning)
    let long_sentence = format!(
        "Detailed verification narrative {} ends cleanly here. ",
        "including invariant checks across modules ".repeat(60)
    );
    let small_chunks: Vec<String> = long_sentence
        .as_bytes()
        .chunks(2)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
        .collect();
    let build_long_events = || {
        let mut events: Vec<StreamEvent> = small_chunks.iter().cloned().map(StreamEvent::TextDelta).collect();
        events.push(StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".to_string()),
        });
        events
    };
    {
        let mut agent = make_agent(build_long_events()).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.run_once_streaming_mpsc("explain", Vec::new(), None, tx).await?;
        drain_events(&mut rx);
    }
    let mut times_long_us = Vec::with_capacity(RUNS);
    let mut long_allocs = 0u64;
    let mut long_bytes = 0u64;
    for _run in 0..RUNS {
        let mut agent = make_agent(build_long_events()).await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
        COUNTING.store(true, Ordering::SeqCst);
        let start = Instant::now();
        agent.run_once_streaming_mpsc("explain", Vec::new(), None, tx).await?;
        let elapsed = start.elapsed();
        COUNTING.store(false, Ordering::SeqCst);
        drain_events(&mut rx);
        times_long_us.push(elapsed.as_micros());
        long_allocs = ALLOCS.load(Ordering::Relaxed) - a0;
        long_bytes = ALLOC_BYTES.load(Ordering::Relaxed) - b0;
    }
    times_long_us.sort_unstable();
    println!(
        "BENCH prose_repeat_long_sentence_1200deltas: runs={RUNS} min_us={} med_us={} max_us={} allocs={long_allocs} alloc_bytes={long_bytes}",
        times_long_us[0],
        times_long_us[RUNS / 2],
        times_long_us[RUNS - 1]
    );
    Ok(())
}

fn build_trim_messages(oversized_count: usize) -> Vec<Message> {
    let mut messages = Vec::with_capacity(60);
    for i in 0..27 {
        messages.push(Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: format!("call-{i}"),
                name: "read".to_string(),
                input: json!({"file_path": "a.rs"}),
                thought_signature: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        });
        let (body, is_error) = if i < oversized_count {
            ("x".repeat(8_000), None)
        } else if i == 25 {
            ("e".repeat(8_000), Some(true))
        } else {
            ("ok: line 1..10".repeat(8), None)
        };
        messages.push(Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: format!("call-{i}"),
                content: body,
                is_error,
                verdict: None,
            }],
            timestamp: None,
            tool_duration_ms: None,
        });
    }
    for i in 0..6 {
        messages.push(Message::user(&format!("recent-{i}")));
    }
    messages
}

async fn bench_tool_result_trim(home: &Path, work: &Path) -> Result<(), Box<dyn Error>> {
    reset_home(home, "[tools]\nprofile = \"core\"\n")?;
    let policy = TrimPolicy::new(1_000)?;
    let mut messages = build_trim_messages(6);
    if let Some(ContentBlock::ToolResult { content, .. }) = messages.get_mut(1).and_then(|m| m.content.get_mut(0)) {
        *content = "가나다라마바사아자차카타파하 ".repeat(500);
    }
    let hit_msgs = messages;
    let miss_msgs = build_trim_messages(0);

    measure_sync(
        "tool_result_trim_preview_x200",
        200,
        |_| -> Result<usize, Box<dyn Error>> { Ok(tool_result_trim::preview(&hit_msgs, policy).chars_removed) },
    )?;

    measure_sync(
        "tool_result_trim_apply_hits_x200",
        200,
        |_| -> Result<usize, Box<dyn Error>> {
            let out = tool_result_trim::apply(&hit_msgs, policy);
            Ok(out.map_or(0, |v| v.len()))
        },
    )?;

    measure_sync(
        "tool_result_trim_apply_nohit_x200",
        200,
        |_| -> Result<usize, Box<dyn Error>> {
            let out = tool_result_trim::apply(&miss_msgs, policy);
            Ok(out.map_or(0, |v| v.len()))
        },
    )?;

    // End-to-end turn with trim policy enabled
    let make_trimmed_agent = || -> Result<Agent, Box<dyn Error>> {
        let provider: Arc<dyn Provider> = ScriptedProvider::new(
            "stub",
            vec![vec![
                StreamEvent::TextDelta("ok".to_string()),
                StreamEvent::MessageEnd {
                    stop_reason: Some("end_turn".to_string()),
                },
            ]],
        );
        let mut agent = Agent::new(provider, Registry::empty());
        agent.set_memory_enabled(false);
        agent.set_working_dir(&work.display().to_string());
        for i in 0..10 {
            let id = format!("trim-turn-{i}");
            agent.add_manual_tool_use(id.clone(), "read".to_string(), json!({"file_path": "x"}))?;
            let text = if i < 4 {
                "z".repeat(6_000)
            } else {
                "short ok".to_string()
            };
            agent.add_manual_tool_result(id, ToolOutput::new(text), 1)?;
        }
        tool_result_trim::set_policy(agent.session_id(), policy);
        Ok(agent)
    };

    // Warmup
    {
        let mut agent = make_trimmed_agent()?;
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.run_once_streaming_mpsc("continue", Vec::new(), None, tx).await?;
        drain_events(&mut rx);
        tool_result_trim::clear_policy(agent.session_id());
    }

    let mut times_us = Vec::with_capacity(RUNS);
    let mut last_allocs = 0u64;
    let mut last_bytes = 0u64;
    for _run in 0..RUNS {
        let mut agent = make_trimmed_agent()?;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
        COUNTING.store(true, Ordering::SeqCst);
        let start = Instant::now();
        agent.run_once_streaming_mpsc("continue", Vec::new(), None, tx).await?;
        let elapsed = start.elapsed();
        COUNTING.store(false, Ordering::SeqCst);
        drain_events(&mut rx);
        tool_result_trim::clear_policy(agent.session_id());
        times_us.push(elapsed.as_micros());
        last_allocs = ALLOCS.load(Ordering::Relaxed) - a0;
        last_bytes = ALLOC_BYTES.load(Ordering::Relaxed) - b0;
    }
    times_us.sort_unstable();
    println!(
        "BENCH tool_result_trim_turn_x1: runs={RUNS} min_us={} med_us={} max_us={} allocs={last_allocs} alloc_bytes={last_bytes}",
        times_us[0],
        times_us[RUNS / 2],
        times_us[RUNS - 1]
    );
    Ok(())
}

async fn bench_enter_package_generation_empty(home: &Path, work: &Path) -> Result<(), Box<dyn Error>> {
    let empty_home = home.join("pkg_empty_home");
    fs::create_dir_all(&empty_home)?;
    reset_home(&empty_home, "[tools]\nprofile = \"core\"\n")?;

    let ws = work.join("ws_with_mcp");
    fs::create_dir_all(&ws)?;
    fs::write(
        ws.join(".mcp.json"),
        r#"{"mcpServers":{"s1":{"command":"echo","args":["1"]},"s2":{"command":"echo","args":["2"]}}}"#,
    )?;

    let provider: Arc<dyn Provider> = ScriptedProvider::new("stub", Vec::new());
    let registry = Registry::empty();

    // Warmup
    {
        let mut agent = Agent::new(Arc::clone(&provider), registry.clone());
        agent.set_working_dir(&ws.display().to_string());
        agent.enter_package_generation().await;
    }

    let mut times_us = Vec::with_capacity(RUNS);
    let mut last_allocs = 0u64;
    let mut last_bytes = 0u64;
    for _run in 0..RUNS {
        let mut run_us = 0u128;
        let mut run_allocs = 0u64;
        let mut run_bytes = 0u64;
        for _iter in 0..10 {
            let mut agent = Agent::new(Arc::clone(&provider), registry.clone());
            agent.set_working_dir(&ws.display().to_string());
            let a0 = ALLOCS.load(Ordering::Relaxed);
            let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
            COUNTING.store(true, Ordering::SeqCst);
            let start = Instant::now();
            agent.enter_package_generation().await;
            let elapsed = start.elapsed();
            COUNTING.store(false, Ordering::SeqCst);
            run_us += elapsed.as_micros();
            run_allocs += ALLOCS.load(Ordering::Relaxed) - a0;
            run_bytes += ALLOC_BYTES.load(Ordering::Relaxed) - b0;
        }
        times_us.push(run_us);
        last_allocs = run_allocs;
        last_bytes = run_bytes;
    }
    times_us.sort_unstable();
    println!(
        "BENCH enter_package_generation_empty_x10: runs={RUNS} min_us={} med_us={} max_us={} allocs={last_allocs} alloc_bytes={last_bytes}",
        times_us[0],
        times_us[RUNS / 2],
        times_us[RUNS - 1]
    );
    Ok(())
}

async fn bench_prompt_and_package_text(home: &Path, work: &Path) -> Result<(), Box<dyn Error>> {
    let pkg_home = home.join("pkg_prompt_home");
    fs::create_dir_all(&pkg_home)?;
    reset_home(&pkg_home, "[tools]\nprofile = \"core\"\n")?;

    let plugin_src = work.join("plugin_src");
    stage_plugin_with_skill(&plugin_src, "bench-plug")?;
    let store = PackageStore::for_horon_home(&pkg_home);
    let record = store.install(InstallRequest::new(&plugin_src, PackageScope::Global))?;

    let ext_src = work.join("ext_src");
    stage_extension_only(&ext_src, "bench-ext")?;
    let ext_record = store.install(InstallRequest::new(&ext_src, PackageScope::Global))?;

    let provider: Arc<dyn Provider> = ScriptedProvider::new("stub", Vec::new());
    let mut agent = Agent::new(provider, Registry::empty());
    agent.set_working_dir(&work.display().to_string());
    agent.enter_package_generation().await;
    drop(agent.tool_definitions_for_debug().await);

    measure_sync(
        "system_prompt_for_debug_plugin_x50",
        50,
        |_| -> Result<usize, Box<dyn Error>> { Ok(agent.system_prompt_for_debug().len()) },
    )?;

    // Also test PackageTextCache::load directly:
    // 1) Extension-only records (no plugins -> canonicalize should be skipped)
    let cache_ext = PackageTextCache::new();
    let ext_records: Vec<PackageRecord> = vec![ext_record.clone()];
    measure_sync(
        "package_text_cache_load_ext_only_x200",
        200,
        |_| -> Result<bool, Box<dyn Error>> {
            let loaded = cache_ext.load(&store, Some(work), ext_records.iter());
            Ok(loaded.is_empty())
        },
    )?;

    // 2) Global plugin cache hit (double BTreeMap lookup + canonicalize)
    let cache_plug = PackageTextCache::new();
    let plug_records: Vec<PackageRecord> = vec![record];
    measure_sync(
        "package_text_cache_load_plugin_hit_x200",
        200,
        |_| -> Result<bool, Box<dyn Error>> {
            let loaded = cache_plug.load(&store, Some(work), plug_records.iter());
            Ok(loaded.is_empty())
        },
    )?;

    // Prepare record executable benchmark using the installed extension record
    let warm_lease = store.lease("bench-prep-warmup")?;
    let warm_exec = store.prepare_record_executable(warm_lease, &ext_record)?;
    black_box(warm_exec.executable().as_os_str().len());
    let mut times_us = Vec::with_capacity(RUNS);
    let mut last_allocs = 0u64;
    let mut last_bytes = 0u64;
    for run in 0..RUNS {
        let mut leases = Vec::with_capacity(100);
        for iter in 0..100 {
            leases.push(store.lease(&format!("bench-prep-{run}-{iter}"))?);
        }
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
        COUNTING.store(true, Ordering::SeqCst);
        let start = Instant::now();
        for lease in leases {
            let exec = store.prepare_record_executable(lease, &ext_record)?;
            black_box(exec.executable().as_os_str().len());
        }
        let elapsed = start.elapsed();
        COUNTING.store(false, Ordering::SeqCst);
        times_us.push(elapsed.as_micros());
        last_allocs = ALLOCS.load(Ordering::Relaxed) - a0;
        last_bytes = ALLOC_BYTES.load(Ordering::Relaxed) - b0;
    }
    times_us.sort_unstable();
    println!(
        "BENCH prepare_record_executable_x100: runs={RUNS} min_us={} med_us={} max_us={} allocs={last_allocs} alloc_bytes={last_bytes}",
        times_us[0],
        times_us[RUNS / 2],
        times_us[RUNS - 1]
    );

    Ok(())
}

async fn bench_spend_cap_inactive(home: &Path, work: &Path) -> Result<(), Box<dyn Error>> {
    reset_home(home, "[tools]\nprofile = \"core\"\n")?;

    let turn_events = || {
        vec![
            StreamEvent::TextDelta("ok".to_string()),
            StreamEvent::TokenUsage {
                input_tokens: Some(1_200),
                output_tokens: Some(350),
                cache_read_input_tokens: Some(400),
                cache_creation_input_tokens: Some(100),
            },
            StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".to_string()),
            },
        ]
    };

    let make_agent = || {
        let provider: Arc<dyn Provider> = ScriptedProvider::new("gpt-4o", vec![turn_events()]);
        let mut agent = Agent::new(provider, Registry::empty());
        agent.set_memory_enabled(false);
        agent.set_working_dir(&work.display().to_string());
        agent
    };

    // Warmup
    {
        let mut agent = make_agent();
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.run_once_streaming_mpsc("hi", Vec::new(), None, tx).await?;
        drain_events(&mut rx);
    }

    let mut times_us = Vec::with_capacity(RUNS);
    let mut last_allocs = 0u64;
    let mut last_bytes = 0u64;
    for _run in 0..RUNS {
        let mut run_us = 0u128;
        let mut run_allocs = 0u64;
        let mut run_bytes = 0u64;
        for _iter in 0..10 {
            let mut agent = make_agent();
            let (tx, mut rx) = mpsc::unbounded_channel();
            let a0 = ALLOCS.load(Ordering::Relaxed);
            let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
            COUNTING.store(true, Ordering::SeqCst);
            let start = Instant::now();
            agent.run_once_streaming_mpsc("hi", Vec::new(), None, tx).await?;
            let elapsed = start.elapsed();
            COUNTING.store(false, Ordering::SeqCst);
            drain_events(&mut rx);
            run_us += elapsed.as_micros();
            run_allocs += ALLOCS.load(Ordering::Relaxed) - a0;
            run_bytes += ALLOC_BYTES.load(Ordering::Relaxed) - b0;
        }
        times_us.push(run_us);
        last_allocs = run_allocs;
        last_bytes = run_bytes;
    }
    times_us.sort_unstable();
    println!(
        "BENCH spend_cap_inactive_turns_x10: runs={RUNS} min_us={} med_us={} max_us={} allocs={last_allocs} alloc_bytes={last_bytes}",
        times_us[0],
        times_us[RUNS / 2],
        times_us[RUNS - 1]
    );
    Ok(())
}

async fn bench_prewalk_tool_rounds(home: &Path, work: &Path) -> Result<(), Box<dyn Error>> {
    let prewalk_home = home.join("prewalk_home");
    fs::create_dir_all(&prewalk_home)?;
    reset_home(
        &prewalk_home,
        "[tools]\nprofile = \"core\"\n\n[prewalk]\nenabled = true\nplanning = \"plan-m\"\ninto = \"exec-m\"\n",
    )?;
    let ls_dir = work.join("prewalk_dir");
    fs::create_dir_all(&ls_dir)?;

    let build_scripts = || {
        let mut scripts = Vec::with_capacity(13);
        for round in 0..12 {
            scripts.push(vec![
                StreamEvent::ToolUseStart {
                    id: format!("pw-{round}"),
                    name: "ls".to_string(),
                },
                StreamEvent::ToolInputDelta(format!("{{\"path\":\"{}\"}}", ls_dir.display())),
                StreamEvent::ToolUseEnd,
                StreamEvent::MessageEnd {
                    stop_reason: Some("tool_use".to_string()),
                },
            ]);
        }
        scripts.push(vec![
            StreamEvent::TextDelta("done".to_string()),
            StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".to_string()),
            },
        ]);
        scripts
    };

    let make_agent = || async {
        let provider: Arc<dyn Provider> = ScriptedProvider::new("plan-m", build_scripts());
        let registry = Registry::new(Arc::clone(&provider)).await;
        let mut agent = Agent::new(provider, registry);
        agent.set_memory_enabled(false);
        agent.set_working_dir(&ls_dir.display().to_string());
        agent
    };

    // Warmup
    {
        let mut agent = make_agent().await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.run_once_streaming_mpsc("check", Vec::new(), None, tx).await?;
        drain_events(&mut rx);
    }

    let mut times_us = Vec::with_capacity(RUNS);
    let mut last_allocs = 0u64;
    let mut last_bytes = 0u64;
    for _run in 0..RUNS {
        let mut agent = make_agent().await;
        let (tx, mut rx) = mpsc::unbounded_channel();
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
        COUNTING.store(true, Ordering::SeqCst);
        let start = Instant::now();
        agent.run_once_streaming_mpsc("check", Vec::new(), None, tx).await?;
        let elapsed = start.elapsed();
        COUNTING.store(false, Ordering::SeqCst);
        drain_events(&mut rx);
        times_us.push(elapsed.as_micros());
        last_allocs = ALLOCS.load(Ordering::Relaxed) - a0;
        last_bytes = ALLOC_BYTES.load(Ordering::Relaxed) - b0;
    }
    times_us.sort_unstable();
    println!(
        "BENCH prewalk_12_tool_rounds_x1: runs={RUNS} min_us={} med_us={} max_us={} allocs={last_allocs} alloc_bytes={last_bytes}",
        times_us[0],
        times_us[RUNS / 2],
        times_us[RUNS - 1]
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    rt.block_on(async_main())
}

async fn async_main() -> Result<(), Box<dyn Error>> {
    let home = tempfile::tempdir()?;
    let work = tempfile::tempdir()?;
    reset_home(home.path(), "[tools]\nprofile = \"core\"\n")?;

    bench_agentgrep_locate(work.path()).await?;
    bench_prose_repeat(home.path(), work.path()).await?;
    bench_tool_result_trim(home.path(), work.path()).await?;
    bench_enter_package_generation_empty(home.path(), work.path()).await?;
    bench_prompt_and_package_text(home.path(), work.path()).await?;
    bench_spend_cap_inactive(home.path(), work.path()).await?;
    bench_prewalk_tool_rounds(home.path(), work.path()).await?;
    Ok(())
}
