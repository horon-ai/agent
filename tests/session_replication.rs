//! Real-surface replication QA: two separate HOMEs, each with a real
//! `horon-agent serve` daemon and a real `api-bridge`, driven through the Rust
//! SDK and (when enabled) the TypeScript SDK.
//!
//! Everything here crosses a process and a Unix socket. The stores are the
//! production layout under each HOME, so an import that only "works" in memory
//! fails here.
//!
//! Set `HORON_AGENT_TS_QA=1` with `packages/sdk/typescript/dist` built to run the
//! TypeScript leg against the same sockets.

#![cfg(unix)]

use horon_agent::session::event_store_api as store;
use horon_agent_sdk::{ConnectOptions, ErrorKind, HoronAgentClient, SessionEventPage};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BINARY: &str = env!("CARGO_BIN_EXE_horon-agent");

struct Instance {
    root: PathBuf,
    home: PathBuf,
    api_socket: PathBuf,
    daemon: Child,
    bridge: Child,
}

impl Instance {
    fn start(root: &Path, label: &str) -> Self {
        let base = root.join(label);
        let home = base.join("home");
        let runtime = base.join("rt");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&runtime).expect("runtime");
        let api_socket = base.join("api.sock");
        let daemon = spawn(&home, &runtime, &["serve", "--no-update"]);
        wait_for_socket(&runtime.join("horon-agent.sock"), Duration::from_secs(30));
        let bridge = spawn(
            &home,
            &runtime,
            &[
                "api-bridge",
                "--no-update",
                "--api-socket",
                api_socket.to_str().expect("utf-8 socket path"),
            ],
        );
        wait_for_socket(&api_socket, Duration::from_secs(30));
        Self {
            root: base,
            home,
            api_socket,
            daemon,
            bridge,
        }
    }

    fn client(&self) -> HoronAgentClient {
        HoronAgentClient::connect(ConnectOptions {
            socket_path: Some(self.api_socket.clone()),
            client_name: "replication-qa/1.0".into(),
            request_timeout: Some(Duration::from_secs(30)),
            ensure_runtime: false,
            supports_user_response: false,
        })
        .unwrap_or_else(|error| panic!("connect to the bridge: {error}\n{}", self.logs()))
    }

    fn logs(&self) -> String {
        let read = |name: &str| std::fs::read_to_string(self.root.join("rt").join(name)).unwrap_or_default();
        format!(
            "daemon.log:\n{}\nbridge.log:\n{}",
            read("daemon.log"),
            read("bridge.log")
        )
    }

    fn paths(&self) -> store::SessionPaths {
        store::SessionPaths::new(self.home.join("sessions"))
    }

    fn stop(&mut self) {
        let _ = self.bridge.kill();
        let _ = self.daemon.kill();
        let _ = self.bridge.wait();
        let _ = self.daemon.wait();
    }

    /// Stop both processes and start fresh ones in the same HOME, keeping the
    /// stored sessions. Replacing the whole value instead would drop the old
    /// instance, which deletes the directory the new one just bound.
    fn restart(&mut self) {
        self.stop();
        let base = self.root.clone();
        let home = base.join("home");
        let runtime = base.join("rt");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&runtime).expect("runtime");
        let api_socket = base.join("api.sock");
        let daemon = spawn(&home, &runtime, &["serve", "--no-update"]);
        wait_for_socket(&runtime.join("horon-agent.sock"), Duration::from_secs(30));
        let bridge = spawn(
            &home,
            &runtime,
            &[
                "api-bridge",
                "--no-update",
                "--api-socket",
                api_socket.to_str().expect("utf-8 socket path"),
            ],
        );
        wait_for_socket(&api_socket, Duration::from_secs(30));
        self.api_socket = api_socket;
        self.daemon = daemon;
        self.bridge = bridge;
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.stop();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn spawn(home: &Path, runtime: &Path, args: &[&str]) -> Child {
    let log_name = if args.first() == Some(&"serve") {
        "daemon.log"
    } else {
        "bridge.log"
    };
    let log = std::fs::File::create(runtime.join(log_name)).expect("log file");
    let errors = log.try_clone().expect("log clone");
    Command::new(BINARY)
        .args(args)
        .env("HORON_HOME", home)
        .env("HORON_AGENT_RUNTIME_DIR", runtime)
        .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
        .env("HORON_AGENT_MEMORY_ENABLED", "0")
        .env("HORON_AGENT_MEMORY_SIDECAR_ENABLED", "0")
        .env("HORON_AGENT_NON_INTERACTIVE", "1")
        .env_remove("HORON_AGENT_SOCKET")
        .env_remove("HORON_AGENT_API_SOCKET")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(errors))
        .spawn()
        .expect("spawn horon-agent")
}

/// Serializes the process-spawning tests: each starts four child processes,
/// and running three of them at once starves the daemons' startup window.
fn instance_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn wait_for_socket(path: &Path, deadline: Duration) {
    let start = Instant::now();
    loop {
        if UnixStream::connect(path).is_ok() {
            return;
        }
        if start.elapsed() >= deadline {
            let log = path
                .parent()
                .and_then(|dir| std::fs::read_to_string(dir.join("daemon.log")).ok())
                .unwrap_or_default();
            panic!(
                "socket {} never became connectable within {deadline:?}; daemon log:\n{log}",
                path.display()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn scratch_root(label: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let short: String = label.chars().filter(|ch| ch.is_ascii_alphanumeric()).take(4).collect();
    let root = PathBuf::from(format!("/private/tmp/hs{}-{short}{n}", std::process::id()));
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

fn text_message(id: &str, role: &str, text: &str) -> store::MessageAppendedPayload {
    store::MessageAppendedPayload {
        message: serde_json::from_value(json!({
            "id": id,
            "role": role,
            "content": [{"type": "text", "text": text}]
        }))
        .expect("stored message"),
    }
}

fn seed_blob_session(paths: &store::SessionPaths, id: &str, text_size: usize) -> store::SessionId {
    let session = store::SessionId::parse(id).expect("session id");
    let mut writer = store::BlockingWriter::open(paths.clone(), session.clone()).expect("writer");
    let root = store::EventDraft::new(
        session.clone(),
        store::EventId::new_v4(),
        None,
        store::OriginId::new_v4(),
        store::SessionMetadata::default().created_at,
        store::EventKind::new(store::KnownEventKind::SessionCreated),
        json!(store::SessionCreatedPayload {
            branch_root: false,
            reset_provenance: None,
            metadata: store::SessionMetadata::default(),
        }),
    )
    .seal()
    .expect("root");
    writer.append(&root).expect("append root");
    let text = "b".repeat(text_size);
    let event = store::EventDraft::new(
        session.clone(),
        store::EventId::new_v4(),
        Some(root.envelope().event_id.clone()),
        store::OriginId::new_v4(),
        store::SessionMetadata::default().created_at,
        store::EventKind::new(store::KnownEventKind::MessageAppended),
        json!(store::MessageAppendedPayload {
            message: text_message("big", "user", &text).message,
        }),
    )
    .seal()
    .expect("big message");
    assert!(
        matches!(event, store::SealedEvent::Blob { .. }),
        "a 9 MiB message must externalize its payload"
    );
    writer.append(&event).expect("append big message");
    session
}

fn canonical_pairs(page: &SessionEventPage) -> Vec<(String, String)> {
    page.events
        .iter()
        .map(|event| (event.event_id.clone(), event.canonical_json.clone()))
        .collect()
}

struct ScratchRoot(PathBuf);

impl Drop for ScratchRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn two_home_replication_history_blob_restart_and_failures() {
    let _serial = instance_lock();
    let root = scratch_root("two-home");
    let _root_guard = ScratchRoot(root.clone());
    let mut a = Instance::start(&root, "a");
    let mut b = Instance::start(&root, "b");
    let result = (|| -> Result<(), String> {
        let client_a = a.client();
        let client_b = b.client();
        assert!(client_a.supports("session_replication_v1"));
        assert!(client_a.supports("session_event_store_v1"));
        assert!(client_a.supports("session_history_cursor_v1"));

        let session = client_a
            .create_session(Some(root.display().to_string()))
            .map_err(|error| format!("create_session: {error}"))?;
        let id = session.session_id.clone();
        client_a
            .send_context_message(&id, "replication fixture message")
            .map_err(|error| format!("send_context_message: {error}"))?;

        let page = client_a
            .export_session_events(&id, Vec::new(), Vec::new(), None)
            .map_err(|error| format!("export: {error}"))?;
        assert!(page.events.len() >= 2, "root plus one message: {page:?}");
        assert!(page.next_cursor.is_none());

        let imported = client_b
            .import_session_events(&page, None)
            .map_err(|error| format!("import: {error}"))?;
        assert_eq!(imported.accepted_ids.len(), page.events.len());
        assert_eq!(imported.heads, page.heads);

        let heads_b = client_b
            .list_session_heads(&id)
            .map_err(|error| format!("heads: {error}"))?;
        assert_eq!(heads_b.heads, page.heads);
        let history = client_b
            .get_history_page(&id, None, None, Some(10))
            .map_err(|error| format!("history: {error}"))?;
        assert_eq!(history.head, page.heads[0]);
        assert!(
            history
                .messages
                .iter()
                .any(|message| message.content.contains("replication fixture message")),
            "imported history must contain the message: {history:?}"
        );

        let reexport = client_b
            .export_session_events(&id, Vec::new(), Vec::new(), None)
            .map_err(|error| format!("re-export: {error}"))?;
        assert_eq!(
            canonical_pairs(&reexport),
            canonical_pairs(&page),
            "imported canonical bytes must be identical, not re-serialized"
        );

        let again = client_b
            .import_session_events(&page, None)
            .map_err(|error| format!("retransmit: {error}"))?;
        assert_eq!(again.accepted_ids, imported.accepted_ids);
        let after = client_b
            .export_session_events(&id, Vec::new(), Vec::new(), None)
            .map_err(|error| format!("export after retransmit: {error}"))?;
        assert_eq!(after.events.len(), reexport.events.len());

        let invalid = client_b
            .get_history_page(&id, None, Some("00000000-0000-4000-8000-000000000000"), Some(5))
            .expect_err("unknown cursor must be refused");
        assert_eq!(
            invalid.kind,
            ErrorKind::Harness(horon_agent_sdk::api::ErrorCode::InvalidRequest)
        );
        let details = invalid.details.expect("typed detail");
        assert_eq!(details.store_code, "invalid_cursor");

        // The cursor walks the whole head on the real socket, one page at a
        // time. Each page is oldest-first inside itself and `next_cursor` moves
        // backwards, so pages are collected and then put in transcript order.
        let mut pages: Vec<Vec<String>> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let page = client_b
                .get_history_page(&id, None, cursor.as_deref(), Some(1))
                .map_err(|error| format!("cursor page: {error}"))?;
            pages.push(page.messages.iter().map(|message| message.content.clone()).collect());
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
            assert!(pages.len() <= 16, "cursor page walk did not terminate");
        }
        let walked: Vec<String> = pages.into_iter().rev().flatten().collect();
        let whole = client_b
            .get_history_page(&id, None, None, Some(50))
            .map_err(|error| format!("whole history: {error}"))?;
        assert_eq!(
            walked,
            whole
                .messages
                .iter()
                .map(|message| message.content.clone())
                .collect::<Vec<_>>(),
            "paging must equal one full read"
        );
        assert!(walked.iter().any(|text| text.contains("replication fixture message")));

        // A conflicting record over the real socket keeps its typed cause.
        let mut forged = page.events[1].clone();
        let mut value: Value =
            serde_json::from_str(&forged.canonical_json).map_err(|error| format!("parse canonical: {error}"))?;
        value["payload"]["message"]["content"][0]["text"] = json!("forged over the socket");
        forged.canonical_json = store::canonical_json(&value).map_err(|error| format!("recanonicalize: {error}"))?;
        forged.content_hash = store::sha256_hex(b"different-hash");
        let mut forged_page = page.clone();
        forged_page.events = vec![page.events[0].clone(), forged];
        let conflict = client_b
            .import_session_events(&forged_page, None)
            .expect_err("a conflicting record must be refused");
        let conflict_details = conflict.details.expect("typed detail");
        assert_eq!(conflict_details.store_code, "hash_conflict");
        assert!(!conflict_details.retryable);

        let oversized = SessionEventPage {
            session_id: id.clone(),
            heads: Vec::new(),
            events: vec![horon_agent_sdk::api::CanonicalEventRecord {
                event_id: "11111111-1111-4111-8111-111111111111".into(),
                content_hash: "a".repeat(64),
                canonical_json: "x".repeat(17 * 1024 * 1024),
            }],
            next_cursor: None,
            blob_refs: Vec::new(),
            record_refs: Vec::new(),
        };
        let error = client_b
            .import_session_events(&oversized, None)
            .expect_err("oversize envelope must be refused");
        assert_eq!(error.details.expect("typed detail").store_code, "record_too_large");

        let writer =
            store::BlockingWriter::open(b.paths(), store::SessionId::parse(&id).expect("id")).expect("hold B's writer");
        let busy = client_b
            .select_session_head(&id, &page.heads[0])
            .expect_err("a held writer must answer busy");
        let busy_details = busy.details.expect("typed detail");
        assert_eq!(busy_details.store_code, "busy");
        assert!(busy_details.retryable);
        drop(writer);

        let old = raw_old_client(&a.api_socket);
        assert_eq!(old["version"], 1);
        assert!(old["capabilities"].as_array().is_some());

        // A 17 MiB replication frame is above the 16 MiB API envelope but below
        // the bridge's 64 MiB transport cap, so the refusal must come from the
        // API layer and the connection must stay usable.
        let oversized_frame = oversized_frame_refusal(&a.api_socket);
        assert_eq!(oversized_frame["ev"], "error");
        assert_eq!(oversized_frame["code"], "invalid_request");
        assert_eq!(oversized_frame["details"]["store_code"], "record_too_large");

        client_a
            .send_context_message(&id, "after restart marker")
            .map_err(|error| format!("second message: {error}"))?;
        let before_restart = client_a
            .export_session_events(&id, Vec::new(), Vec::new(), None)
            .map_err(|error| format!("export before restart: {error}"))?;
        client_b
            .import_session_events(&before_restart, None)
            .map_err(|error| format!("import before restart: {error}"))?;
        drop(client_a);
        drop(client_b);
        b.restart();
        let client_b = b.client();
        let heads_after = client_b
            .list_session_heads(&id)
            .map_err(|error| format!("heads after restart: {error}"))?;
        assert_eq!(heads_after.heads, before_restart.heads);
        let history_after = client_b
            .get_history_page(&id, None, None, Some(10))
            .map_err(|error| format!("history after restart: {error}"))?;
        assert!(
            history_after
                .messages
                .iter()
                .any(|message| message.content.contains("after restart marker"))
        );
        Ok(())
    })();

    if let Err(error) = &result {
        eprintln!("two-home replication QA failed: {error}");
    }
    a.stop();
    b.stop();
    let _ = std::fs::remove_dir_all(&root);
    result.expect("two-home replication QA");
}

#[test]
fn maximum_record_travels_by_reference_through_the_page_flow() {
    let _serial = instance_lock();
    let root = scratch_root("record");
    let _root_guard = ScratchRoot(root.clone());
    let _root_guard = ScratchRoot(root.clone());
    let mut a = Instance::start(&root, "a");
    let mut b = Instance::start(&root, "b");
    let result = (|| -> Result<(), String> {
        let session = store::SessionId::parse("record_ref_session").map_err(|error| format!("session id: {error}"))?;
        let timestamp = store::SessionMetadata::default().created_at;
        let mut writer =
            store::BlockingWriter::open(a.paths(), session.clone()).map_err(|error| format!("writer: {error}"))?;
        let created = store::EventDraft::new(
            session.clone(),
            store::EventId::new_v4(),
            None,
            store::OriginId::new_v4(),
            timestamp,
            store::EventKind::new(store::KnownEventKind::SessionCreated),
            json!(store::SessionCreatedPayload {
                branch_root: false,
                reset_provenance: None,
                metadata: store::SessionMetadata::default(),
            }),
        )
        .seal()
        .map_err(|error| format!("seal root: {error}"))?;
        writer
            .append(&created)
            .map_err(|error| format!("append root: {error}"))?;

        let empty = store::EventDraft::new(
            session.clone(),
            store::EventId::new_v4(),
            Some(created.envelope().event_id.clone()),
            store::OriginId::new_v4(),
            timestamp,
            store::EventKind::new(store::KnownEventKind::MessageAppended),
            json!(store::MessageAppendedPayload {
                message: text_message("big", "user", "").message,
            }),
        )
        .seal()
        .map_err(|error| format!("seal empty: {error}"))?;
        let overhead = store::encode_record_line(empty.envelope())
            .map_err(|error| format!("encode empty: {error}"))?
            .len();
        let quotes = (store::MAX_RECORD_BYTES - overhead) / 2;
        let big_text: String = "\"".repeat(quotes);
        let big = store::EventDraft::new(
            session.clone(),
            store::EventId::new_v4(),
            Some(created.envelope().event_id.clone()),
            store::OriginId::new_v4(),
            timestamp,
            store::EventKind::new(store::KnownEventKind::MessageAppended),
            json!(store::MessageAppendedPayload {
                message: text_message("big", "user", &big_text).message,
            }),
        )
        .seal()
        .map_err(|error| format!("seal maximum: {error}"))?;
        assert!(
            big.blob().is_none(),
            "the maximum record must stay inline in the log, not externalize its payload"
        );
        let big_line = store::encode_record_line(big.envelope()).map_err(|error| format!("encode maximum: {error}"))?;
        assert!(
            big_line.len() >= store::MAX_RECORD_BYTES - 1,
            "fixture must sit on the record limit, got {}",
            big_line.len()
        );
        let big_id = big.envelope().event_id.clone();
        writer
            .append(&big)
            .map_err(|error| format!("append maximum: {error}"))?;
        let tail = store::EventDraft::new(
            session.clone(),
            store::EventId::new_v4(),
            Some(big_id.clone()),
            store::OriginId::new_v4(),
            timestamp,
            store::EventKind::new(store::KnownEventKind::MessageAppended),
            json!(store::MessageAppendedPayload {
                message: text_message("tail", "assistant", "tail after the maximum").message,
            }),
        )
        .seal()
        .map_err(|error| format!("seal tail: {error}"))?;
        writer.append(&tail).map_err(|error| format!("append tail: {error}"))?;
        drop(writer);

        let id = session.as_str().to_owned();
        let client_a = a.client();
        let client_b = b.client();
        let page = client_a
            .export_session_events(&id, Vec::new(), Vec::new(), None)
            .map_err(|error| format!("export page 1: {error}"))?;
        eprintln!(
            "OBSERVED page 1: inline={} refs={} cursor={} envelope={}",
            page.events.len(),
            page.record_refs.len(),
            page.next_cursor.is_some(),
            serde_json::to_vec(&page).map(|bytes| bytes.len()).unwrap_or(0),
        );
        assert_eq!(page.events.len(), 1, "page 1 carries the root inline");
        assert_eq!(page.events[0].event_id, created.envelope().event_id.as_str());
        assert!(
            page.record_refs.is_empty(),
            "the maximum record is deferred, not inline and not yet referenced"
        );
        let cursor = page
            .next_cursor
            .clone()
            .ok_or_else(|| "page 1 must leave a continuation cursor".to_string())?;
        client_b
            .import_session_events(&page, None)
            .map_err(|error| format!("import page 1: {error}"))?;

        let page2 = client_a
            .export_session_events(&id, page.heads.clone(), Vec::new(), Some(cursor))
            .map_err(|error| format!("export page 2: {error}"))?;
        eprintln!(
            "OBSERVED page 2: inline={} refs={} cursor={} envelope={}",
            page2.events.len(),
            page2.record_refs.len(),
            page2.next_cursor.is_some(),
            serde_json::to_vec(&page2).map(|bytes| bytes.len()).unwrap_or(0),
        );
        assert_eq!(
            page2.record_refs.len(),
            1,
            "the maximum record must travel by reference"
        );
        let reference = page2.record_refs[0].clone();
        assert_eq!(reference.position, 0);
        assert_eq!(reference.event_id, big_id.as_str());
        assert!(
            reference.sha256 != reference.content_hash,
            "record hash and event content hash are distinct values"
        );
        assert_eq!(page2.events.len(), 1, "the tail travels inline beside the reference");
        assert_eq!(page2.events[0].event_id, tail.envelope().event_id.as_str());
        assert!(page2.next_cursor.is_none(), "page 2 completes the fixed head");
        let envelope = serde_json::to_vec(&page2).map_err(|error| format!("serialize page 2: {error}"))?;
        assert!(
            envelope.len() <= 16 * 1024 * 1024,
            "the actual page envelope must stay inside 16 MiB, got {}",
            envelope.len()
        );

        let graph = store::EventGraph::open(&a.paths(), &session).map_err(|error| format!("open graph: {error}"))?;
        let record = graph
            .event(&big_id)
            .ok_or_else(|| "maximum record missing".to_string())?
            .canonical_record()
            .map_err(|error| format!("canonical record: {error}"))?;
        let root_record = graph
            .event(&created.envelope().event_id)
            .ok_or_else(|| "root record missing".to_string())?
            .canonical_record()
            .map_err(|error| format!("root record: {error}"))?;
        let tail_record = graph
            .event(&tail.envelope().event_id)
            .ok_or_else(|| "tail record missing".to_string())?
            .canonical_record()
            .map_err(|error| format!("tail record: {error}"))?;
        let inline = store::replication::SessionEventPage {
            protocol_version: store::replication::REPLICATION_PROTOCOL_VERSION,
            session_id: session.clone(),
            heads: page
                .heads
                .iter()
                .map(|head| store::EventId::parse(head).expect("head id"))
                .collect(),
            events: vec![root_record, record.clone(), tail_record],
            next_cursor: None,
            blob_refs: Vec::new(),
            record_refs: Vec::new(),
        };
        let inline_size = serde_json::to_vec(&inline)
            .map_err(|error| format!("serialize inline: {error}"))?
            .len();
        assert!(
            inline_size > 16 * 1024 * 1024,
            "the same page with the maximum record inline must exceed 16 MiB, got {inline_size}"
        );

        let mut offset = 0u64;
        let mut bytes = Vec::new();
        while offset < reference.size {
            let chunk = client_a
                .export_session_record_blob(&id, &reference, offset)
                .map_err(|error| format!("export record at {offset}: {error}"))?;
            let status = client_b
                .import_session_record_blob(&id, &reference, &chunk)
                .map_err(|error| format!("import record at {offset}: {error}"))?;
            bytes.extend(base64_decode(&chunk.data_base64)?);
            offset = chunk.next_offset;
            if chunk.complete {
                assert!(status.complete);
            }
        }
        assert_eq!(bytes.len() as u64, reference.size);
        assert_eq!(store::sha256_hex(&bytes), reference.sha256);
        assert_eq!(bytes, record.canonical_json.as_bytes());

        let imported = client_b
            .import_session_events(&page2, None)
            .map_err(|error| format!("import page 2: {error}"))?;
        assert_eq!(
            imported.accepted_ids,
            vec![big_id.as_str().to_owned(), tail.envelope().event_id.as_str().to_owned()],
            "the referenced maximum and the inline tail both commit"
        );

        let history = client_b
            .get_history_page(&id, None, None, Some(10))
            .map_err(|error| format!("history: {error}"))?;
        assert_eq!(history.messages.len(), 2, "maximum plus tail");
        assert_eq!(history.messages[0].content.chars().count(), quotes);
        assert_eq!(history.messages[1].content, "tail after the maximum");

        let reexport = client_b
            .export_session_events(&id, Vec::new(), Vec::new(), None)
            .map_err(|error| format!("re-export page 1: {error}"))?;
        let reexport_cursor = reexport
            .next_cursor
            .clone()
            .ok_or_else(|| "re-export page 1 must leave a cursor".to_string())?;
        let reexport2 = client_b
            .export_session_events(&id, reexport.heads.clone(), Vec::new(), Some(reexport_cursor))
            .map_err(|error| format!("re-export page 2: {error}"))?;
        assert_eq!(reexport2.record_refs.len(), 1);
        assert_eq!(reexport2.record_refs[0].sha256, reference.sha256);
        assert_eq!(reexport2.record_refs[0].size, reference.size);
        Ok(())
    })();

    if let Err(error) = &result {
        eprintln!("maximum record QA failed: {error}");
    }
    a.stop();
    b.stop();
    let _ = std::fs::remove_dir_all(&root);
    result.expect("maximum record QA");
}

#[test]
fn payload_blob_crosses_two_homes_and_is_verified() {
    let _serial = instance_lock();
    let root = scratch_root("blob");
    let _root_guard = ScratchRoot(root.clone());
    let mut a = Instance::start(&root, "a");
    let mut b = Instance::start(&root, "b");
    let result = (|| -> Result<(), String> {
        let session = seed_blob_session(&a.paths(), "blob_session", 9 * 1024 * 1024);
        let id = session.as_str().to_owned();
        let client_a = a.client();
        let client_b = b.client();

        let page = client_a
            .export_session_events(&id, Vec::new(), Vec::new(), None)
            .map_err(|error| format!("export: {error}"))?;
        let reference = page
            .blob_refs
            .first()
            .ok_or_else(|| "a 9 MiB message must advertise a payload blob".to_string())?
            .clone();

        let chunk = client_a
            .export_session_blob(&id, &reference.sha256, reference.size, 0)
            .map_err(|error| format!("export blob: {error}"))?;
        assert!(!chunk.complete, "9 MiB needs several 1 MiB chunks");
        let status = client_b
            .import_session_blob(&id, &chunk)
            .map_err(|error| format!("import blob: {error}"))?;
        assert_eq!(status.received, chunk.next_offset);
        let mut offset = chunk.next_offset;
        while offset < reference.size {
            let chunk = client_a
                .export_session_blob(&id, &reference.sha256, reference.size, offset)
                .map_err(|error| format!("export blob at {offset}: {error}"))?;
            let status = client_b
                .import_session_blob(&id, &chunk)
                .map_err(|error| format!("import blob at {offset}: {error}"))?;
            offset = chunk.next_offset;
            if chunk.complete {
                assert!(status.complete);
            }
        }
        let resumed = client_b
            .session_blob_status(&id, &reference.sha256, reference.size)
            .map_err(|error| format!("blob status: {error}"))?;
        assert!(resumed.complete);
        assert_eq!(resumed.received, reference.size);

        let imported = client_b
            .import_session_events(&page, None)
            .map_err(|error| format!("import events: {error}"))?;
        assert_eq!(imported.accepted_ids.len(), page.events.len());
        let history = client_b
            .get_history_page(&id, None, None, Some(5))
            .map_err(|error| format!("history: {error}"))?;
        assert_eq!(
            history.messages.last().map(|message| message.content.len()),
            Some(9 * 1024 * 1024)
        );
        let source_blob = std::fs::read(a.paths().blob_file(&session, &reference.sha256).expect("blob path"))
            .map_err(|error| format!("read source blob: {error}"))?;
        let destination_blob = std::fs::read(b.paths().blob_file(&session, &reference.sha256).expect("blob path"))
            .map_err(|error| format!("read destination blob: {error}"))?;
        assert_eq!(destination_blob, source_blob);

        // A whole canonical record travels by reference on the same sockets.
        // The descriptor comes from the real stored record, so the bytes
        // compared below are the ones the store wrote, not a fixture copy.
        let graph = store::EventGraph::open(&a.paths(), &session).map_err(|error| format!("open graph: {error}"))?;
        let head = graph.heads()[0].clone();
        let record = graph
            .event(&head)
            .ok_or_else(|| "head record missing".to_string())?
            .canonical_record()
            .map_err(|error| format!("canonical record: {error}"))?;
        let record_ref = horon_agent_sdk::api::CanonicalRecordRef {
            position: 0,
            event_id: record.event_id.as_str().to_owned(),
            content_hash: record.content_hash.clone(),
            sha256: store::sha256_hex(record.canonical_json.as_bytes()),
            size: record.canonical_json.len() as u64,
        };
        let mut offset = 0u64;
        let mut record_bytes = Vec::new();
        while offset < record_ref.size {
            let chunk = client_a
                .export_session_record_blob(&id, &record_ref, offset)
                .map_err(|error| format!("export record at {offset}: {error}"))?;
            let status = client_b
                .import_session_record_blob(&id, &record_ref, &chunk)
                .map_err(|error| format!("import record at {offset}: {error}"))?;
            record_bytes.extend(base64_decode(&chunk.data_base64)?);
            offset = chunk.next_offset;
            if chunk.complete {
                assert!(status.complete);
            }
        }
        let resumed_record = client_b
            .session_record_blob_status(&id, &record_ref)
            .map_err(|error| format!("record status: {error}"))?;
        assert!(resumed_record.complete);
        assert_eq!(resumed_record.reference.sha256, record_ref.sha256);
        assert_eq!(record_bytes, record.canonical_json.as_bytes());
        Ok(())
    })();
    a.stop();
    b.stop();
    let _ = std::fs::remove_dir_all(&root);
    result.expect("payload blob QA");
}

#[test]
fn diverged_heads_are_retained_and_no_original_is_lost() {
    let _serial = instance_lock();
    let root = scratch_root("diverge");
    let _root_guard = ScratchRoot(root.clone());
    let mut a = Instance::start(&root, "a");
    let mut b = Instance::start(&root, "b");
    let result = (|| -> Result<(), String> {
        let session =
            store::SessionId::parse("diverged_heads_session").map_err(|error| format!("session id: {error}"))?;
        let base = {
            let mut writer = store::BlockingWriter::open(a.paths(), session.clone())
                .map_err(|error| format!("seed writer: {error}"))?;
            let root_event = store::EventDraft::new(
                session.clone(),
                store::EventId::new_v4(),
                None,
                store::OriginId::new_v4(),
                store::SessionMetadata::default().created_at,
                store::EventKind::new(store::KnownEventKind::SessionCreated),
                json!(store::SessionCreatedPayload {
                    branch_root: false,
                    reset_provenance: None,
                    metadata: store::SessionMetadata::default(),
                }),
            )
            .seal()
            .map_err(|error| format!("seal root: {error}"))?;
            writer
                .append(&root_event)
                .map_err(|error| format!("append root: {error}"))?;
            let base = store::EventDraft::new(
                session.clone(),
                store::EventId::new_v4(),
                Some(root_event.envelope().event_id.clone()),
                store::OriginId::new_v4(),
                store::SessionMetadata::default().created_at,
                store::EventKind::new(store::KnownEventKind::MessageAppended),
                json!(text_message("base", "user", "shared base message")),
            )
            .seal()
            .map_err(|error| format!("seal base: {error}"))?;
            writer.append(&base).map_err(|error| format!("append base: {error}"))?;
            base.envelope().event_id.clone()
        };
        let id = session.as_str().to_owned();
        let client_a = a.client();
        let client_b = b.client();

        let seeded = client_a
            .export_session_events(&id, Vec::new(), Vec::new(), None)
            .map_err(|error| format!("export seed: {error}"))?;
        assert_eq!(seeded.heads, vec![base.as_str().to_owned()]);
        client_b
            .import_session_events(&seeded, None)
            .map_err(|error| format!("import seed: {error}"))?;

        for (paths, label, text) in [
            (a.paths(), "branch-a", "divergent branch A"),
            (b.paths(), "branch-b", "divergent branch B"),
        ] {
            let mut writer = store::BlockingWriter::open(paths, session.clone())
                .map_err(|error| format!("branch writer {label}: {error}"))?;
            let event = store::EventDraft::new(
                session.clone(),
                store::EventId::new_v4(),
                Some(base.clone()),
                store::OriginId::new_v4(),
                store::SessionMetadata::default().created_at,
                store::EventKind::new(store::KnownEventKind::MessageAppended),
                json!(text_message(label, "user", text)),
            )
            .seal()
            .map_err(|error| format!("seal {label}: {error}"))?;
            writer
                .append(&event)
                .map_err(|error| format!("append {label}: {error}"))?;
        }

        let from_b = client_b
            .export_session_events(&id, Vec::new(), Vec::new(), None)
            .map_err(|error| format!("export branch b: {error}"))?;
        match client_a.import_session_events(&from_b, None) {
            Ok(result) => assert!(
                result.conflict_heads.is_empty(),
                "a clean accept reports no retained conflict: {result:?}"
            ),
            Err(error) => {
                let details = error
                    .details
                    .clone()
                    .ok_or_else(|| format!("typed detail: {error:?}"))?;
                assert_eq!(
                    details.store_code, "conflict_heads",
                    "a diverged import names the retained heads: {details:?}"
                );
                assert!(!details.retryable);
            }
        }

        let heads = client_a
            .list_session_heads(&id)
            .map_err(|error| format!("heads: {error}"))?
            .heads;
        assert_eq!(
            heads.len(),
            2,
            "both divergent heads are retained over the socket: {heads:?}"
        );
        let mut branches = Vec::new();
        for head in &heads {
            let page = client_a
                .get_history_page(&id, Some(head), None, Some(10))
                .map_err(|error| format!("history for {head}: {error}"))?;
            let joined = page
                .messages
                .iter()
                .map(|message| message.content.clone())
                .collect::<Vec<_>>()
                .join("|");
            assert!(
                joined.contains("shared base message"),
                "head {head} lost the shared base: {joined}"
            );
            branches.push(joined);
        }
        assert!(
            branches.iter().any(|text| text.contains("divergent branch A")),
            "branch A survives: {branches:?}"
        );
        assert!(
            branches.iter().any(|text| text.contains("divergent branch B")),
            "branch B is imported beside it: {branches:?}"
        );

        let from_a = client_a
            .export_session_events(&id, Vec::new(), Vec::new(), None)
            .map_err(|error| format!("export branch a: {error}"))?;
        let _ = client_b.import_session_events(&from_a, None);
        let back = client_a
            .export_session_events(&id, Vec::new(), Vec::new(), None)
            .map_err(|error| format!("re-export: {error}"))?;
        assert!(
            back.events.len() >= from_a.events.len(),
            "every accepted record stays in the source store"
        );
        Ok(())
    })();

    if let Err(error) = &result {
        eprintln!("diverged-head replication QA failed: {error}");
    }
    a.stop();
    b.stop();
    let _ = std::fs::remove_dir_all(&root);
    result.expect("diverged-head replication QA");
}

#[test]
fn typescript_sdk_drives_the_same_sockets() {
    let Ok(_) = std::env::var("HORON_AGENT_TS_QA") else {
        eprintln!("HORON_AGENT_TS_QA is not set; skipping the TypeScript leg");
        return;
    };
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("packages/sdk/typescript");
    let entry = sdk.join("dist/index.js");
    assert!(
        entry.is_file(),
        "build packages/sdk/typescript before running the TS leg"
    );
    let _serial = instance_lock();
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/replication_ts_client.mjs");
    let root = scratch_root("ts");
    let _root_guard = ScratchRoot(root.clone());
    let mut a = Instance::start(&root, "a");
    let mut b = Instance::start(&root, "b");
    let output = Command::new("node")
        .arg(&script)
        .env("A_API", &a.api_socket)
        .env("B_API", &b.api_socket)
        .env("TS_SDK", &sdk)
        .output()
        .expect("run the TypeScript QA client");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    a.stop();
    b.stop();
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        output.status.success(),
        "TypeScript QA failed:\nstdout: {stdout}\nstderr: {stderr}"
    );
    let report: Value = serde_json::from_str(stdout.trim()).expect("JSON report");
    assert_eq!(report["ok"], true, "{report}");
    assert_eq!(report["canonical_preserved"], true, "{report}");
    assert_eq!(report["duplicate_idempotent"], true, "{report}");
    assert_eq!(report["history_matched"], true, "{report}");
}

fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let raw = text.trim_end_matches('=');
    let mut out = Vec::new();
    let mut accumulator = 0u32;
    let mut bits = 0u32;
    for byte in raw.bytes() {
        let value = ALPHABET
            .iter()
            .position(|candidate| *candidate == byte)
            .ok_or_else(|| format!("invalid base64 byte {byte}"))? as u32;
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((accumulator >> bits) & 0xff) as u8);
            accumulator &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

fn oversized_frame_refusal(api_socket: &Path) -> Value {
    let mut stream = UnixStream::connect(api_socket).expect("connect");
    let hello =
        b"{\"v\":1,\"id\":1,\"req\":\"hello\",\"min_version\":1,\"max_version\":1,\"client\":\"oversize-probe/0.1\"}\n";
    stream.write_all(hello).expect("write hello");
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("hello reply");

    // 17 MiB of escaped payload: above the 16 MiB API envelope, below the
    // bridge's 64 MiB transport frame cap.
    let payload = "x".repeat(17 * 1024 * 1024);
    let frame = format!(
        "{{\"v\":1,\"id\":2,\"req\":\"import_session_events\",\"session_id\":\"oversize_probe\",\"events\":[{{\"event_id\":\"11111111-1111-4111-8111-111111111111\",\"content_hash\":\"{}\",\"canonical_json\":\"{}\"}}]}}\n",
        "a".repeat(64),
        payload
    );
    stream.write_all(frame.as_bytes()).expect("write oversized frame");
    loop {
        line.clear();
        reader.read_line(&mut line).expect("oversize reply");
        let reply: Value = serde_json::from_str(&line).expect("reply json");
        if reply["reply_to"] == 2 {
            return reply;
        }
    }
}

fn raw_old_client(api_socket: &Path) -> Value {
    let mut stream = UnixStream::connect(api_socket).expect("connect");
    let hello =
        b"{\"v\":1,\"id\":1,\"req\":\"hello\",\"min_version\":1,\"max_version\":1,\"client\":\"old-client/0.1\"}\n";
    stream.write_all(hello).expect("write hello");
    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
    let mut line = String::new();
    reader.read_line(&mut line).expect("hello reply");
    let hello: Value = serde_json::from_str(&line).expect("hello json");
    stream
        .write_all(b"{\"v\":1,\"id\":2,\"req\":\"list_sessions\"}\n")
        .expect("write list");
    loop {
        line.clear();
        reader.read_line(&mut line).expect("list reply");
        let frame: Value = serde_json::from_str(&line).expect("list json");
        if frame["reply_to"] == 2 {
            assert_eq!(frame["ev"], "sessions");
            break;
        }
    }
    hello
}
