use super::*;
use crate::message::{Message, ToolDefinition};
use crate::provider::EventStream;
use anyhow::Result;
use async_trait::async_trait;
use horon_agent_provider_core::judge::{JudgeChoiceOption, JudgeItem, JudgeQuestion};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A provider that answers every simple completion from one script, so a
/// judgment attempt is exercised without a network.
///
/// `answers` is consumed in order and repeated once exhausted, `delay` lets a
/// timeout test outlast a short budget, and `refuse_model` makes `set_model`
/// fail the way a provider without model switching does.
struct ScriptedJudgeProvider {
    answers: Vec<Result<String, String>>,
    calls: Arc<AtomicUsize>,
    prompts: Arc<Mutex<Vec<(String, String)>>>,
    delay: Option<Duration>,
    refuse_model: bool,
    model: Arc<Mutex<String>>,
    unavailable: Option<String>,
}

struct ScriptedJudge {
    provider: Arc<ScriptedJudgeProvider>,
}

impl ScriptedJudge {
    fn new(answers: Vec<Result<String, String>>) -> Self {
        Self {
            provider: Arc::new(ScriptedJudgeProvider {
                answers,
                calls: Arc::new(AtomicUsize::new(0)),
                prompts: Arc::new(Mutex::new(Vec::new())),
                delay: None,
                refuse_model: false,
                model: Arc::new(Mutex::new("session-model".to_string())),
                unavailable: None,
            }),
        }
    }

    fn with_delay(mut self, delay: Duration) -> Self {
        Arc::get_mut(&mut self.provider).unwrap().delay = Some(delay);
        self
    }

    fn refusing_model(mut self) -> Self {
        Arc::get_mut(&mut self.provider).unwrap().refuse_model = true;
        self
    }

    fn unavailable(mut self, reason: &str) -> Self {
        Arc::get_mut(&mut self.provider).unwrap().unavailable = Some(reason.to_string());
        self
    }

    fn handle(&self) -> Arc<dyn Provider> {
        self.provider.clone()
    }

    fn calls(&self) -> usize {
        self.provider.calls.load(Ordering::SeqCst)
    }

    fn selected_model(&self) -> String {
        self.provider.model.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for ScriptedJudgeProvider {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _system: &str,
        _resume_session_id: Option<&str>,
    ) -> Result<EventStream> {
        Ok(Box::pin(futures::stream::empty()))
    }

    fn name(&self) -> &str {
        "scripted-judge"
    }

    fn model(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    fn set_model(&self, model: &str) -> Result<()> {
        if self.refuse_model {
            return Err(anyhow::anyhow!("this provider cannot switch models"));
        }
        *self.model.lock().unwrap() = model.to_string();
        Ok(())
    }

    fn unavailable_reason(&self) -> Option<String> {
        self.unavailable.clone()
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(Self {
            answers: self.answers.clone(),
            calls: Arc::clone(&self.calls),
            prompts: Arc::clone(&self.prompts),
            delay: self.delay,
            refuse_model: self.refuse_model,
            model: Arc::clone(&self.model),
            unavailable: self.unavailable.clone(),
        })
    }

    async fn complete_simple_with_usage(
        &self,
        prompt: &str,
        system: &str,
    ) -> Result<(String, horon_agent_provider_core::SimpleCompletionUsage)> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.prompts
            .lock()
            .unwrap()
            .push((prompt.to_string(), system.to_string()));
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }
        let answer = match self.answers.get(index).or_else(|| self.answers.last()) {
            Some(Ok(text)) => text.clone(),
            Some(Err(error)) => return Err(anyhow::anyhow!("{error}")),
            None => return Err(anyhow::anyhow!("the script is empty")),
        };
        Ok((
            answer,
            horon_agent_provider_core::SimpleCompletionUsage {
                input_tokens: Some(11),
                output_tokens: Some(2),
                ..Default::default()
            },
        ))
    }
}

#[tokio::test]
async fn a_zero_budget_is_the_default_rather_than_no_deadline() {
    // A configured `0` must not mean "wait forever": a judgment blocks the turn
    // it is judging, so the attempt stays bounded.
    let config = AutoJudgeConfig {
        attempt_timeout_ms: 0,
        ..AutoJudgeConfig::default()
    };
    assert_eq!(
        config.attempt_timeout_ms(),
        horon_agent_config_types::DEFAULT_AUTOJUDGE_ATTEMPT_TIMEOUT_MS
    );

    let clamped = AutoJudgeConfig {
        attempt_timeout_ms: 10 * horon_agent_config_types::MAX_AUTOJUDGE_ATTEMPT_TIMEOUT_MS,
        ..AutoJudgeConfig::default()
    };
    assert_eq!(
        clamped.attempt_timeout_ms(),
        horon_agent_config_types::MAX_AUTOJUDGE_ATTEMPT_TIMEOUT_MS
    );

    let asked = AutoJudgeConfig {
        attempt_timeout_ms: 750,
        ..AutoJudgeConfig::default()
    };
    assert_eq!(asked.attempt_timeout_ms(), 750, "an explicit budget is kept as asked");
}

fn bool_request(id: &str) -> JudgeRequest {
    JudgeRequest::new(
        serde_json::json!("the assistant stopped after 'I will now write the file.'"),
        vec![JudgeItem::new(
            id,
            JudgeQuestion::boolean("Did the assistant stop before finishing the work it announced?"),
        )],
    )
    .expect("the request is inside the contract")
}

fn bool_reply(id: &str, probability: f32) -> String {
    serde_json::json!({"answers": [{"id": id, "probability": probability}]}).to_string()
}

#[tokio::test]
async fn an_attempt_parses_a_bool_answer_and_keeps_usage() {
    let judge = ScriptedJudge::new(vec![Ok(bool_reply("stopped", 0.91))]);
    let outcome = judge_with(&judge.handle(), "judge-model", 3000, &bool_request("stopped"))
        .await
        .expect("the scripted answer satisfies the contract");

    assert_eq!(outcome.answers.probability("stopped"), Some(0.91));
    assert_eq!(outcome.usage.input_tokens, Some(11));
    assert_eq!(outcome.usage.output_tokens, Some(2));
    assert_eq!(judge.selected_model(), "judge-model");
    assert_eq!(judge.calls(), 1);
}

#[tokio::test]
async fn a_choice_and_a_score_answer_parse_together() {
    let judge = ScriptedJudge::new(vec![Ok(serde_json::json!({
        "answers": [
            {"id": "kind", "label": "truncated"},
            {"id": "severity", "level": 2}
        ]
    })
    .to_string())]);
    let request = JudgeRequest::new(
        serde_json::json!("an answer that stops mid-sentence"),
        vec![
            JudgeItem::new(
                "kind",
                JudgeQuestion::choice(
                    "Why did the turn stop?",
                    vec![
                        JudgeChoiceOption::new("complete", "the work the user asked for is done"),
                        JudgeChoiceOption::new("truncated", "the text ends mid-sentence"),
                    ],
                ),
            ),
            JudgeItem::new(
                "severity",
                JudgeQuestion::score("How much work is left?", vec!["none".into(), "some".into()]),
            ),
        ],
    )
    .expect("the request is inside the contract");

    let outcome = judge_with(&judge.handle(), "judge-model", 3000, &request)
        .await
        .expect("both answers satisfy the contract");
    assert_eq!(outcome.answers.label("kind"), Some("truncated"));
    assert_eq!(outcome.answers.level("severity"), Some(2));
}

#[tokio::test]
async fn a_malformed_reply_is_not_repaired() {
    let judge = ScriptedJudge::new(vec![Ok("I think it probably is not finished.".to_string())]);
    let error = judge_with(&judge.handle(), "judge-model", 3000, &bool_request("stopped"))
        .await
        .expect_err("prose is not the documented shape");
    assert!(matches!(error, JudgeError::Malformed(_)), "{error}");
}

#[tokio::test]
async fn a_reply_that_invents_a_probability_field_is_rejected() {
    let judge = ScriptedJudge::new(vec![Ok(serde_json::json!({
        "answers": [{"id": "stopped", "probability": 0.4, "label": "yes"}]
    })
    .to_string())]);
    let error = judge_with(&judge.handle(), "judge-model", 3000, &bool_request("stopped"))
        .await
        .expect_err("an extra field is outside the contract");
    assert!(matches!(error, JudgeError::InvalidAnswer { .. }), "{error}");
}

#[tokio::test(flavor = "current_thread")]
async fn an_attempt_that_outlasts_its_budget_times_out() {
    let judge = ScriptedJudge::new(vec![Ok(bool_reply("stopped", 0.2))]).with_delay(Duration::from_millis(120));
    let error = judge_with(&judge.handle(), "judge-model", 20, &bool_request("stopped"))
        .await
        .expect_err("the delayed answer arrives after the budget");
    assert!(matches!(error, JudgeError::Timeout { after_ms: 20 }), "{error}");
}

#[tokio::test]
async fn a_backend_failure_is_reported_as_one() {
    let judge = ScriptedJudge::new(vec![Err("502 from the upstream".to_string())]);
    let error = judge_with(&judge.handle(), "judge-model", 3000, &bool_request("stopped"))
        .await
        .expect_err("the backend refused the call");
    assert!(matches!(error, JudgeError::Backend(_)), "{error}");
    assert!(horon_agent_provider_core::judge::is_retryable(&error));
}

#[tokio::test]
async fn a_judge_model_that_cannot_be_selected_refuses_instead_of_inheriting_the_session_model() {
    let judge = ScriptedJudge::new(vec![Ok(bool_reply("stopped", 0.9))]).refusing_model();
    let error = judge_with(&judge.handle(), "judge-model", 3000, &bool_request("stopped"))
        .await
        .expect_err("a judgment must run on the model it names");
    assert!(matches!(error, JudgeError::Backend(_)), "{error}");
    assert_eq!(judge.calls(), 0, "no request is spent on a model that was not selected");
}

#[tokio::test]
async fn a_provider_without_a_backend_is_unavailable_before_any_request() {
    let judge = ScriptedJudge::new(vec![Ok(bool_reply("stopped", 0.9))]).unavailable("no credential");
    let error = judge_with(&judge.handle(), "judge-model", 3000, &bool_request("stopped"))
        .await
        .expect_err("an unconfigured provider cannot judge");
    assert!(matches!(error, JudgeError::Unavailable(_)), "{error}");
    assert_eq!(judge.calls(), 0);
}

#[test]
fn a_request_outside_the_contract_fails_before_the_model_is_selected() {
    let request = JudgeRequest {
        state: serde_json::json!(42),
        questions: vec![JudgeItem::new("q", JudgeQuestion::boolean("is it done?"))],
    };
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let judge = ScriptedJudge::new(vec![Ok(bool_reply("q", 0.9))]);
    let error = runtime
        .block_on(judge_with(&judge.handle(), "judge-model", 3000, &request))
        .expect_err("a non-text state is outside the contract");
    assert!(matches!(error, JudgeError::Input(_)), "{error}");
    assert_eq!(judge.calls(), 0);
}

#[test]
fn the_role_chain_reads_the_judge_entry_then_aux_then_default() {
    let roles = ModelRoles {
        judge: Some("judge-model".to_string()),
        aux: Some("aux-model".to_string()),
        default: Some("default-model".to_string()),
        ..ModelRoles::default()
    };
    assert_eq!(
        resolve_judge_model(None, &roles, Some("profile"), Some("provider")).unwrap(),
        "judge-model"
    );

    let without_judge = ModelRoles {
        judge: None,
        ..roles.clone()
    };
    assert_eq!(
        resolve_judge_model(None, &without_judge, Some("profile"), Some("provider")).unwrap(),
        "aux-model"
    );

    let only_default = ModelRoles {
        judge: None,
        aux: None,
        ..roles
    };
    assert_eq!(
        resolve_judge_model(None, &only_default, Some("profile"), Some("provider")).unwrap(),
        "default-model"
    );
}

#[test]
fn the_run_override_wins_over_the_role_entry() {
    let roles = ModelRoles {
        judge: Some("judge-model".to_string()),
        ..ModelRoles::default()
    };
    assert_eq!(
        resolve_judge_model(Some("override-model"), &roles, None, None).unwrap(),
        "override-model"
    );
}

#[test]
fn a_prefixed_judge_entry_is_refused_with_the_role_named() {
    let roles = ModelRoles {
        judge: Some("openai:gpt-5".to_string()),
        ..ModelRoles::default()
    };
    let error = resolve_judge_model(None, &roles, None, None).expect_err("a route prefix must not be inherited");
    assert_eq!(
        error,
        RoleResolutionError::RoutePrefix {
            role: ModelRole::Judge,
            value: "openai:gpt-5".to_string(),
            prefix: "openai".to_string(),
        }
    );
    assert!(error.to_string().contains("`judge`"), "{error}");
}

#[test]
fn an_empty_chain_names_every_step_it_tried() {
    let error = resolve_judge_model(None, &ModelRoles::default(), None, None).expect_err("nothing resolved");
    let message = error.to_string();
    for expected in ["roles.judge", "roles.aux", "roles.default", "default_model"] {
        assert!(message.contains(expected), "{message}");
    }
}

#[test]
fn the_state_carries_both_sides_and_stays_inside_the_contract() {
    let long_user = "가".repeat(MAX_STATE_BYTES);
    let long_text = "나".repeat(MAX_STATE_BYTES);
    let state = turn_recovery_state(&long_user, &long_text);
    let text = state.as_str().expect("the state is text");
    assert!(text.starts_with("## Last user message\n"));
    assert!(text.contains("## Assistant text at the stop"));
    assert!(text.len() <= MAX_STATE_BYTES, "the state is {} bytes", text.len());

    let request = JudgeRequest::new(state, vec![JudgeItem::new("q", JudgeQuestion::boolean("premature?"))])
        .expect("the clipped state satisfies the contract");
    request.state_text().expect("the clipped state is inside the byte cap");
}

#[test]
fn clipping_never_splits_a_character() {
    // Two 3-byte characters plus a 3-byte ellipsis need 9 bytes; a 4-byte
    // budget holds the ellipsis alone rather than half a character.
    assert_eq!(clip_to_bytes("가나다라", 4), "…");
    assert_eq!(clip_to_bytes("가나다라", 9), "가나…");
    assert_eq!(clip_to_bytes("short", 64), "short");
}

// ---------------------------------------------------------------------------
// Loopback HTTP mock: the acceptance wording asks for a mock-server test, so one
// test drives the real OpenAI-compatible provider through a real socket.

struct MockJudgeServer {
    base_url: String,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicUsize>,
    accept: Option<std::thread::JoinHandle<()>>,
}

fn sse_body(payload: &str) -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::json!({
            "choices": [{"delta": {"content": payload}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 31, "completion_tokens": 7},
        })
    )
}

impl MockJudgeServer {
    fn start(payload: &str) -> Self {
        Self::start_with(payload, false)
    }

    /// A server that accepts the request and then never answers, which is the
    /// failure a deadline exists for: the connection is open, so no transport
    /// error arrives to end the attempt.
    fn stalled() -> Self {
        Self::start_with("", true)
    }

    fn start_with(payload: &str, stall: bool) -> Self {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback listener");
        listener.set_nonblocking(true).expect("non-blocking listener");
        let address = listener.local_addr().expect("loopback address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicUsize::new(0));
        let seen_thread = Arc::clone(&seen);
        let stop_thread = Arc::clone(&stop);
        let body = sse_body(payload);
        let accept = std::thread::spawn(move || {
            while stop_thread.load(Ordering::SeqCst) == 0 {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).ok();
                        stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();
                        let mut buffer = Vec::new();
                        let mut chunk = [0u8; 2048];
                        let mut content_length = 0usize;
                        loop {
                            let read = match stream.read(&mut chunk) {
                                Ok(0) | Err(_) => break,
                                Ok(read) => read,
                            };
                            buffer.extend_from_slice(&chunk[..read]);
                            let text = String::from_utf8_lossy(&buffer).to_string();
                            if let Some(end) = text.find("\r\n\r\n") {
                                if content_length == 0 {
                                    content_length = text[..end]
                                        .lines()
                                        .filter_map(|line| line.split_once(':'))
                                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                                        .and_then(|(_, value)| value.trim().parse().ok())
                                        .unwrap_or(0);
                                }
                                if buffer.len() >= end + 4 + content_length {
                                    break;
                                }
                            }
                        }
                        if let Ok(text) = String::from_utf8(buffer) {
                            seen_thread.lock().unwrap().push(text);
                        }
                        if stall {
                            // Hold the connection without answering until the
                            // client gives up, so the test measures the
                            // client's own deadline rather than a socket error.
                            std::thread::sleep(std::time::Duration::from_secs(5));
                            continue;
                        }
                        let head = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\
                             Content-Length: {}\r\n\r\n",
                            body.len()
                        );
                        let _ = stream.write_all(head.as_bytes());
                        let _ = stream.write_all(body.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
                }
            }
        });
        Self {
            base_url: format!("http://{address}/v1"),
            seen,
            stop,
            accept: Some(accept),
        }
    }

    fn requests(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    fn provider(&self) -> Arc<dyn Provider> {
        Arc::new(
            horon_agent_provider_openai::OpenAiCompatibleProvider::new(
                self.base_url.clone(),
                horon_agent_provider_openai::ChatAuth::None,
                None,
                "session-model",
                vec![],
            )
            .expect("the loopback profile is valid"),
        )
    }
}

impl Drop for MockJudgeServer {
    fn drop(&mut self) {
        self.stop.store(1, Ordering::SeqCst);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

#[tokio::test]
async fn a_judgment_over_a_real_socket_parses_and_reports_usage() {
    let server = MockJudgeServer::start(&bool_reply("stopped", 0.73));
    let provider = server.provider();

    let outcome = judge_with(&provider, "judge-model", 5000, &bool_request("stopped"))
        .await
        .expect("the mock server answers inside the contract");

    assert_eq!(outcome.answers.probability("stopped"), Some(0.73));
    assert_eq!(outcome.usage.input_tokens, Some(31));
    assert_eq!(outcome.usage.output_tokens, Some(7));
    assert_eq!(outcome.model, "judge-model");

    let requests = server.requests();
    assert_eq!(requests.len(), 1, "one attempt is one request");
    let request = &requests[0];
    assert!(request.starts_with("POST /v1/chat/completions "), "{request}");
    assert!(
        request.contains("\"model\":\"judge-model\""),
        "the judge model is the one the role resolved: {request}"
    );
    assert!(
        request.contains("## Questions"),
        "the question block travelled with the request: {request}"
    );
    assert!(
        request.contains("### stopped"),
        "the question id travelled with the request: {request}"
    );
}

#[tokio::test]
async fn a_server_error_is_a_typed_backend_failure() {
    // A port nobody listens on refuses the connection, which is the same class
    // of failure a 5xx produces: the attempt was made and the backend failed.
    let provider: Arc<dyn Provider> = Arc::new(
        horon_agent_provider_openai::OpenAiCompatibleProvider::new(
            "http://127.0.0.1:9/v1".to_string(),
            horon_agent_provider_openai::ChatAuth::None,
            None,
            "session-model",
            vec![],
        )
        .expect("the loopback profile is valid"),
    );
    let error = judge_with(&provider, "judge-model", 5000, &bool_request("stopped"))
        .await
        .expect_err("nothing answers on the discard port");
    assert!(matches!(error, JudgeError::Backend(_)), "{error}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_server_that_never_answers_hits_the_attempt_deadline() {
    let server = MockJudgeServer::stalled();
    let provider = server.provider();

    let started = std::time::Instant::now();
    let error = judge_with(&provider, "judge-model", 150, &bool_request("stopped"))
        .await
        .expect_err("a connection that never answers cannot produce an answer");
    let waited = started.elapsed();

    assert!(matches!(error, JudgeError::Timeout { after_ms: 150 }), "{error}");
    assert!(
        waited < std::time::Duration::from_secs(3),
        "the deadline ends the attempt instead of the transport timeout: {waited:?}"
    );
}

#[tokio::test]
async fn selecting_a_judge_model_leaves_the_callers_provider_alone() {
    let server = MockJudgeServer::start(&bool_reply("stopped", 0.8));
    let provider = server.provider();
    assert_eq!(provider.model(), "session-model");

    judge_with(&provider, "judge-model", 5000, &bool_request("stopped"))
        .await
        .expect("the mock server answers inside the contract");

    assert_eq!(
        provider.model(),
        "session-model",
        "the judgment is forked, so the session keeps its own model"
    );
}
