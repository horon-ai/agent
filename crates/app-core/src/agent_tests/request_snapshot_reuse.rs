use super::*;

use crate::message::{ContentBlock, Message, Role, StreamEvent, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc as tokio_mpsc;

#[derive(Default)]
struct PayloadRecordingProvider {
    payloads: Arc<std::sync::Mutex<Vec<Vec<Message>>>>,
    fail: Arc<AtomicBool>,
}

impl PayloadRecordingProvider {
    fn payloads(&self) -> Vec<Vec<Message>> {
        self.payloads.lock().unwrap().clone()
    }

    fn fail_next(&self) {
        self.fail.store(true, Ordering::SeqCst);
    }

    fn response(&self) -> Result<EventStream> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(anyhow::anyhow!("injected provider failure"));
        }
        Ok(Box::pin(futures::stream::iter([
            Ok(StreamEvent::TextDelta("done".into())),
            Ok(StreamEvent::MessageEnd {
                stop_reason: Some("end_turn".into()),
            }),
        ])))
    }
}

#[async_trait]
impl Provider for PayloadRecordingProvider {
    async fn complete(
        &self,
        messages: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        self.payloads.lock().unwrap().push(messages.to_vec());
        self.response()
    }

    /// Overridden instead of inherited: the inherited default folds the dynamic
    /// system prompt into the message list first, so the recorded payload would
    /// not be the request snapshot this test measures.
    async fn complete_split(
        &self,
        messages: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: &str,
        _: Option<&str>,
    ) -> Result<EventStream> {
        self.payloads.lock().unwrap().push(messages.to_vec());
        self.response()
    }

    fn name(&self) -> &str {
        "payload-recording-test"
    }

    fn supports_compaction(&self) -> bool {
        false
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(PayloadRecordingProvider {
            payloads: Arc::clone(&self.payloads),
            fail: Arc::clone(&self.fail),
        })
    }
}

const MEMORY_FACT: &str = "snapshot reuse fixture";
const CREDENTIAL: &str = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123";

fn user_text(text: &str) -> (Role, Vec<ContentBlock>) {
    (
        Role::User,
        vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    )
}

fn assistant_text(text: &str) -> (Role, Vec<ContentBlock>) {
    (
        Role::Assistant,
        vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    )
}

fn seed_session(working_dir: Option<String>) -> crate::session::Session {
    let mut session = crate::session::Session::create(None, None);
    session.working_dir = working_dir;
    for index in 0..6 {
        let (role, content) = user_text(&format!("user turn {index}"));
        session.add_message(role, content);
        let (role, content) = assistant_text(&format!("assistant reply {index}"));
        session.add_message(role, content);
    }
    let (role, content) = assistant_text(&format!("the credential is {CREDENTIAL}"));
    session.add_message(role, content);
    let (role, content) = user_text("final question");
    session.add_message(role, content);
    session
}

/// Serialized payload, message by message. Messages minted during request
/// preparation carry `Utc::now()`, so their creation time is dropped before
/// comparison; every transcript byte, including its own timestamps, is compared
/// exactly.
fn serialized_payload(messages: &[Message], transcript_len: usize) -> Vec<String> {
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let mut value = serde_json::to_value(message).unwrap_or(serde_json::Value::Null);
            if index >= transcript_len
                && let serde_json::Value::Object(map) = &mut value
            {
                map.remove("timestamp");
            }
            serde_json::to_string(&value).unwrap_or_default()
        })
        .collect()
}

/// The pre-change request construction: copy the whole transcript, append the
/// suffix, stamp, then mask. The new path must reproduce this payload exactly.
fn baseline_payload(
    transcript: &[Message],
    suffix: &[Message],
    timestamps: bool,
    mask: bool,
    session_id: &str,
) -> Vec<Message> {
    let mut with_suffix: Vec<Message> = transcript.to_vec();
    with_suffix.extend(suffix.iter().cloned());
    let stamped = timestamps.then(|| Message::with_timestamps(&with_suffix));
    let send = stamped.as_deref().unwrap_or(&with_suffix);
    if !mask {
        return send.to_vec();
    }
    crate::secrets::outbound_messages(send, session_id, crate::config::SecretsMode::Obfuscate)
        .expect("masking must not refuse this fixture")
        .unwrap_or_else(|| send.to_vec())
}

struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvRestore {
    fn apply(pairs: &[(&'static str, &str)]) -> Self {
        let saved = pairs
            .iter()
            .map(|(key, value)| {
                let previous = std::env::var_os(key);
                crate::env::set_var(key, value);
                (*key, previous)
            })
            .collect::<Vec<_>>();
        crate::config::invalidate_config_cache();
        Self(saved)
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, previous) in self.0.drain(..) {
            match previous {
                Some(value) => crate::env::set_var(key, value),
                None => crate::env::remove_var(key),
            }
        }
        crate::config::invalidate_config_cache();
    }
}

fn write_config(contents: &str) {
    let path = crate::config::Config::path().expect("config path");
    std::fs::create_dir_all(path.parent().expect("config parent")).expect("config dir");
    std::fs::write(&path, contents).expect("write config");
    crate::config::invalidate_config_cache();
}

struct Case {
    name: &'static str,
    timestamps: bool,
    mask: bool,
    memory: bool,
}

const CASES: &[Case] = &[
    Case {
        name: "plain",
        timestamps: false,
        mask: false,
        memory: false,
    },
    Case {
        name: "memory",
        timestamps: false,
        mask: false,
        memory: true,
    },
    Case {
        name: "timestamps",
        timestamps: true,
        mask: false,
        memory: false,
    },
    Case {
        name: "masking",
        timestamps: false,
        mask: true,
        memory: false,
    },
];

struct Prepared {
    agent: Agent,
    provider: Arc<PayloadRecordingProvider>,
    expected: Vec<Message>,
    transcript: Vec<Message>,
    transcript_len: usize,
    project: Option<std::path::PathBuf>,
    _env: EnvRestore,
    _home: tempfile::TempDir,
}

impl Prepared {
    fn payload_texts(messages: &[Message]) -> Vec<String> {
        messages
            .iter()
            .map(|message| content_text(&message.content).to_string())
            .collect()
    }
}

/// The two turn paths run against separate sessions, so their own message
/// timestamps differ. With timestamps on, the rendered prefix carries that
/// difference and is stripped before the contents are compared.
fn normalized_payload_texts(case: &Case, messages: &[Message]) -> Vec<String> {
    Prepared::payload_texts(messages)
        .into_iter()
        .map(|text| {
            if !case.timestamps {
                return text;
            }
            match (text.find("] "), text.starts_with('[')) {
                (Some(end), true) => text[end + 2..].to_string(),
                _ => text,
            }
        })
        .collect()
}

fn prepare(case: &Case) -> Prepared {
    prepare_with(case, case.mask)
}

fn prepare_with(case: &Case, mask: bool) -> Prepared {
    write_config(if mask {
        "[tools]\nsecrets_mode = \"obfuscate\"\n"
    } else {
        ""
    });
    let env = EnvRestore::apply(&[(
        "HORON_AGENT_MESSAGE_TIMESTAMPS",
        if case.timestamps { "true" } else { "false" },
    )]);
    crate::memory::clear_all_pending_memory();

    let home = tempfile::tempdir().expect("isolated memory home");
    let project = home.path().join("project");
    let working_dir = if case.memory {
        std::fs::create_dir_all(&project).expect("isolated project");
        Some(project.to_string_lossy().into_owned())
    } else {
        None
    };

    let provider = Arc::new(PayloadRecordingProvider::default());
    let dynamic: Arc<dyn Provider> = provider.clone();
    let session = seed_session(working_dir);
    let mut agent = Agent::new_with_session(dynamic, Registry::empty(), session, None);
    agent.memory_enabled = case.memory;

    let transcript = agent.session.messages_for_provider_uncached();
    let transcript_len = transcript.len();

    let mut suffix: Vec<Message> = Vec::new();
    if case.memory {
        let entry = crate::memory::MemoryEntry::new(crate::memory::MemoryCategory::Fact, MEMORY_FACT);
        crate::memory::MemoryManager::new()
            .with_project_dir(&project)
            .remember_project(entry.clone())
            .expect("persist the selected memory for scoped revalidation");
        let prompt =
            crate::memory::format_relevant_prompt(std::slice::from_ref(&entry), 1).expect("canonical memory prompt");
        crate::memory::set_pending_memory(
            &agent.session.id,
            prompt.clone(),
            1,
            vec![entry.id.clone()],
            None,
            agent.session.working_dir.as_deref(),
        );
        suffix.push(Message::user(&format!(
            "<system-reminder>\n{prompt}\n</system-reminder>"
        )));
    }

    let expected = baseline_payload(&transcript, &suffix, case.timestamps, mask, &agent.session.id);

    Prepared {
        agent,
        provider,
        expected,
        transcript,
        transcript_len,
        project: case.memory.then_some(project),
        _env: env,
        _home: home,
    }
}

fn transcript_cache_fields(event: &ServerEvent) -> (u64, Vec<u64>, usize, usize) {
    match event {
        ServerEvent::KvCacheRequest {
            messages_hash,
            message_hashes,
            message_count,
            messages_json_chars,
            ..
        } => (
            *messages_hash,
            message_hashes.clone(),
            *message_count,
            *messages_json_chars,
        ),
        other => panic!("expected a cache request event, got {other:?}"),
    }
}

fn assert_case_effects(case: &Case, payload: &[Message]) {
    let rendered = payload.iter().map(|message| format!("{message:?}")).collect::<String>();
    if case.mask {
        assert!(
            !rendered.contains(CREDENTIAL),
            "{}: the credential must not leave",
            case.name
        );
        assert!(
            rendered.contains("«SECRET:github:"),
            "{}: masking must apply",
            case.name
        );
    } else {
        assert!(
            rendered.contains(CREDENTIAL),
            "{}: unmasked text must pass through",
            case.name
        );
    }
    if case.memory {
        let last = payload.last().expect("the request carries messages");
        let text = content_text(&last.content);
        assert!(
            text.starts_with("<system-reminder>\n# Memory") && text.contains(MEMORY_FACT),
            "{}: the memory message must be appended verbatim, got {text:?}",
            case.name
        );
    }
}

#[tokio::test]
async fn request_snapshot_reuse() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();

    for case in CASES {
        let Prepared {
            mut agent,
            provider,
            expected,
            transcript,
            transcript_len,
            _env,
            _home,
            ..
        } = prepare(case);
        let expected_serialized = serialized_payload(&expected, transcript_len);

        let before = crate::agent::request_extra_clone_counters();
        let (tx, mut rx) = tokio_mpsc::unbounded_channel();
        agent
            .run_turn_streaming_mpsc(tx)
            .await
            .expect("the streaming turn completes");
        let after = crate::agent::request_extra_clone_counters();
        let mut cache_event = None;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ServerEvent::KvCacheRequest { .. }) {
                cache_event = Some(event);
            }
        }

        let streaming_payloads = provider.payloads();
        assert_eq!(
            streaming_payloads.len(),
            1,
            "{}: the turn must issue exactly one provider request",
            case.name
        );
        assert_eq!(
            serialized_payload(&streaming_payloads[0], transcript_len),
            expected_serialized,
            "{}: the streaming payload must match the pre-change construction byte for byte",
            case.name
        );
        assert_case_effects(case, &streaming_payloads[0]);

        let cache_event = cache_event.unwrap_or_else(|| panic!("{}: no cache request event", case.name));
        let signature_messages = if case.timestamps {
            Message::with_timestamps(&transcript)
        } else {
            transcript.clone()
        };
        let baseline_event = kv_cache_request_event(&signature_messages, &[], "", &[]);
        assert_eq!(
            transcript_cache_fields(&cache_event),
            transcript_cache_fields(&baseline_event),
            "{}: cache signature values must match the slice-based construction",
            case.name
        );

        let extra_clones = after.0 - before.0;
        let extra_bytes = after.1 - before.1;
        let expected_extra = u64::from(case.memory || case.timestamps || case.mask);
        assert_eq!(
            extra_clones, expected_extra,
            "{}: request preparation made {extra_clones} extra full-transcript copies, expected {expected_extra}",
            case.name
        );
        std::println!(
            "QA_CLONES {} streaming extra_clones={} extra_bytes={} messages={}",
            case.name,
            extra_clones,
            extra_bytes,
            streaming_payloads[0].len()
        );
        drop(_env);
        drop(_home);

        let Prepared {
            mut agent,
            provider,
            expected,
            transcript_len,
            _env,
            _home,
            ..
        } = prepare(case);
        let expected_serialized = serialized_payload(&expected, transcript_len);
        let before = crate::agent::request_extra_clone_counters();
        agent.run_turn(false).await.expect("the non-streaming turn completes");
        let after = crate::agent::request_extra_clone_counters();
        let nonstreaming_payloads = provider.payloads();
        assert_eq!(
            nonstreaming_payloads.len(),
            1,
            "{}: the non-streaming turn must issue exactly one provider request",
            case.name
        );
        assert_eq!(
            serialized_payload(&nonstreaming_payloads[0], transcript_len),
            expected_serialized,
            "{}: the non-streaming payload must match the pre-change construction byte for byte",
            case.name
        );
        assert_case_effects(case, &nonstreaming_payloads[0]);
        assert_eq!(
            normalized_payload_texts(case, &nonstreaming_payloads[0]),
            normalized_payload_texts(case, &streaming_payloads[0]),
            "{}: both turn paths must send the same request content",
            case.name
        );
        assert_eq!(
            after.0 - before.0,
            expected_extra,
            "{}: the non-streaming path must make the same number of copies as the streaming path",
            case.name
        );
        std::println!(
            "QA_CLONES {} non_streaming extra_clones={} extra_bytes={} messages={}",
            case.name,
            after.0 - before.0,
            after.1 - before.1,
            nonstreaming_payloads[0].len()
        );
    }

    crate::memory::clear_all_pending_memory();
}

#[tokio::test]
async fn request_snapshot_reuse_holds_failures_off_the_transcript() {
    let _sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();

    for (name, block_masking, inject_provider_error) in
        [("masking-refusal", true, false), ("provider-error", false, true)]
    {
        let case = Case {
            name,
            timestamps: false,
            mask: false,
            memory: false,
        };
        let Prepared {
            mut agent,
            provider,
            transcript,
            _env,
            _home,
            ..
        } = prepare_with(&case, !block_masking);
        if block_masking {
            write_config("[tools]\nsecrets_mode = \"block\"\n");
        }
        if inject_provider_error {
            provider.fail_next();
        }
        let context_before = serde_json::to_value(agent.session.active_context()).unwrap();

        let (tx, _rx) = tokio_mpsc::unbounded_channel();
        let result = agent.run_turn_streaming_mpsc(tx).await;

        assert!(result.is_err(), "{name}: the turn must fail");
        assert_eq!(
            serde_json::to_value(agent.session.active_context()).unwrap(),
            context_before,
            "{name}: a failed request must not mutate the transcript"
        );
        assert_eq!(
            serde_json::to_value(agent.session.messages_for_provider_uncached()).unwrap(),
            serde_json::to_value(&transcript).unwrap(),
            "{name}: the provider projection must be unchanged"
        );
        std::println!(
            "QA_FAILURE {name} error={} payloads={}",
            result.unwrap_err(),
            provider.payloads().len()
        );
    }

    let case = Case {
        name: "memory-change",
        timestamps: false,
        mask: false,
        memory: true,
    };
    let Prepared {
        mut agent,
        provider,
        transcript,
        transcript_len,
        project,
        _env,
        _home,
        expected: _,
    } = prepare(&case);
    let project = project.expect("the memory case binds a project");
    let manager = crate::memory::MemoryManager::new().with_project_dir(&project);
    let memory_id = manager
        .list_all()
        .expect("list project memory")
        .into_iter()
        .next()
        .expect("the fixture stored one memory")
        .id;
    manager.forget(&memory_id).expect("forget the selected memory");

    let (tx, mut rx) = tokio_mpsc::unbounded_channel();
    agent
        .run_turn_streaming_mpsc(tx)
        .await
        .expect("a stale memory must not fail the turn");
    let mut memory_injected = false;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, ServerEvent::MemoryInjected { .. }) {
            memory_injected = true;
        }
    }
    let payloads = provider.payloads();
    assert_eq!(payloads.len(), 1, "the turn still issues one request");
    assert!(!memory_injected, "a memory removed between turns must not be injected");
    assert_eq!(
        payloads[0].len(),
        transcript_len,
        "a discarded memory must not leave a stray suffix message"
    );
    let after_turn = agent.session.messages_for_provider_uncached();
    assert_eq!(
        serde_json::to_value(&after_turn[..transcript_len]).unwrap(),
        serde_json::to_value(&transcript).unwrap(),
        "the turn must append to the transcript without rewriting it"
    );
    std::println!(
        "QA_FAILURE memory-change injected=false request_messages={} transcript_prefix_intact=true",
        payloads[0].len()
    );

    crate::memory::clear_all_pending_memory();
}
