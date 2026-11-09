//! Lightweight sidecar client for fast, cheap model calls.
//!
//! Used for memory relevance verification and other quick tasks that don't need the full Agent
//! SDK infrastructure.
//!
//! Every call is a fork of the process's active model provider, so the sidecar
//! runs on the same backend as the session and inherits its model. A process
//! without a configured provider reports the sidecar unavailable instead of
//! answering through a different provider.

use anyhow::{Context, Result};
use reqwest::StatusCode;
use std::fmt;
use std::sync::Arc;
/// Whether retrying a failed sidecar request can reasonably succeed without a
/// configuration or credential change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarErrorKind {
    Transient,
    Permanent,
}

#[derive(Debug)]
struct SidecarHttpError {
    provider: &'static str,
    status: StatusCode,
    body: String,
}

impl fmt::Display for SidecarHttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} API error ({}): {}", self.provider, self.status, self.body)
    }
}

impl std::error::Error for SidecarHttpError {}

/// Classify a sidecar failure for retry policy. HTTP client/auth/request errors
/// are permanent; throttling, server failures, and transport failures are
/// transient. Unknown provider errors retain the conservative retry behavior.
pub fn classify_error(error: &anyhow::Error) -> SidecarErrorKind {
    if let Some(error) = error.downcast_ref::<SidecarHttpError>() {
        return classify_http_status(error.status);
    }
    for cause in error.chain() {
        if let Some(error) = cause.downcast_ref::<reqwest::Error>() {
            if let Some(status) = error.status() {
                return classify_http_status(status);
            }
            return SidecarErrorKind::Transient;
        }
    }

    // Provider-backed sidecars may not expose a typed HTTP error yet.
    let message = error.to_string().to_ascii_lowercase();
    if [
        "400",
        "401",
        "403",
        "404",
        "bad request",
        "unauthorized",
        "forbidden",
        "not_found_error",
    ]
    .iter()
    .any(|needle| message.contains(needle))
    {
        SidecarErrorKind::Permanent
    } else {
        SidecarErrorKind::Transient
    }
}

fn classify_http_status(status: StatusCode) -> SidecarErrorKind {
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        SidecarErrorKind::Transient
    } else if status.is_client_error() {
        SidecarErrorKind::Permanent
    } else {
        SidecarErrorKind::Transient
    }
}

/// Which backend the sidecar runs on.
#[derive(Debug, Clone, Copy, PartialEq)]
enum SidecarBackend {
    /// Dispatch through the active model provider (`crate::provider::active_provider_fork`).
    Provider,
    /// No model provider is configured in this process; every call is refused with a reason.
    Unavailable,
}

/// Lightweight client for fast sidecar calls.
#[derive(Clone)]
pub struct Sidecar {
    model: String,
    backend: SidecarBackend,
    /// Provider snapshot selected with the model. Keeping the fork here avoids re-resolving a
    /// mutable global provider between sidecar construction and dispatch.
    provider: Option<Arc<dyn crate::provider::Provider>>,
    /// Why this process has no sidecar backend, when it has none.
    unavailable_reason: Option<String>,
}

/// The live model provider behind the active session, when one is available.
fn sidecar_provider_fork() -> Option<Arc<dyn crate::provider::Provider>> {
    crate::provider::active_provider_fork().filter(|provider| provider.unavailable_reason().is_none())
}

impl Sidecar {
    /// Create a new sidecar client on the process's active model provider.
    pub fn new() -> Self {
        let configured_model = crate::config::config().agents.memory_model.clone();
        Self::with_configured_model(configured_model)
    }

    pub fn for_session(session: &crate::session::Session) -> Self {
        let model = session.effective_agent_model("memory", crate::config::Config::load().agents.memory_model);
        if model.as_deref() == Some("inherit") {
            let Some(provider) = sidecar_provider_fork() else {
                return Self::unavailable();
            };
            if let Some(model) = session.model.as_deref()
                && let Err(error) = provider.restore_model(model)
            {
                let mut sidecar = Self::unavailable();
                sidecar.unavailable_reason = Some(format!("the inherited model could not be restored: {error}"));
                return sidecar;
            }
            return Self::provider_backed(provider);
        }
        Self::with_configured_model(model)
    }

    /// A sidecar on the active provider, with an optional model selection applied to
    /// this fork. The model name is provider-local; it is never parsed into a built-in route.
    fn with_configured_model(configured_model: Option<String>) -> Self {
        let Some(provider) = sidecar_provider_fork() else {
            return Self::unavailable();
        };
        if let Some(model) = configured_model.as_deref() {
            Self::select_model(&provider, model);
        }
        Self::provider_backed(provider)
    }

    /// Apply one model selection to a fork, warning when it is unusable instead of moving
    /// the request to another backend.
    fn select_model(provider: &Arc<dyn crate::provider::Provider>, model: &str) -> bool {
        let selected = provider.set_model(model).map_err(|error| {
            anyhow::anyhow!("the model '{model}' could not be selected on the active provider: {error}")
        });
        match selected {
            Ok(()) => true,
            Err(error) => {
                crate::logging::warn(&format!("Ignoring the memory sidecar model '{model}': {error}"));
                false
            }
        }
    }

    fn provider_backed(provider: Arc<dyn crate::provider::Provider>) -> Self {
        Self {
            model: provider.model(),
            backend: SidecarBackend::Provider,
            provider: Some(provider),
            unavailable_reason: None,
        }
    }

    /// A sidecar that refuses every call because no model provider is configured here.
    fn unavailable() -> Self {
        Self {
            model: String::new(),
            backend: SidecarBackend::Unavailable,
            provider: None,
            unavailable_reason: Some(
                "no model provider is configured in this process; add [providers.<name>] with \
                 type = \"openai-compatible\" to config.toml and select it with --provider-profile <name>"
                    .to_string(),
            ),
        }
    }

    /// Whether a usable sidecar backend exists right now: an active model provider.
    ///
    /// Re-evaluated live, so a backend composed later is seen without a restart. This is the
    /// signal the memory system uses to decide whether the LLM precision judge can run; when it
    /// is false, memory's sidecar mode is treated as unavailable rather than silently degrading.
    pub fn llm_backend_available() -> bool {
        sidecar_provider_fork().is_some()
    }

    /// Return the currently selected sidecar model name.
    pub fn model_name(&self) -> &str {
        &self.model
    }

    /// Construct a sidecar pinned to one model of the process's active provider, with an optional
    /// explicit reasoning effort. Used by the memory recall benchmark judge. Without a provider
    /// the sidecar is unavailable instead of answering through a different transport.
    pub fn with_model(model: impl Into<String>, reasoning_effort: Option<String>) -> Self {
        let model = model.into();
        let Some(provider) = sidecar_provider_fork() else {
            return Self::unavailable();
        };
        Self::select_model(&provider, &model);
        if let Some(effort) = reasoning_effort.as_deref()
            && let Err(error) = provider.set_reasoning_effort(effort)
        {
            crate::logging::warn(&format!(
                "The memory sidecar reasoning effort '{effort}' could not be selected: {error}"
            ));
        }
        Self::provider_backed(provider)
    }

    /// Return the currently selected backend label.
    pub fn backend_name(&self) -> &'static str {
        match self.backend {
            SidecarBackend::Provider => "provider",
            SidecarBackend::Unavailable => "unavailable",
        }
    }

    /// Simple completion - send a prompt, get a response.
    pub async fn complete(&self, system: &str, user_message: &str) -> Result<String> {
        match self.backend {
            SidecarBackend::Provider => self.complete_via_provider(system, user_message).await,
            SidecarBackend::Unavailable => Err(anyhow::anyhow!(
                "{}",
                self.unavailable_reason
                    .as_deref()
                    .unwrap_or("no sidecar backend is composed here")
            )),
        }
    }
    /// Complete via the live agent provider (`complete_simple`).
    ///
    /// This is the universal path: it works for every provider horon-agent supports,
    /// because `complete_simple` is a default method on the `Provider` trait that
    /// collects the streamed `TextDelta`s into a single string. The provider was
    /// forked at construction time, so it carries the user's selected model.
    async fn complete_via_provider(&self, system: &str, user_message: &str) -> Result<String> {
        let provider = self
            .provider
            .as_ref()
            .context("No active provider registered for sidecar; memory features require a logged-in provider")?;
        let (text, _usage) = provider
            .complete_simple_with_usage(user_message, system)
            .await
            .context("Sidecar completion via active provider failed")?;
        Ok(text)
    }

    /// Check if a memory is relevant to the current context
    /// Returns (is_relevant, explanation)
    pub async fn check_relevance(&self, memory_content: &str, current_context: &str) -> Result<(bool, String)> {
        let system = r#"You are a memory relevance checker. Your job is to determine if a stored memory is relevant to the current context.

Respond in this exact format:
RELEVANT: yes/no
REASON: <brief explanation>

Be conservative - only say "yes" if the memory would actually be useful for the current task."#;

        let prompt = format!(
            "## Stored Memory\n{}\n\n## Current Context\n{}\n\nIs this memory relevant to the current context?",
            memory_content, current_context
        );

        let response = self.complete(system, &prompt).await?;

        // Parse response
        let mut is_relevant = false;
        for line in response.lines() {
            let line = line.trim();
            if line.len() >= 9 && line[..9].eq_ignore_ascii_case("relevant:") {
                let value = line[9..].trim();
                is_relevant = value.eq_ignore_ascii_case("yes") || value.starts_with("yes");
                break;
            }
        }
        let reason = response
            .lines()
            .find(|line| line.to_lowercase().starts_with("reason:"))
            .map(|line| line.trim_start_matches(|c: char| !c.is_alphabetic()).trim())
            .unwrap_or(&response)
            .to_string();

        Ok((is_relevant, reason))
    }

    /// Check if new information contradicts existing information
    /// Returns true if the two statements are contradictory
    pub async fn check_contradiction(&self, new_content: &str, existing_content: &str) -> Result<bool> {
        let system = "You are a contradiction detector. Given two statements, determine if the new information directly contradicts the existing information. Reply with exactly YES or NO.";

        let prompt = format!(
            "## Existing Information\n{}\n\n## New Information\n{}\n\nDoes the new information contradict the existing information?",
            existing_content, new_content
        );

        let response = self.complete(system, &prompt).await?;
        let trimmed = response.trim().to_uppercase();
        Ok(trimmed.starts_with("YES"))
    }

    /// Extract memories from a session transcript
    pub async fn extract_memories(&self, transcript: &str) -> Result<Vec<ExtractedMemory>> {
        self.extract_memories_with_existing(transcript, &[]).await
    }

    /// Extract memories from a session transcript, aware of what's already stored.
    pub async fn extract_memories_with_existing(
        &self,
        transcript: &str,
        existing: &[String],
    ) -> Result<Vec<ExtractedMemory>> {
        let mut system = String::from(
            r#"You are a memory extraction assistant. Extract important NEW learnings from the conversation that should be remembered for future sessions.

Categories (use EXACTLY one of these):
- fact: Technical facts about the codebase, architecture, patterns, dependencies, tools, environment
- preference: User preferences, workflow habits, UX expectations, coding style, conventions, how they want the assistant to behave
- correction: Mistakes that were corrected, bugs found and fixed, wrong assumptions, things the user corrected
- entity: Named entities worth tracking - people, projects, services, repos, teams

Categorization rules:
- If it describes what the USER WANTS or HOW THEY LIKE THINGS, it is "preference", not "fact"
- If it describes a BUG FIX or MISTAKE, it is "correction", not "fact"
- "fact" is for objective technical information about code/systems, not user behavior

IMPORTANT - Do NOT extract:
- Transient debugging details, compile errors, or intermediate build steps
- Specific commit hashes, git operations, or "changes were committed/pushed" details
- Line-by-line code changes like "X was updated to Y in file Z" - these belong in git history, not memory
- Self-evident project context (e.g., the project name, repo URL, language) that is already in the system prompt
- Redundant variations of information already known (check the "Already known" list carefully)

Quality bar: Only extract information that would ACTUALLY BE USEFUL if recalled in a future session on a different topic. Ask: "Would a developer benefit from knowing this weeks from now?"

For each memory, output in this format (one per line):
CATEGORY|CONTENT|TRUST

Where:
- CATEGORY is one of: fact, preference, correction, entity
- CONTENT is a concise statement (1-2 sentences max, under 200 characters preferred)
- TRUST is one of: high (user stated), medium (observed), low (inferred)

Output ONLY the formatted lines, no other text. If no NEW memories worth extracting, output nothing."#,
        );

        if !existing.is_empty() {
            system.push_str("\n\nAlready known (do NOT re-extract these or close paraphrases):\n");
            for mem in existing.iter().take(80) {
                system.push_str("- ");
                system.push_str(crate::util::truncate_str(mem, 150));
                system.push('\n');
            }
        }

        let response = self.complete(&system, transcript).await?;

        let memories = response
            .lines()
            .filter(|line| line.contains('|'))
            .filter_map(|line| {
                let parts: Vec<&str> = line.split('|').collect();
                if parts.len() >= 3 {
                    Some(ExtractedMemory {
                        category: parts[0].trim().to_lowercase(),
                        content: parts[1].trim().to_string(),
                        trust: parts[2].trim().to_lowercase(),
                    })
                } else {
                    None
                }
            })
            .collect();

        Ok(memories)
    }
}

impl Default for Sidecar {
    fn default() -> Self {
        Self::new()
    }
}

/// A memory extracted by the sidecar
#[derive(Debug, Clone)]
pub struct ExtractedMemory {
    pub category: String,
    pub content: String,
    pub trust: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set_path(key: &'static str, value: &std::path::Path) -> Self {
            let previous = std::env::var_os(key);
            crate::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = &self.previous {
                crate::env::set_var(self.key, previous);
            } else {
                crate::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn sidecar_http_error_classifies_permanent_client_failures() {
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::FORBIDDEN,
            StatusCode::NOT_FOUND,
        ] {
            let error: anyhow::Error = SidecarHttpError {
                provider: "test",
                status,
                body: "failure".to_string(),
            }
            .into();
            assert_eq!(classify_error(&error), SidecarErrorKind::Permanent);
        }
    }

    #[test]
    fn sidecar_http_error_classifies_retryable_failures() {
        for status in [StatusCode::TOO_MANY_REQUESTS, StatusCode::BAD_GATEWAY] {
            let error: anyhow::Error = SidecarHttpError {
                provider: "test",
                status,
                body: "failure".to_string(),
            }
            .into();
            assert_eq!(classify_error(&error), SidecarErrorKind::Transient);
        }
        assert_eq!(
            classify_error(&anyhow::anyhow!("connection reset")),
            SidecarErrorKind::Transient
        );
    }

    /// Provider stub that keeps the selected model, answers `complete`, and
    /// counts every call so a test can prove which provider served the request.
    struct RecordingStub {
        model: Mutex<String>,
        reply: String,
        calls: Arc<AtomicUsize>,
    }

    impl RecordingStub {
        fn new(model: &str, reply: &str) -> Self {
            Self::with_calls(model, reply, Arc::new(AtomicUsize::new(0)))
        }

        fn with_calls(model: &str, reply: &str, calls: Arc<AtomicUsize>) -> Self {
            Self {
                model: Mutex::new(model.to_string()),
                reply: reply.to_string(),
                calls,
            }
        }
    }

    #[async_trait::async_trait]
    impl crate::provider::Provider for RecordingStub {
        async fn complete(
            &self,
            _messages: &[crate::message::Message],
            _tools: &[crate::message::ToolDefinition],
            _system: &str,
            _resume_session_id: Option<&str>,
        ) -> Result<crate::provider::EventStream> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let reply = self.reply.clone();
            let stream =
                futures::stream::once(async move { Ok(horon_agent_message_types::StreamEvent::TextDelta(reply)) });
            Ok(Box::pin(stream))
        }

        fn name(&self) -> &str {
            "openai-compatible"
        }

        fn model(&self) -> String {
            self.model.lock().unwrap().clone()
        }

        fn set_model(&self, model: &str) -> Result<()> {
            self.restore_model(model)
        }

        fn restore_model(&self, model: &str) -> Result<()> {
            *self.model.lock().unwrap() = model.to_string();
            Ok(())
        }

        fn fork(&self) -> Arc<dyn crate::provider::Provider> {
            Arc::new(RecordingStub::with_calls(
                &self.model(),
                &self.reply,
                Arc::clone(&self.calls),
            ))
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    /// The sidecar forks the active provider and answers through it.
    #[test]
    fn the_sidecar_uses_the_active_provider() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::TempDir::new().expect("create temp horon-agent home");
        let _home = EnvVarGuard::set_path("HORON_HOME", temp.path());

        let stub = Arc::new(RecordingStub::new("active-model", "[2,1]"));
        crate::provider::set_active_provider(stub.clone());

        let sidecar = Sidecar::with_configured_model(None);
        assert_eq!(sidecar.backend_name(), "provider");
        assert_eq!(sidecar.model_name(), "active-model");
        assert!(Sidecar::llm_backend_available());
        assert_eq!(
            block_on(sidecar.complete("rank these", "1. a\n2. b")).expect("provider-backed completion"),
            "[2,1]"
        );
        assert_eq!(stub.calls.load(Ordering::SeqCst), 1);
    }

    /// The session's memory model is applied to the fork, without ever being
    /// parsed as a built-in transport prefix.
    #[test]
    fn session_sidecar_uses_session_memory_model_override() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::TempDir::new().expect("create temp horon-agent home");
        let _home = EnvVarGuard::set_path("HORON_HOME", temp.path());
        std::fs::write(
            temp.path().join("config.toml"),
            "[agents]\nmemory_model = \"global-memory-model\"\n",
        )
        .unwrap();
        crate::provider::set_active_provider(Arc::new(RecordingStub::new("coordinator-model", "ok")));
        let mut session = crate::session::Session::create_with_id("sidecar_memory".into(), None, None);
        session.save_prepared().unwrap();
        session
            .set_agent_model_override("memory", Some("session-memory-model".into()))
            .unwrap();
        assert_eq!(Sidecar::for_session(&session).model_name(), "session-memory-model");
        session.set_agent_model_override("memory", None).unwrap();
        assert_eq!(Sidecar::for_session(&session).model_name(), "global-memory-model");
    }

    #[test]
    fn inherited_sidecar_model_is_not_reparsed_as_a_user_selection() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::TempDir::new().expect("create temp horon-agent home");
        let _home = EnvVarGuard::set_path("HORON_HOME", temp.path());
        std::fs::write(
            temp.path().join("config.toml"),
            "[agents]\nmemory_model = \"inherit\"\n",
        )
        .unwrap();
        crate::provider::set_active_provider(Arc::new(RecordingStub::new("coordinator-model", "ok")));
        let mut session = crate::session::Session::create_with_id("sidecar_inherit".into(), None, None);
        for model in ["vendor:beta", "vendor/model@revision"] {
            session.model = Some(model.to_string());
            assert_eq!(Sidecar::for_session(&session).model_name(), model);
        }
    }

    #[test]
    fn provider_sidecar_keeps_the_route_selected_at_construction() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::TempDir::new().expect("create temp horon-agent home");
        let _home = EnvVarGuard::set_path("HORON_HOME", temp.path());

        crate::provider::set_active_provider(Arc::new(RecordingStub::new(
            "configured-profile-model",
            "configured-profile-response",
        )));
        let sidecar = Sidecar::with_configured_model(None);

        // Model switches and catalog refreshes can replace the process-global provider while a
        // background memory request is queued. Dispatch must still use the fork selected above.
        crate::provider::set_active_provider(Arc::new(RecordingStub::new("fallback-route", "wrong-response")));

        assert_eq!(
            block_on(sidecar.complete("extract memories", "conversation")).expect("provider-backed completion"),
            "configured-profile-response"
        );
        assert_eq!(sidecar.model_name(), "configured-profile-model");
    }

    /// The benchmark judge's explicit model constructor pins a model and effort.
    #[test]
    fn with_model_pins_the_model_and_reports_availability() {
        let _guard = crate::storage::lock_test_env();
        let temp = tempfile::TempDir::new().expect("create temp horon-agent home");
        let _home = EnvVarGuard::set_path("HORON_HOME", temp.path());

        crate::provider::set_active_provider(Arc::new(RecordingStub::new("coordinator-model", "ok")));
        let sidecar = Sidecar::with_model("judge-model", Some("none".to_string()));
        assert_eq!(sidecar.backend_name(), "provider");
        assert_eq!(sidecar.model_name(), "judge-model");
    }
}
