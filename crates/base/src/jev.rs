//! Shared Jev typed Decisions transport, separate from chat completions.
//!
//! BYOK credentials are bound to fixed provider endpoints. Memory and voice
//! recall use them directly; there is no hosted subscription route.

use anyhow::{Result, anyhow, bail, ensure};
use reqwest::{Client, Response};
use serde_json::{Map, Value, json};
use std::time::Duration;

const PROVIDER_ENV: &str = "HORON_AGENT_MEMORY_JEV_PROVIDER";
const VOICE_PROVIDER_ENV: &str = "HORON_AGENT_VOICE_JEV_PROVIDER";
const MAX_REQUEST_BYTES: usize = 80 * 1024;
const MAX_RESPONSE_BYTES: usize = 256 * 1024;
pub(crate) const MAX_QUESTIONS: usize = 24;
/// Typesafe direct accepts far more (256 verified live on 2026-09-23). A voice
/// request has at most 26 questions, so it always fits in one round trip.
const TYPESAFE_MAX_QUESTIONS: usize = 64;

/// Typesafe latency is bimodal and sticky per connection: a connection is
/// served in ~150ms or 2-12s (measured live 2026-09-23). Voice waits on this
/// before inserting or sending, so duplicates on fresh connections race the
/// primary. Decisions are side-effect-free and cost fractions of a cent.
#[cfg(not(test))]
const VOICE_HEDGE_DELAYS: [Duration; 3] = [
    Duration::from_millis(300),
    Duration::from_millis(700),
    Duration::from_millis(1500),
];
#[cfg(test)]
const VOICE_HEDGE_DELAYS: [Duration; 3] = [
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(150),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JevPurpose {
    Memory,
    Voice,
}

impl JevPurpose {
    fn selector_with(
        self,
        env: impl FnOnce(&str) -> Result<String, std::env::VarError>,
        memory_default: impl FnOnce() -> String,
    ) -> Result<String> {
        let key = match self {
            Self::Memory => PROVIDER_ENV,
            Self::Voice => VOICE_PROVIDER_ENV,
        };
        match env(key) {
            Ok(value) => Ok(value),
            Err(std::env::VarError::NotPresent) => Ok(match self {
                Self::Memory => memory_default(),
                Self::Voice => "auto".into(),
            }),
            Err(_) => bail!("{key} must contain a valid provider name"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum JevProvider {
    OpenRouter,
    TypeSafe,
    Aimlapi,
}

impl JevProvider {
    fn name(self) -> &'static str {
        match self {
            Self::OpenRouter => "openrouter",
            Self::TypeSafe => "typesafe",
            Self::Aimlapi => "aimlapi",
        }
    }

    fn credentials(self) -> (&'static str, &'static str) {
        match self {
            Self::OpenRouter => ("OPENROUTER_API_KEY", "openrouter.env"),
            Self::TypeSafe => ("TYPESAFE_API_KEY", "typesafe.env"),
            Self::Aimlapi => ("AIMLAPI_API_KEY", "aimlapi.env"),
        }
    }

    fn max_questions(self) -> usize {
        match self {
            Self::TypeSafe => TYPESAFE_MAX_QUESTIONS,
            _ => MAX_QUESTIONS,
        }
    }

    fn model(self) -> &'static str {
        match self {
            Self::OpenRouter => "typesafe/jev-1.13",
            Self::TypeSafe => "jev-latest",
            Self::Aimlapi => "typesafe/jev",
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Self::OpenRouter => "https://openrouter.ai/api/alpha/decisions",
            Self::TypeSafe => "https://api.typesafe.ai/v1/systemone",
            Self::Aimlapi => "https://api.aimlapi.com/v1/decisions",
        }
    }
}

/// Do not derive Debug: this contains a provider secret.
#[derive(Clone)]
pub struct JevClient {
    client: Client,
    /// Voice only. Typesafe speed is sticky per connection (a slow connection
    /// stays slow), so each hedge needs its own client and connection.
    hedge_clients: Vec<Client>,
    purpose: JevPurpose,
    provider: JevProvider,
    api_key: String,
    endpoint: String,
}

impl JevClient {
    /// A configured credential route exists. This is not a health probe.
    pub fn available() -> bool {
        Self::resolve(JevPurpose::Memory).is_ok()
    }

    pub fn new() -> Result<Self> {
        Self::for_purpose(JevPurpose::Memory)
    }

    /// Voice uses Typesafe directly. Other provider keys and memory configuration
    /// are ignored. HORON_AGENT_VOICE_JEV_PROVIDER may explicitly select
    /// typesafe. Evaluation never changes accounts.
    pub fn for_voice() -> Result<Self> {
        Self::for_purpose(JevPurpose::Voice)
    }

    fn for_purpose(purpose: JevPurpose) -> Result<Self> {
        let (provider, api_key) = Self::resolve(purpose)?;
        let build = || {
            client_builder()
                .build()
                .map_err(|_| anyhow!("Could not initialize the Jev decision client"))
        };
        let client = build()?;
        let hedge_clients = if purpose == JevPurpose::Voice {
            VOICE_HEDGE_DELAYS.iter().map(|_| build()).collect::<Result<_>>()?
        } else {
            Vec::new()
        };
        Ok(Self {
            client,
            hedge_clients,
            purpose,
            provider,
            api_key,
            endpoint: provider.endpoint().to_string(),
        })
    }

    fn resolve(purpose: JevPurpose) -> Result<(JevProvider, String)> {
        let selector = purpose.selector_with(
            |key| std::env::var(key),
            || crate::config::config().agents.memory_jev_provider.clone(),
        )?;
        // Unlike the API-key helper, this does not consult registered
        // cross-provider fallback resolvers or the shared compatible slot.
        let load = |env: &str, file: &str| crate::provider_catalog::load_env_value_from_env_or_config(env, file);
        let (provider, api_key) = if purpose == JevPurpose::Voice {
            resolve_voice_with(&selector, load)?
        } else {
            resolve_with(&selector, load)?
        };
        Ok((provider, api_key))
    }

    pub fn provider_name(&self) -> &str {
        self.provider.name()
    }

    pub fn model_id(&self) -> &str {
        self.provider.model()
    }

    /// Return the full typed Decisions response, including provider usage.
    /// Never retries using another provider or account after an auth/billing
    /// failure. Callers own the relevance threshold and uncertainty policy.
    pub async fn evaluate(&self, state: Value, questions: Map<String, Value>) -> Result<Value> {
        let body = request_body_for(self.provider, state, &questions)?;
        if self.purpose == JevPurpose::Voice {
            return self.send_hedged(body, &questions).await;
        }
        let value = self.send(&self.endpoint, body).await?;
        validate_answers(&value, &questions)?;
        Ok(value)
    }

    /// Send once, then again on a fresh connection after each
    /// [`VOICE_HEDGE_DELAYS`] step while nothing has answered. The first valid
    /// answer wins and the rest are dropped. A primary failure returns at once,
    /// so auth/billing errors are never duplicated. Hedge failures are ignored
    /// while any other attempt is still pending.
    async fn send_hedged(&self, body: Vec<u8>, questions: &Map<String, Value>) -> Result<Value> {
        use futures::StreamExt;
        let attempt = |client: &Client, delay: Duration, primary: bool| {
            let body = body.clone();
            let client = client.clone();
            async move {
                if !delay.is_zero() {
                    tokio::time::sleep(delay).await;
                }
                let result = async {
                    let value = self.send_with(&client, &self.endpoint, body).await?;
                    validate_answers(&value, questions)?;
                    Ok::<_, anyhow::Error>(value)
                }
                .await;
                (primary, result)
            }
        };
        let mut attempts = futures::stream::FuturesUnordered::new();
        attempts.push(attempt(&self.client, Duration::ZERO, true));
        for (client, delay) in self.hedge_clients.iter().zip(VOICE_HEDGE_DELAYS) {
            attempts.push(attempt(client, delay, false));
        }
        let mut hedge_error = None;
        while let Some((primary, result)) = attempts.next().await {
            match result {
                Ok(value) => return Ok(value),
                Err(error) if primary => return Err(error),
                Err(error) => hedge_error = Some(error),
            }
        }
        Err(hedge_error.unwrap_or_else(|| anyhow!("Jev decision request failed")))
    }

    async fn send(&self, endpoint: &str, body: Vec<u8>) -> Result<Value> {
        self.send_with(&self.client, endpoint, body).await
    }

    /// Decisions requests are side-effect-free classifications, so transient
    /// overload responses (429/502/503/504/529) are retried with a short bounded
    /// backoff on the same provider and account. Auth, billing, redirect, and
    /// other failures are never retried and never fall back to another account.
    async fn send_with(&self, client: &Client, endpoint: &str, body: Vec<u8>) -> Result<Value> {
        let mut attempt = 0;
        loop {
            let response = self.send_once(client, endpoint, body.clone()).await?;
            let status = response.status().as_u16();
            // A long Retry-After means a plan quota (hours), not overload.
            // Retrying cannot succeed, so surface the upgrade prompt at once.
            let long_wait = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok())
                .is_some_and(|secs| Duration::from_secs(secs) > MAX_RETRY_AFTER);
            if attempt < TRANSIENT_RETRY_DELAYS.len() && is_transient_status(status) && !long_wait {
                let delay = retry_after(&response).unwrap_or(TRANSIENT_RETRY_DELAYS[attempt]);
                attempt += 1;
                crate::logging::info(&format!(
                    "Jev {} returned HTTP {status}; retry {attempt}/{} in {}ms",
                    self.provider.name(),
                    TRANSIENT_RETRY_DELAYS.len(),
                    delay.as_millis()
                ));
                tokio::time::sleep(delay).await;
                continue;
            }
            return read_response(response, MAX_RESPONSE_BYTES).await;
        }
    }

    async fn send_once(&self, client: &Client, endpoint: &str, body: Vec<u8>) -> Result<Response> {
        let mut request = client
            .post(endpoint)
            .bearer_auth(&self.api_key)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body);
        if self.provider == JevProvider::OpenRouter {
            request = request.header("HTTP-Referer", "https://horon-agent.sh").header(
                "X-Title",
                match self.purpose {
                    JevPurpose::Memory => "HoronAgent Memory",
                    JevPurpose::Voice => "HoronAgent Voice",
                },
            );
        }
        request
            .send()
            .await
            .map_err(|_| anyhow!("Jev decision request failed or timed out; check the selected provider"))
    }
}

#[cfg(not(test))]
const TRANSIENT_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(600), Duration::from_millis(1800)];
#[cfg(test)]
const TRANSIENT_RETRY_DELAYS: [Duration; 2] = [Duration::from_millis(1), Duration::from_millis(1)];
const MAX_RETRY_AFTER: Duration = Duration::from_secs(5);

fn is_transient_status(status: u16) -> bool {
    matches!(status, 429 | 502 | 503 | 504 | 529)
}

/// Honor a short numeric Retry-After. Long waits fail fast instead of
/// stalling the caller beyond the bounded retry budget.
fn retry_after(response: &Response) -> Option<Duration> {
    let secs: u64 = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let delay = Duration::from_secs(secs);
    if cfg!(test) {
        return Some(Duration::from_millis(1));
    }
    (delay <= MAX_RETRY_AFTER).then_some(delay)
}

fn client_builder() -> reqwest::ClientBuilder {
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(25))
        .redirect(reqwest::redirect::Policy::none())
}

fn resolve_voice_with(selector: &str, load: impl FnMut(&str, &str) -> Option<String>) -> Result<(JevProvider, String)> {
    match selector.trim().to_ascii_lowercase().as_str() {
        "auto" => resolve_providers(&[JevProvider::TypeSafe], load),
        "typesafe" => resolve_with(selector, load),
        "horon-agent" | "subscription" | "horon-agent-subscription" => bail!(
            "The hosted HoronAgent Jev subscription was removed. Set HORON_AGENT_VOICE_JEV_PROVIDER=typesafe and configure TYPESAFE_API_KEY"
        ),
        _ => bail!("Invalid voice Jev provider. Choose auto or typesafe; voice never uses OpenRouter or AIMLAPI"),
    }
}

fn resolve_with(selector: &str, load: impl FnMut(&str, &str) -> Option<String>) -> Result<(JevProvider, String)> {
    let providers: &[JevProvider] = match selector.trim().to_ascii_lowercase().as_str() {
        "auto" => &[
            // Typesafe serves Jev directly, so it beats resellers of the same model.
            JevProvider::TypeSafe,
            JevProvider::OpenRouter,
            JevProvider::Aimlapi,
        ],
        "openrouter" => &[JevProvider::OpenRouter],
        "typesafe" => &[JevProvider::TypeSafe],
        "aimlapi" => &[JevProvider::Aimlapi],
        "horon-agent" | "subscription" | "horon-agent-subscription" => {
            bail!("The hosted HoronAgent Jev subscription was removed. Choose auto, openrouter, typesafe, or aimlapi")
        }
        _ => bail!("Invalid Jev provider. Choose auto, openrouter, typesafe, or aimlapi"),
    };
    resolve_providers(providers, load)
}

fn resolve_providers(
    providers: &[JevProvider],
    mut load: impl FnMut(&str, &str) -> Option<String>,
) -> Result<(JevProvider, String)> {
    for &provider in providers {
        let (env, file) = provider.credentials();
        if let Some(key) = load(env, file) {
            let key = horon_agent_provider_env::sanitize_secret_value(&key);
            if !key.is_empty() {
                // Reject malformed headers now rather than leaking a provider's
                // request-builder error through a later error chain.
                ensure!(
                    reqwest::header::HeaderValue::from_str(&format!("Bearer {key}")).is_ok(),
                    "The selected Jev provider credential is not a valid HTTP bearer value"
                );
                return Ok((provider, key.to_owned()));
            }
        }
    }
    if let [provider] = providers {
        let (env, file) = provider.credentials();
        bail!(
            "No credential for the selected Jev provider {}. Configure {env} ({file}). Explicit provider selection never falls back to another account.",
            provider.name()
        );
    }
    let credentials = providers
        .iter()
        .map(|provider| {
            let (env, file) = provider.credentials();
            format!("{env} ({file})")
        })
        .collect::<Vec<_>>()
        .join(" or ");
    bail!(
        "No credential for the selected Jev route. Configure {credentials}. Provider failures never fall back to another account."
    )
}

#[cfg(test)]
fn request_body(provider: JevProvider, state: Value, questions: &Map<String, Value>) -> Result<Vec<u8>> {
    request_body_for(provider, state, questions)
}

fn request_body_for(provider: JevProvider, state: Value, questions: &Map<String, Value>) -> Result<Vec<u8>> {
    ensure!(
        state.is_string() || state.is_object() || state.is_array(),
        "Jev state must be text, an object, or an array"
    );
    ensure!(
        (1..=provider.max_questions()).contains(&questions.len()),
        "Jev requests require between 1 and {} questions",
        provider.max_questions()
    );
    for (id, question) in questions {
        ensure!(!id.is_empty() && id.len() <= 64, "Invalid Jev question ID");
        let instructions = &question["instructions"];
        ensure!(
            instructions.is_string() || instructions.is_object() || instructions.is_array(),
            "Jev questions require instructions"
        );
        match question["type"].as_str() {
            Some("noul") => {}
            Some("choice") => {
                let criteria = question["criteria"].as_object();
                ensure!(
                    criteria.is_some_and(|criteria| {
                        (2..=255).contains(&criteria.len()) && criteria.values().all(|v| v.is_string() || v.is_null())
                    }),
                    "Jev choice questions require 2 to 255 described options"
                );
            }
            Some("score") => ensure!(
                question["criteria"].as_array().is_some_and(|criteria| {
                    (2..=255).contains(&criteria.len()) && criteria.iter().all(Value::is_string)
                }),
                "Jev score questions require 2 to 255 level descriptions"
            ),
            _ => bail!("Unsupported Jev question type; expected noul, choice, or score"),
        }
    }
    // String state is accepted by every provider and preserves the existing
    // OpenRouter Decisions wire contract. Direct providers retain structured data.
    let state = if provider == JevProvider::OpenRouter && !state.is_string() {
        Value::String(serde_json::to_string(&state).map_err(|_| anyhow!("Invalid Jev state"))?)
    } else {
        state
    };
    let body = serde_json::to_vec(&json!({
        "model": provider.model(), "state": state, "questions": questions
    }))
    .map_err(|_| anyhow!("Could not encode Jev request"))?;
    ensure!(
        body.len() <= MAX_REQUEST_BYTES,
        "Jev request exceeds the bounded context size"
    );
    Ok(body)
}

async fn read_response(mut response: Response, limit: usize) -> Result<Value> {
    let status = response.status();
    if !status.is_success() {
        let hint = match status.as_u16() {
            401 => "selected provider credential is invalid or revoked",
            403 => "selected provider denied access or the account is not entitled",
            402 => "selected provider credits or account spending limit are exhausted",
            404 => "selected gateway does not support this Jev endpoint",
            429 | 529 => "selected provider is rate limited or overloaded; try again later",
            300..=399 => "redirect refused to protect provider credentials",
            _ => "selected provider is unavailable or rejected the request",
        };
        bail!("Jev returned HTTP {}: {hint}", status.as_u16());
    }
    ensure!(
        !response.content_length().is_some_and(|length| length > limit as u64),
        "Jev response exceeds the bounded response size"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("Could not read Jev response"))?
    {
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= limit,
            "Jev response exceeds the bounded response size"
        );
        bytes.extend_from_slice(&chunk);
    }
    // Never retain serde's diagnostic, which can quote untrusted response data.
    serde_json::from_slice(&bytes).map_err(|_| anyhow!("Jev returned invalid response JSON"))
}

fn validate_answers(value: &Value, questions: &Map<String, Value>) -> Result<()> {
    let answers = value["answers"]
        .as_object()
        .ok_or_else(|| anyhow!("Jev returned no typed answers"))?;
    ensure!(
        answers.len() == questions.len(),
        "Jev returned an incomplete or unexpected answer set"
    );
    for (id, question) in questions {
        let answer = answers
            .get(id)
            .ok_or_else(|| anyhow!("Jev omitted a requested answer"))?;
        ensure!(
            answer["type"] == question["type"],
            "Jev answer type does not match its question"
        );
        if question["type"] == "noul" {
            ensure!(
                answer["noul"]
                    .as_f64()
                    .is_some_and(|v| v.is_finite() && (0.0..=1.0).contains(&v)),
                "Jev returned an invalid noul probability"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn questions() -> Map<String, Value> {
        json!({"m0": {"type": "noul", "instructions": "Is this memory relevant?", "criteria": {"true": "Useful", "false": "Irrelevant"}}})
            .as_object().unwrap().clone()
    }

    fn response() -> Value {
        json!({"answers": {"m0": {"type": "noul", "noul": 0.91}}, "usage": {"input_tokens": 123, "output_tokens": 4}})
    }

    #[test]
    fn voice_routes_only_to_typesafe() {
        let selector = JevPurpose::Voice
            .selector_with(
                |key| {
                    assert_eq!(key, VOICE_PROVIDER_ENV);
                    Err(std::env::VarError::NotPresent)
                },
                || panic!("voice must ignore memory configuration"),
            )
            .unwrap();
        assert_eq!(selector, "auto");
        assert_eq!(
            JevPurpose::Voice
                .selector_with(
                    |key| {
                        assert_eq!(key, VOICE_PROVIDER_ENV);
                        Ok("typesafe".into())
                    },
                    || panic!()
                )
                .unwrap(),
            "typesafe"
        );
        let (provider, _) = resolve_voice_with("auto", |env, _| {
            assert_eq!(env, "TYPESAFE_API_KEY");
            Some("test-key".into())
        })
        .unwrap();
        assert_eq!(provider, JevProvider::TypeSafe);
        for selector in ["horon-agent", "subscription", "horon-agent-subscription"] {
            let error = resolve_voice_with(selector, |_, _| panic!("removed route must not load keys")).unwrap_err();
            assert!(error.to_string().contains("was removed"), "{error}");
        }
        for selector in ["openrouter", "aimlapi", ""] {
            assert!(resolve_voice_with(selector, |_, _| panic!("invalid voice route must not load keys")).is_err());
        }
        assert!(resolve_voice_with("auto", |_, _| None).is_err());
    }

    #[test]
    fn memory_selector_uses_its_own_override_and_config_default() {
        let env = |key: &str| match key {
            PROVIDER_ENV => Ok("typesafe".into()),
            _ => panic!("unexpected configuration lookup"),
        };
        assert_eq!(JevPurpose::Memory.selector_with(env, || panic!()).unwrap(), "typesafe");
        assert_eq!(
            JevPurpose::Memory
                .selector_with(
                    |key| {
                        assert_eq!(key, PROVIDER_ENV);
                        Err(std::env::VarError::NotPresent)
                    },
                    || "aimlapi".into(),
                )
                .unwrap(),
            "aimlapi"
        );
        assert!(
            JevPurpose::Memory
                .selector_with(
                    |_| Err(std::env::VarError::NotUnicode("invalid".into())),
                    || panic!("invalid override must not use defaults"),
                )
                .is_err()
        );
    }

    #[test]
    fn resolver_keeps_provider_credentials_and_endpoints_isolated() {
        for (selector, expected, key, file, endpoint, model) in [
            (
                "openrouter",
                JevProvider::OpenRouter,
                "OPENROUTER_API_KEY",
                "openrouter.env",
                "https://openrouter.ai/api/alpha/decisions",
                "typesafe/jev-1.13",
            ),
            (
                "typesafe",
                JevProvider::TypeSafe,
                "TYPESAFE_API_KEY",
                "typesafe.env",
                "https://api.typesafe.ai/v1/systemone",
                "jev-latest",
            ),
            (
                "aimlapi",
                JevProvider::Aimlapi,
                "AIMLAPI_API_KEY",
                "aimlapi.env",
                "https://api.aimlapi.com/v1/decisions",
                "typesafe/jev",
            ),
        ] {
            let (provider, secret) = resolve_with(selector, |env, env_file| {
                assert_eq!((env, env_file), (key, file));
                Some(format!("test-{selector}"))
            })
            .unwrap();
            assert_eq!(provider, expected);
            assert_eq!(secret, format!("test-{selector}"));
            assert_eq!(provider.endpoint(), endpoint);
            assert_eq!(provider.model(), model);
        }
    }

    #[test]
    fn explicit_missing_provider_never_uses_another_key() {
        let mut lookups = Vec::new();
        let error = resolve_with("typesafe", |env, _| {
            lookups.push(env.to_string());
            (env != "TYPESAFE_API_KEY").then(|| "other-provider-secret".into())
        })
        .err()
        .unwrap();
        assert_eq!(lookups, ["TYPESAFE_API_KEY"]);
        assert!(!error.to_string().contains("other-provider-secret"));
        assert!(error.to_string().contains("TYPESAFE_API_KEY (typesafe.env)"));
        assert!(!error.to_string().contains("OPENROUTER_API_KEY"));
    }

    #[test]
    fn auto_prefers_typesafe_then_billed_resellers() {
        for available in ["OPENROUTER_API_KEY", "TYPESAFE_API_KEY", "AIMLAPI_API_KEY"] {
            let (provider, _) = resolve_with("auto", |env, _| {
                assert!(!env.starts_with("HORON_AGENT_OPENROUTER"));
                (env == available).then(|| "test-secret".into())
            })
            .unwrap();
            assert_eq!(provider.credentials().0, available);
        }
        // Typesafe direct wins over resellers of the same model.
        assert_eq!(
            resolve_with("auto", |_, _| Some("all-present".into())).unwrap().0,
            JevProvider::TypeSafe
        );
        assert_eq!(
            resolve_with("openrouter", |_, _| Some("all-present".into())).unwrap().0,
            JevProvider::OpenRouter
        );
        for selector in ["horon-agent", "subscription", "horon-agent-subscription"] {
            let error = resolve_with(selector, |_, _| panic!("removed route must not load keys")).unwrap_err();
            assert!(error.to_string().contains("was removed"), "{error}");
        }
        assert!(resolve_with("auto", |_, _| None).is_err());
        assert!(resolve_with("untrusted-selector", |_, _| panic!("must not load any credential")).is_err());
        assert!(resolve_with("", |_, _| panic!("empty override must not choose another account")).is_err());
    }

    #[test]
    fn empty_and_malformed_credentials_are_rejected_without_echoing() {
        assert!(resolve_with("openrouter", |_, _| Some(" \"\" ".into())).is_err());
        let secret = "sensitive\r\nheader";
        let error = resolve_with("openrouter", |_, _| Some(secret.into())).err().unwrap();
        assert!(!format!("{error:#}").contains("sensitive"));
    }

    #[test]
    fn request_uses_decisions_not_chat_and_preserves_provider_models() {
        for provider in [JevProvider::OpenRouter, JevProvider::TypeSafe, JevProvider::Aimlapi] {
            let body: Value =
                serde_json::from_slice(&request_body(provider, json!({"memory": "example"}), &questions()).unwrap())
                    .unwrap();
            assert_eq!(body["model"], provider.model());
            assert_eq!(body["questions"], Value::Object(questions()));
            assert!(body.get("messages").is_none());
        }
    }

    #[test]
    fn request_bounds_and_gateway_contract_are_checked_before_network() {
        assert!(
            request_body(
                JevProvider::OpenRouter,
                json!("x".repeat(MAX_REQUEST_BYTES)),
                &questions()
            )
            .is_err()
        );
        assert!(request_body(JevProvider::OpenRouter, Value::Null, &questions()).is_err());
        assert!(request_body(JevProvider::OpenRouter, json!("state"), &Map::new()).is_err());
        let many = (0..25).map(|i| (format!("m{i}"), questions()["m0"].clone())).collect();
        assert!(request_body(JevProvider::OpenRouter, json!("state"), &many).is_err());
        let choice = json!({"pick": {"type": "choice", "instructions": "Pick", "criteria": {"a": "A", "b": "B"}}})
            .as_object()
            .unwrap()
            .clone();
        assert!(request_body(JevProvider::OpenRouter, json!("state"), &choice).is_ok());
    }

    #[test]
    fn answers_must_match_questions_and_have_valid_noul_probabilities() {
        assert!(validate_answers(&response(), &questions()).is_ok());
        for value in [
            json!({}),
            json!({"answers": {}}),
            json!({"answers": {"other": {"type": "noul", "noul": 0.9}}}),
            json!({"answers": {"m0": {"type": "choice", "noul": 0.9}}}),
            json!({"answers": {"m0": {"type": "noul", "noul": 1.1}}}),
        ] {
            assert!(validate_answers(&value, &questions()).is_err());
        }
    }

    // One listener can serve preflight + decision, and captures the exact wire
    // requests without touching credentials, process environment, or real APIs.
    type MockReply = (u16, String, Vec<(String, String)>);

    fn mock_server(replies: Vec<MockReply>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let count = replies.len();
        let mut replies = replies.into_iter();
        mock_server_with(count, move |_| replies.next().unwrap())
    }

    /// Serve `count` requests, choosing each reply from the request itself, so
    /// concurrent requests get correct replies whatever order they connect in.
    fn mock_server_with(
        count: usize,
        mut respond: impl FnMut(&str) -> MockReply + Send + 'static,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for _ in 0..count {
                let deadline = std::time::Instant::now() + Duration::from_secs(30);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(std::time::Instant::now() < deadline, "expected mock request");
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accept: {error}"),
                    }
                };
                // On BSD/macOS an accepted socket inherits the listener's
                // non-blocking flag, so without this the timed read below
                // returns WouldBlock instead of waiting for the request.
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0u8; 4096];
                loop {
                    let n = stream.read(&mut buffer).unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let len = headers
                            .lines()
                            .find_map(|line| {
                                let (key, value) = line.split_once(':')?;
                                key.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                let request = String::from_utf8(bytes).unwrap();
                let (status, body, headers) = respond(&request);
                requests.push(request);
                let chunked = headers
                    .iter()
                    .any(|(key, value)| key.eq_ignore_ascii_case("transfer-encoding") && value == "chunked");
                let extra: String = headers.into_iter().map(|(k, v)| format!("{k}: {v}\r\n")).collect();
                let (length_header, wire_body) = if chunked {
                    (String::new(), format!("{:x}\r\n{body}\r\n0\r\n\r\n", body.len()))
                } else {
                    (format!("Content-Length: {}\r\n", body.len()), body)
                };
                let reply = format!(
                    "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\n{length_header}Connection: close\r\n{extra}\r\n{wire_body}"
                );
                // A bounded client may close early on an oversized response.
                let _ = stream.write_all(reply.as_bytes());
            }
            requests
        });
        (base, worker)
    }

    fn mock_client(base: &str, provider: JevProvider) -> JevClient {
        JevClient {
            client: client_builder().no_proxy().build().unwrap(),
            hedge_clients: VOICE_HEDGE_DELAYS
                .iter()
                .map(|_| client_builder().no_proxy().build().unwrap())
                .collect(),
            purpose: JevPurpose::Memory,
            provider,
            api_key: "test-route-secret".into(),
            endpoint: format!("{base}/v1/decisions"),
        }
    }

    /// Serve every connection on its own thread. Connection `n` waits
    /// `delays[n]` before replying, so a slow primary can overlap a hedge.
    fn concurrent_mock(delays: Vec<(Duration, u16)>) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = seen.clone();
        std::thread::spawn(move || {
            for (index, stream) in listener.incoming().enumerate() {
                let Ok(mut stream) = stream else { return };
                let Some(&(delay, status)) = delays.get(index) else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    let mut buffer = [0u8; 4096];
                    while !bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                        let n = stream.read(&mut buffer).unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        bytes.extend_from_slice(&buffer[..n]);
                    }
                    std::thread::sleep(delay);
                    let body = response().to_string();
                    let _ = stream.write_all(format!("HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes());
                });
            }
        });
        (base, seen)
    }

    #[tokio::test]
    async fn voice_hedges_a_slow_request_and_takes_the_first_valid_answer() {
        let (base, seen) = concurrent_mock(vec![(Duration::from_secs(3), 200), (Duration::ZERO, 200)]);
        let mut client = mock_client(&base, JevProvider::TypeSafe);
        client.purpose = JevPurpose::Voice;
        let started = std::time::Instant::now();
        let value = client
            .evaluate(json!({"transcript": "synthetic"}), questions())
            .await
            .unwrap();
        assert!(validate_answers(&value, &questions()).is_ok());
        assert!(started.elapsed() < Duration::from_secs(2), "hedge won");
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn voice_hedge_failure_waits_for_primary_and_memory_never_hedges() {
        let (base, seen) = concurrent_mock(vec![(Duration::from_millis(300), 200), (Duration::ZERO, 500)]);
        let mut client = mock_client(&base, JevProvider::TypeSafe);
        client.purpose = JevPurpose::Voice;
        client
            .evaluate(json!({"transcript": "synthetic"}), questions())
            .await
            .unwrap();
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 2);

        let (base, seen) = concurrent_mock(vec![(Duration::from_millis(300), 200)]);
        mock_client(&base, JevProvider::TypeSafe)
            .evaluate(json!({"transcript": "synthetic"}), questions())
            .await
            .unwrap();
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn transient_overload_is_retried_on_same_route_then_succeeds() {
        for status in [429, 502, 503, 504, 529] {
            let (base, worker) = mock_server(vec![
                (status, "{}".into(), vec![("Retry-After".into(), "1".into())]),
                (status, "{}".into(), vec![]),
                (200, response().to_string(), vec![]),
            ]);
            let client = mock_client(&base, JevProvider::OpenRouter);
            let value = client.evaluate(json!("state"), questions()).await.unwrap();
            assert_eq!(value, response());
            let requests = worker.join().unwrap();
            assert_eq!(requests.len(), 3);
            assert!(requests.iter().all(|r| r.starts_with("POST /v1/decisions ")));
        }
    }

    #[tokio::test]
    async fn persistent_overload_fails_after_bounded_retries_without_echo() {
        for status in [429, 529] {
            let replies = (0..=TRANSIENT_RETRY_DELAYS.len())
                .map(|_| (status, "private-provider-error test-route-secret".into(), vec![]))
                .collect();
            let (base, worker) = mock_server(replies);
            let client = mock_client(&base, JevProvider::OpenRouter);
            let detail = format!(
                "{:#}",
                client.evaluate(json!("private-state"), questions()).await.unwrap_err()
            );
            assert!(detail.contains(&status.to_string()));
            assert!(detail.contains("overloaded"));
            assert!(!detail.contains("test-route-secret"));
            assert!(!detail.contains("private-state"));
            assert_eq!(worker.join().unwrap().len(), TRANSIENT_RETRY_DELAYS.len() + 1);
        }
    }

    #[tokio::test]
    async fn auth_billing_and_redirect_errors_are_redacted_and_never_retried() {
        for status in [401, 402, 403, 404, 500, 302, 307] {
            let headers = vec![("Location".into(), "http://127.0.0.1:1/never-follow".into())];
            let (base, worker) = mock_server(vec![(
                status,
                "private-provider-error test-route-secret".into(),
                headers,
            )]);
            let client = mock_client(&base, JevProvider::OpenRouter);
            let error = client.evaluate(json!("private-state"), questions()).await.unwrap_err();
            let detail = format!("{error:#}");
            assert!(detail.contains(&status.to_string()));
            assert!(!detail.contains("test-route-secret"));
            assert!(!detail.contains("private-provider-error"));
            assert!(!detail.contains("private-state"));
            assert_eq!(worker.join().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn oversized_and_invalid_json_responses_are_rejected_without_echo() {
        for body in [
            "x".repeat(MAX_RESPONSE_BYTES + 1),
            "private-response-invalid-json".into(),
        ] {
            let (base, worker) = mock_server(vec![(200, body, vec![])]);
            let client = mock_client(&base, JevProvider::TypeSafe);
            let error = client.evaluate(json!("state"), questions()).await.unwrap_err();
            assert!(!format!("{error:#}").contains("private-response"));
            worker.join().unwrap();
        }
    }

    #[tokio::test]
    async fn streamed_body_limit_is_enforced_without_content_length() {
        let (base, worker) = mock_server(vec![(
            200,
            "x".repeat(MAX_RESPONSE_BYTES + 1),
            vec![("Transfer-Encoding".into(), "chunked".into())],
        )]);
        let client = mock_client(&base, JevProvider::Aimlapi);
        let error = client.evaluate(json!("state"), questions()).await.unwrap_err();
        assert!(error.to_string().contains("bounded response size"));
        worker.join().unwrap();
    }
}
