//! Micro-benchmarks for round-3 hosting and harness-API-server optimization paths.
//!
//! Exercises `local_usage`, `BridgeState` `package_control` / `get_effective_settings`
//! translation, `run_bridge_exposed` TCP bearer handshake, and
//! `HostingInputs`/`ExposureInputs` startup config parsing under the release
//! bench profile with a deterministic counting allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::error::Error;
use std::fs;
use std::hint::black_box;
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use horon_agent_base::harness_api::{ExposureInputs, resolve as resolve_exposure};
use horon_agent_base::hosting::HostingInputs;
use horon_agent_harness_api_server::credentials::CredentialStore;
use horon_agent_harness_api_server::run_bridge_exposed;
use horon_agent_harness_api_server::run_bridge_stream;
use horon_agent_harness_api_server::translate::{BridgeState, Outbound, local_usage};
use horon_agent_transport::Listener;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

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

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str, prefer_tmp: bool) -> Result<Self, Box<dyn Error>> {
        let base = if prefer_tmp && Path::new("/tmp").is_dir() {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let path = base.join(format!("hr3h-{label}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path)?;
        Ok(Self { path })
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("bench cleanup {}: {error}", self.path.display());
        }
    }
}

struct CleanupPath {
    path: PathBuf,
    is_dir: bool,
}

impl Drop for CleanupPath {
    fn drop(&mut self) {
        let outcome = if self.is_dir {
            fs::remove_dir_all(&self.path)
        } else {
            fs::remove_file(&self.path)
        };
        if let Err(error) = outcome
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("bench cleanup {}: {error}", self.path.display());
        }
    }
}

fn measure<F: FnMut() -> Result<(), Box<dyn Error>>>(name: &str, mut work: F) -> Result<(), Box<dyn Error>> {
    work()?;
    let mut times_us = Vec::with_capacity(RUNS);
    let mut last_allocs = 0u64;
    let mut last_bytes = 0u64;
    for _ in 0..RUNS {
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

fn seed_usage_home(root: &Path, sessions: usize, msgs_per_session: usize) -> Result<(), Box<dyn Error>> {
    let dates = ["2026-11-21", "2026-11-22", "2026-11-23"];
    for s in 0..sessions {
        let sid = format!("sess_{s:03}");
        let dir = root.join("sessions").join(&sid);
        fs::create_dir_all(&dir)?;
        let file = fs::File::create(dir.join("events.jsonl"))?;
        let mut writer = std::io::BufWriter::new(file);
        writeln!(
            writer,
            "{}",
            json!({
                "event_id": format!("{sid}-created"),
                "session_id": sid,
                "kind": "session_created",
                "payload": {"metadata": {"model": format!("model-{}", s % 3), "provider_key": "openai"}},
            })
        )?;
        for m in 0..msgs_per_session {
            let date = dates[m % dates.len()];
            if m % 10 == 5 {
                writeln!(
                    writer,
                    "{}",
                    json!({
                        "event_id": format!("{sid}-tool-{m}"),
                        "session_id": sid,
                        "kind": "tool_output",
                        "timestamp": format!("{date}T12:00:00Z"),
                        "payload": {"text": "ordinary tool output payload without usage or model metadata repeated several times for realism"},
                    })
                )?;
                continue;
            }
            let event_id = format!("{sid}-m{m}");
            writeln!(
                writer,
                "{}",
                json!({
                    "event_id": event_id,
                    "session_id": sid,
                    "kind": "message_appended",
                    "timestamp": format!("{date}T10:00:00Z"),
                    "payload": {
                        "message": {
                            "id": format!("msg-{m}"),
                            "role": "assistant",
                            "token_usage": {
                                "input_tokens": 100 + u64::try_from(m).unwrap_or(0),
                                "output_tokens": 25,
                                "cache_read_input_tokens": 10
                            }
                        }
                    }
                })
            )?;
            if m % 12 == 0 {
                let revised_date = dates[(m + 1) % dates.len()];
                writeln!(
                    writer,
                    "{}",
                    json!({
                        "event_id": format!("{sid}-rev-{m}"),
                        "session_id": sid,
                        "kind": "message_revised",
                        "timestamp": format!("{revised_date}T11:00:00Z"),
                        "payload": {
                            "target_event_id": event_id,
                            "replacement": {
                                "id": format!("msg-{m}"),
                                "role": "assistant",
                                "token_usage": {
                                    "input_tokens": 150 + u64::try_from(m).unwrap_or(0),
                                    "output_tokens": 35,
                                    "cache_read_input_tokens": 12
                                }
                            }
                        }
                    })
                )?;
            }
        }
        writer.flush()?;
    }
    Ok(())
}

async fn spawn_fake_daemon(socket_path: PathBuf) -> Result<(), Box<dyn Error>> {
    let mut listener = Listener::bind(&socket_path)?;
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (read_half, mut write_half) = stream.into_split();
                let mut reader = BufReader::new(read_half);
                let mut line = String::new();
                loop {
                    line.clear();
                    match reader.read_line(&mut line).await {
                        Ok(0) | Err(_) => return,
                        Ok(_) => {}
                    }
                    let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
                        continue;
                    };
                    if value["type"] == "ping" {
                        let reply = format!(
                            "{}\n",
                            json!({
                                "type": "pong",
                                "id": value["id"],
                                "capabilities": [
                                    "runtime_commands",
                                    "package_control",
                                    "effective_settings_api"
                                ],
                            })
                        );
                        if write_half.write_all(reply.as_bytes()).await.is_err() {
                            return;
                        }
                        if write_half.flush().await.is_err() {
                            return;
                        }
                    }
                }
            });
        }
    });
    Ok(())
}

async fn enable_bridge_capabilities(legacy_socket: &Path) -> Result<(), Box<dyn Error>> {
    let (mut client, server) = tokio::io::duplex(8192);
    let (server_read, server_write) = tokio::io::split(server);
    let legacy = legacy_socket.to_path_buf();
    let bridge_task = tokio::spawn(async move { run_bridge_stream(server_read, server_write, legacy).await });
    client
        .write_all(b"{\"id\":1,\"req\":\"hello\",\"min_version\":1,\"max_version\":1,\"client\":\"bench\"}\n")
        .await?;
    let mut reader = BufReader::new(client);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    drop(reader);
    bridge_task.await??;
    Ok(())
}

fn free_loopback_port() -> Result<u16, Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    Ok(listener.local_addr()?.port())
}

fn write_realistic_config(path: &Path, include_sections: bool) -> Result<(), Box<dyn Error>> {
    let mut text = String::with_capacity(16 * 1024);
    text.push_str("[provider]\ndefault_provider = \"openai\"\ndefault_model = \"gpt-5\"\n\n");
    for idx in 0..40 {
        text.push_str(&format!(
            "[providers.p{idx}]\ntype = \"openai-compatible\"\nbase_url = \"https://api.example{idx}.invalid/v1\"\n\
             default_model = \"model-{idx}\"\napi_key_env = \"KEY_{idx}\"\n\
             models = [{{ id = \"model-{idx}\", context_window = 128000 }}, {{ id = \"model-{idx}-fast\" }}]\n\n"
        ));
    }
    if include_sections {
        text.push_str("[hosting]\nallow_shared_children = true\nallowed_mcp_servers = [\"docs\", \"git\"]\n\n");
        text.push_str("[harness_api]\nbind = \"127.0.0.1:7946\"\ninsecure_behind_proxy = false\n");
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let usage_root = TempDir::new("usage", false)?;
    seed_usage_home(&usage_root.path, 50, 200)?;
    measure("hosting_local_usage_50x200", || {
        let report = local_usage(&usage_root.path, Some("2026-11-22"), Some("2026-11-23"))?;
        black_box(report.rows.len());
        Ok(())
    })?;

    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let sock_root = TempDir::new("sock", true)?;
    let legacy_socket = sock_root.path.join("daemon.sock");
    let api_socket = sock_root.path.join("api.sock");
    rt.block_on(spawn_fake_daemon(legacy_socket.clone()))?;
    rt.block_on(enable_bridge_capabilities(&legacy_socket))?;

    let pkg_req = json!({
        "v": 1,
        "id": 1,
        "req": "package_control",
        "operation_id": "op-1",
        "operation": {
            "op": "install",
            "source": "/tmp/example-plugin-directory",
            "scope": "global",
            "replace": true
        }
    });
    let settings_req = json!({
        "v": 1,
        "id": 2,
        "req": "get_effective_settings",
        "session_id": "s1"
    });
    let mut settings_rows = Vec::with_capacity(20);
    for idx in 0..20 {
        settings_rows.push(json!({
            "key": format!("setting.key.{idx}"),
            "label": format!("Setting label {idx}"),
            "value": format!("value-{idx}"),
            "source": "file",
            "origin": "/data/alice/home/config.toml"
        }));
    }
    let pkg_receipt_base = json!({
        "type": "package_control_result",
        "id": 1,
        "receipt": {
            "operation_id": "op-1",
            "generation": 42,
            "result": {
                "kind": "list",
                "packages": [
                    {
                        "id": "demo-pkg",
                        "version": "1.0.0",
                        "name": "Demo Package",
                        "scope": "global",
                        "enabled": true,
                        "status": "active",
                        "generation": 42,
                        "source": "/tmp/demo"
                    }
                ]
            }
        }
    });
    let settings_reply_base = json!({
        "type": "effective_settings",
        "id": 2,
        "settings": settings_rows
    });

    let mut pkg_events = vec![pkg_receipt_base; 200];
    let mut set_events = vec![settings_reply_base; 200];

    let mut state = BridgeState::default();
    state.session_id = Some("s1".to_string());
    measure("hosting_translate_pkg_settings_200", || {
        let mut count = 0usize;
        for idx in 0..200 {
            let pkg_out = state.api_request_to_legacy(&pkg_req);
            let pkg_legacy_id = match pkg_out.first() {
                Some(Outbound::Legacy(val)) => val["id"].as_u64().unwrap_or(0),
                _ => 0,
            };
            let pkg_ev = &mut pkg_events[idx];
            pkg_ev["id"] = Value::from(pkg_legacy_id);
            let pkg_frames = state.legacy_event_to_api(pkg_ev);
            count = count.wrapping_add(pkg_frames.len());

            let set_out = state.api_request_to_legacy(&settings_req);
            let set_legacy_id = match set_out.first() {
                Some(Outbound::Legacy(val)) => val["id"].as_u64().unwrap_or(0),
                _ => 0,
            };
            let set_ev = &mut set_events[idx];
            set_ev["id"] = Value::from(set_legacy_id);
            let set_frames = state.legacy_event_to_api(set_ev);
            count = count.wrapping_add(set_frames.len());
        }
        black_box(count);
        Ok(())
    })?;

    let cred_store = CredentialStore::default_store()?;
    let cred_dir = match cred_store.path().parent() {
        Some(parent) => parent.to_path_buf(),
        None => sock_root.path.clone(),
    };
    let _cred_cleanup = CleanupPath {
        path: cred_dir,
        is_dir: true,
    };
    let issued = cred_store.issue(Some("bench"))?;
    let port = free_loopback_port()?;
    let bind_addr = format!("127.0.0.1:{port}");
    let exposure_inputs = ExposureInputs {
        bind: Some(bind_addr.clone()),
        insecure_behind_proxy: Some(false),
        section: Default::default(),
        credentials_available: true,
    };
    let exposure = match resolve_exposure(&exposure_inputs)? {
        Some(exposure) => exposure,
        None => return Err("expected loopback exposure".into()),
    };
    let bridge_api = api_socket.clone();
    let bridge_legacy = legacy_socket.clone();
    let bridge_handle = rt.spawn(async move {
        match run_bridge_exposed(bridge_api, bridge_legacy, &exposure).await {
            Ok(()) => {}
            Err(error) => eprintln!("bridge error: {error}"),
        }
    });
    let auth_header = format!(
        "Authorization: Bearer {}\n{{\"id\":1,\"req\":\"hello\",\"min_version\":1,\"max_version\":1,\"client\":\"bench\"}}\n",
        issued.token
    );
    rt.block_on(async {
        for _ in 0..100 {
            if let Ok(mut stream) = TcpStream::connect(&bind_addr).await {
                stream.write_all(auth_header.as_bytes()).await?;
                stream.flush().await?;
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                reader.read_line(&mut line).await?;
                if line.contains("hello_ok") {
                    return Ok::<(), Box<dyn Error>>(());
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Err("bridge TCP listener did not become ready".into())
    })?;

    measure("hosting_tcp_auth_handshake_10", || {
        rt.block_on(async {
            let mut bytes_read = 0usize;
            for _ in 0..10 {
                let mut stream = TcpStream::connect(&bind_addr).await?;
                stream.write_all(auth_header.as_bytes()).await?;
                stream.flush().await?;
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                bytes_read = bytes_read.wrapping_add(reader.read_line(&mut line).await?);
            }
            black_box(bytes_read);
            Ok::<(), Box<dyn Error>>(())
        })
    })?;
    bridge_handle.abort();

    let cfg_path = match horon_agent_base::config::Config::path() {
        Some(path) => path,
        None => return Err("missing Config::path() in isolated HORON_HOME".into()),
    };
    let _cfg_cleanup = CleanupPath {
        path: cfg_path.clone(),
        is_dir: false,
    };
    write_realistic_config(&cfg_path, false)?;
    measure("hosting_env_config_absent_sections_50", || {
        let mut count = 0usize;
        for _ in 0..50 {
            let h = HostingInputs::from_environment();
            let e = ExposureInputs::from_environment(None, false);
            count = count.wrapping_add(usize::from(h.section.allow_shared_children.is_some()));
            count = count.wrapping_add(usize::from(e.section.bind.is_some()));
        }
        black_box(count);
        Ok(())
    })?;

    write_realistic_config(&cfg_path, true)?;
    measure("hosting_env_config_with_sections_50", || {
        let mut count = 0usize;
        for _ in 0..50 {
            let h = HostingInputs::from_environment();
            let e = ExposureInputs::from_environment(None, false);
            count = count.wrapping_add(usize::from(h.section.allow_shared_children.is_some()));
            count = count.wrapping_add(usize::from(e.section.bind.is_some()));
        }
        black_box(count);
        Ok(())
    })?;

    Ok(())
}
