//! `usage_report`: what this harness knows about token usage, and what it does not.
//!
//! The report is assembled from three sources, in decreasing authority:
//!
//! 1. The daemon's own cumulative `token_usage_totals` for the attached
//!    session, taken from its `state` snapshot. It owns the in-memory session
//!    and has already summed every request, including ones it has not flushed
//!    to disk yet.
//! 2. The session's stored original: the `token_usage` record on each stored
//!    message, in either the canonical event log or the preserved legacy JSON.
//! 3. The last live `token_usage` event, which describes exactly one provider
//!    call. It is the only figure available for a session whose record does
//!    not exist yet.
//!
//! Nothing here estimates or interpolates. A metric no request recorded stays
//! `None`; it never becomes zero, because "the provider did not report cache
//! tokens" and "the provider reported no cache tokens" are different facts and
//! a usage screen must not print the same number for both.
//!
//! `pricing` is `"unknown"` and `estimated_cost` is `null` for every report.
//! This repository carries no price table, so there is nothing to multiply
//! tokens by; a made-up rate would be worse than an honest gap.

use horon_agent_harness_api::{ApiEvent, ErrorCode, UsageModel, UsageSession, UsageTotals};
use serde::Deserialize;
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor, value::MapAccessDeserializer};
use serde_json::Value;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Label every report carries instead of a price table it does not have.
const PRICING_UNKNOWN: &str = "unknown";

/// Bucket for sessions whose metadata never named a model.
const MODEL_UNKNOWN: &str = "unknown";

#[path = "usage_report_local.rs"]
pub(super) mod local;

/// What the daemon has told this bridge about usage on this connection.
///
/// Kept per connection because every field here is session identity the daemon
/// pushes to one client, not a durable fact about the account.
#[derive(Debug, Default)]
pub(crate) struct Cache {
    /// Cumulative totals the daemon reported for the attached session.
    live_totals: Option<LiveTotals>,
    /// The last `token_usage` event: one provider call, not a session total.
    last_call: Option<LiveTotals>,
    /// Context window of the active route, from `model_changed`.
    context_window: Option<u64>,
}

#[derive(Debug)]
struct LiveTotals {
    session_id: String,
    model: Option<String>,
    sums: Sum,
}

impl Cache {
    /// Remember the daemon's cumulative totals from a `state` snapshot.
    ///
    /// The daemon omits the field when it has nothing to report, so an absent
    /// key keeps the previous value rather than clearing it to zero.
    pub(crate) fn note_state(&mut self, session_id: &str, event: &Value) {
        if session_id.is_empty() || !event["token_usage_totals"].is_object() {
            return;
        }
        let totals = &event["token_usage_totals"];
        self.live_totals = Some(LiveTotals {
            session_id: session_id.to_string(),
            model: event["provider_model"]
                .as_str()
                .filter(|model| *model != MODEL_UNKNOWN)
                .map(str::to_string),
            sums: Sum {
                input: totals["input_tokens"].as_u64(),
                output: totals["output_tokens"].as_u64(),
                cache_read: totals["cache_read_input_tokens"].as_u64(),
                cache_creation: totals["cache_creation_input_tokens"].as_u64(),
                requests: totals["messages_with_token_usage"].as_u64().unwrap_or(0),
            },
        });
    }

    /// Remember the active route's context window. A present-but-null key
    /// means the daemon no longer knows it, so the cache is cleared too.
    pub(crate) fn note_context_window(&mut self, event: &Value) {
        if event.get("context_window").is_some() {
            self.context_window = event["context_window"].as_u64();
        }
    }

    /// Remember the latest provider call for a session that may not be stored
    /// yet. This is a single request's usage, never added to anything else.
    pub(crate) fn note_call(&mut self, session_id: &str, event: &Value) {
        let session_id = event["session_id"].as_str().unwrap_or(session_id);
        if session_id.is_empty() {
            return;
        }
        self.last_call = Some(LiveTotals {
            session_id: session_id.to_string(),
            model: None,
            sums: Sum {
                input: event["input"].as_u64(),
                output: event["output"].as_u64(),
                cache_read: event["cache_read_input"].as_u64(),
                cache_creation: event["cache_creation_input"].as_u64(),
                requests: 1,
            },
        });
    }

    /// A different session attached: the route window no longer describes it.
    pub(crate) fn reset_identity(&mut self) {
        self.context_window = None;
    }
}

/// Running token sums where "unknown" stays unknown.
#[derive(Debug, Clone, Default)]
struct Sum {
    input: Option<u64>,
    output: Option<u64>,
    cache_read: Option<u64>,
    cache_creation: Option<u64>,
    requests: u64,
}

impl Sum {
    fn record(&mut self, usage: &StoredUsage) {
        self.requests += 1;
        add(&mut self.input, usage.input_tokens);
        add(&mut self.output, usage.output_tokens);
        add(&mut self.cache_read, usage.cache_read_input_tokens);
        add(&mut self.cache_creation, usage.cache_creation_input_tokens);
    }

    fn merge(&mut self, other: &Sum) {
        add(&mut self.input, other.input);
        add(&mut self.output, other.output);
        add(&mut self.cache_read, other.cache_read);
        add(&mut self.cache_creation, other.cache_creation);
        self.requests = self.requests.saturating_add(other.requests);
    }
}

fn add(slot: &mut Option<u64>, value: Option<u64>) {
    if let Some(value) = value {
        *slot = Some(slot.unwrap_or(0).saturating_add(value));
    }
}

/// The `token_usage` record a stored message may carry.
///
/// Every field is optional: records written before a metric existed, and
/// providers that never report one, both leave the field out.
#[derive(Debug, Clone, Copy, Deserialize)]
struct StoredUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
}

/// Durable usage read from one session's stored original.
#[derive(Debug, Default)]
struct Reading {
    model: Option<String>,
    sums: Sum,
}

#[derive(Debug)]
struct UsageRecord {
    usage: StoredUsage,
    draft: Option<local::GroupDraft>,
}

/// Build the report for one request on this connection.
///
/// A `usage_report` is answered locally from stored records and the live
/// figures the daemon pushed; it never sends a request of its own. This build
/// carries no subscription, so the limit list is always empty.
pub(crate) fn reply(state: &mut super::BridgeState, request: &Value, api_id: u64) -> Vec<super::Outbound> {
    build_reply(state, request, api_id)
}

fn build_reply(state: &super::BridgeState, request: &Value, api_id: u64) -> Vec<super::Outbound> {
    match build(
        request,
        state.session_id.as_deref(),
        &state.known_sessions,
        &state.usage,
    ) {
        Ok(event) => vec![super::Outbound::Reply(super::ServerFrame::reply(api_id, event))],
        Err((code, message)) => super::BridgeState::error_reply(api_id, code, &message),
    }
}

/// The `token_usage` event, after caching the call for `usage_report`.
pub(crate) fn token_usage_event(cache: &mut Cache, session_id: &str, event: &Value) -> ApiEvent {
    cache.note_call(session_id, event);
    ApiEvent::TokenUsage {
        session_id: session_id.to_string(),
        input: event["input"].as_u64().unwrap_or(0),
        output: event["output"].as_u64().unwrap_or(0),
        cache_read_input: event["cache_read_input"].as_u64(),
        cache_creation_input: event["cache_creation_input"].as_u64(),
    }
}

/// Build the report.
///
/// `attached` is the session this connection is attached to, `known` every
/// session id the daemon has mentioned, and `cache` the live figures this
/// connection has seen. A requested session id must name one of those or an
/// existing stored record; anything else is `unknown_session`.
fn build(
    request: &Value,
    attached: Option<&str>,
    known: &[String],
    cache: &Cache,
) -> Result<ApiEvent, (ErrorCode, String)> {
    let home = crate::translate::BridgeState::horon_agent_home();
    let requested = request["session_id"].as_str().filter(|id| !id.is_empty());

    let ids = match requested {
        Some(id) => {
            if !valid_id(id) {
                return Err((ErrorCode::InvalidRequest, "invalid session_id".into()));
            }
            if !exists(home.as_deref(), id, attached, known) {
                return Err((ErrorCode::UnknownSession, format!("no session `{id}`")));
            }
            vec![id.to_string()]
        }
        None => enumerate(home.as_deref(), attached, known),
    };

    let mut sessions = Vec::with_capacity(ids.len());
    let mut totals = Sum::default();
    let mut models: BTreeMap<String, (Sum, BTreeSet<String>)> = BTreeMap::new();
    for id in ids {
        let reading = read_session(home.as_deref(), &id);
        let (sums, model) = resolve(&id, reading, attached, cache);
        totals.merge(&sums);
        sessions.push(UsageSession {
            session_id: id.clone(),
            model: model.clone(),
            input: sums.input,
            output: sums.output,
            cache_read_input: sums.cache_read,
            cache_creation_input: sums.cache_creation,
            requests: sums.requests,
        });
        let entry = models
            .entry(model.unwrap_or_else(|| MODEL_UNKNOWN.to_string()))
            .or_default();
        entry.0.merge(&sums);
        entry.1.insert(id);
    }

    let models = models
        .into_iter()
        .map(|(model, (sums, sessions))| UsageModel {
            model,
            input: sums.input,
            output: sums.output,
            cache_read_input: sums.cache_read,
            cache_creation_input: sums.cache_creation,
            requests: sums.requests,
            sessions: sessions.len(),
        })
        .collect();
    let sessions_count = sessions.len();
    let context_window = match (requested, attached) {
        // A single-session report for some other session must not show the
        // attached route's window as if it were that session's.
        (Some(id), Some(attached)) if id != attached => None,
        _ => cache.context_window,
    };

    Ok(ApiEvent::UsageReport {
        session_id: requested.map(str::to_string),
        sessions,
        totals: UsageTotals {
            input: totals.input,
            output: totals.output,
            cache_read_input: totals.cache_read,
            cache_creation_input: totals.cache_creation,
            requests: totals.requests,
            sessions: sessions_count,
        },
        models,
        context_window,
        limits: Vec::new(),
        pricing: PRICING_UNKNOWN.into(),
        estimated_cost: None,
    })
}

/// Pick the most authoritative figure for one session.
fn resolve(id: &str, reading: Reading, attached: Option<&str>, cache: &Cache) -> (Sum, Option<String>) {
    let is_attached = attached == Some(id);
    let live = cache.live_totals.as_ref().filter(|totals| totals.session_id == id);
    let call = cache.last_call.as_ref().filter(|call| call.session_id == id);
    if is_attached && let Some(live) = live {
        return (live.sums.clone(), live.model.clone().or(reading.model));
    }
    if reading.sums.requests > 0 {
        return (reading.sums, reading.model);
    }
    if is_attached && let Some(call) = call {
        return (call.sums.clone(), call.model.clone().or(reading.model));
    }
    (reading.sums, reading.model)
}

fn valid_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

fn sessions_root(home: &Path) -> PathBuf {
    home.join("sessions")
}

fn exists(home: Option<&Path>, id: &str, attached: Option<&str>, known: &[String]) -> bool {
    if attached == Some(id) || known.iter().any(|known| known == id) {
        return true;
    }
    let Some(home) = home else { return false };
    let root = sessions_root(home);
    root.join(id).join("events.jsonl").is_file() || root.join(format!("{id}.json")).is_file()
}

/// Every session this bridge can see: stored originals first, then the ids the
/// daemon mentioned, then the attached one.
fn enumerate(home: Option<&Path>, attached: Option<&str>, known: &[String]) -> Vec<String> {
    let mut ids: BTreeSet<String> = BTreeSet::new();
    if let Some(home) = home
        && let Ok(entries) = std::fs::read_dir(sessions_root(home))
    {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.join("events.jsonl").is_file()
                    && let Some(id) = path.file_name().and_then(|name| name.to_str())
                {
                    ids.insert(id.to_string());
                }
            } else if path.extension().is_some_and(|extension| extension == "json")
                && let Some(id) = path.file_stem().and_then(|name| name.to_str())
            {
                ids.insert(id.to_string());
            }
        }
    }
    ids.extend(known.iter().cloned());
    if let Some(attached) = attached {
        ids.insert(attached.to_string());
    }
    ids.into_iter().filter(|id| valid_id(id)).collect()
}

fn read_session(home: Option<&Path>, session_id: &str) -> Reading {
    let Some(home) = home else {
        return Reading::default();
    };
    let root = sessions_root(home);
    let canonical = root.join(session_id).join("events.jsonl");
    if canonical.is_file() {
        return read_canonical(&canonical, None);
    }
    read_legacy(&root.join(format!("{session_id}.json")), None)
}

pub(super) enum LocalSession {
    Canonical(PathBuf),
    Legacy(PathBuf),
}

/// Discover local session storage paths in one pass without redundant stat calls.
pub(super) fn enumerate_local_sessions(root: &Path) -> Vec<LocalSession> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut by_id: HashMap<String, LocalSession> = HashMap::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        let is_dir = if file_type.is_symlink() {
            path.is_dir()
        } else {
            file_type.is_dir()
        };
        if is_dir {
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                continue;
            };
            if !valid_id(name) {
                continue;
            }
            let canonical = path.join("events.jsonl");
            if canonical.is_file() {
                by_id.insert(name.to_owned(), LocalSession::Canonical(canonical));
            }
        } else if path.extension().is_some_and(|extension| extension == "json")
            && let Some(id) = path.file_stem().and_then(|name| name.to_str())
            && valid_id(id)
        {
            by_id.entry(id.to_owned()).or_insert(LocalSession::Legacy(path));
        }
    }
    by_id.into_values().collect()
}

/// Lenient borrowed-or-owned string: ignores non-string JSON values.
#[derive(Default)]
struct LenientStr<'a>(Option<Cow<'a, str>>);

impl<'a> LenientStr<'a> {
    fn as_deref(&self) -> Option<&str> {
        self.0.as_deref()
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for LenientStr<'a> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct StrVisitor;
        impl<'de> Visitor<'de> for StrVisitor {
            type Value = LenientStr<'de>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an optional string")
            }

            fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E> {
                Ok(LenientStr(Some(Cow::Borrowed(value))))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
                Ok(LenientStr(Some(Cow::Owned(value.to_owned()))))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(LenientStr(Some(Cow::Owned(value))))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(LenientStr(None))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(LenientStr(None))
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientStr(None))
            }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientStr(None))
            }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientStr(None))
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientStr(None))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientStr(None))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(LenientStr(None))
            }
        }
        deserializer.deserialize_any(StrVisitor)
    }
}

enum NullableU64 {
    Null,
    Value(u64),
    Invalid,
}

impl<'de> Deserialize<'de> for NullableU64 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct U64Visitor;
        impl<'de> Visitor<'de> for U64Visitor {
            type Value = NullableU64;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an optional u64")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
                Ok(NullableU64::Value(value))
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
                Ok(match u64::try_from(value) {
                    Ok(v) => NullableU64::Value(v),
                    Err(_) => NullableU64::Invalid,
                })
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(NullableU64::Null)
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(NullableU64::Null)
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(NullableU64::Invalid)
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(NullableU64::Invalid)
            }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(NullableU64::Invalid)
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(NullableU64::Invalid)
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(NullableU64::Invalid)
            }
        }
        deserializer.deserialize_any(U64Visitor)
    }
}

#[derive(Default)]
struct TokenUsageField(Option<StoredUsage>);

impl<'de> Deserialize<'de> for TokenUsageField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct FieldVisitor;
        impl<'de> Visitor<'de> for FieldVisitor {
            type Value = TokenUsageField;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a token_usage object")
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(TokenUsageField(None))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(TokenUsageField(None))
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(TokenUsageField(None))
            }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(TokenUsageField(None))
            }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(TokenUsageField(None))
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(TokenUsageField(None))
            }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(TokenUsageField(None))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(TokenUsageField(None))
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut out = StoredUsage {
                    input_tokens: None,
                    output_tokens: None,
                    cache_read_input_tokens: None,
                    cache_creation_input_tokens: None,
                };
                let mut valid = true;
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    if !valid {
                        map.next_value::<IgnoredAny>()?;
                        continue;
                    }
                    let slot = match key.as_ref() {
                        "input_tokens" => &mut out.input_tokens,
                        "output_tokens" => &mut out.output_tokens,
                        "cache_read_input_tokens" => &mut out.cache_read_input_tokens,
                        "cache_creation_input_tokens" => &mut out.cache_creation_input_tokens,
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                            continue;
                        }
                    };
                    match map.next_value::<NullableU64>()? {
                        NullableU64::Null => *slot = None,
                        NullableU64::Value(v) => *slot = Some(v),
                        NullableU64::Invalid => valid = false,
                    }
                }
                Ok(TokenUsageField(if valid { Some(out) } else { None }))
            }
        }
        deserializer.deserialize_any(FieldVisitor)
    }
}

#[derive(Default)]
struct LenientMap<T>(T);

impl<'de, T: Default + Deserialize<'de>> Deserialize<'de> for LenientMap<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct MapOrDefaultVisitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: Default + Deserialize<'de>> Visitor<'de> for MapOrDefaultVisitor<T> {
            type Value = LenientMap<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an optional object")
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(LenientMap(T::default()))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(LenientMap(T::default()))
            }

            fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientMap(T::default()))
            }

            fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientMap(T::default()))
            }

            fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientMap(T::default()))
            }

            fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientMap(T::default()))
            }

            fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientMap(T::default()))
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientMap(T::default()))
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                T::deserialize(MapAccessDeserializer::new(map)).map(LenientMap)
            }
        }
        deserializer.deserialize_any(MapOrDefaultVisitor(std::marker::PhantomData))
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CanonicalMetadata<'a> {
    #[serde(borrow)]
    model: LenientStr<'a>,
    #[serde(borrow)]
    provider_key: LenientStr<'a>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CanonicalMessage<'a> {
    #[serde(borrow)]
    timestamp: LenientStr<'a>,
    token_usage: TokenUsageField,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct CanonicalPayload<'a> {
    #[serde(borrow)]
    metadata: LenientMap<CanonicalMetadata<'a>>,
    #[serde(borrow)]
    message: LenientMap<CanonicalMessage<'a>>,
    #[serde(borrow)]
    target_event_id: LenientStr<'a>,
    #[serde(borrow)]
    replacement: LenientMap<CanonicalMessage<'a>>,
}

#[derive(Deserialize)]
#[serde(default)]
struct CanonicalEvent<'a> {
    #[serde(borrow)]
    kind: LenientStr<'a>,
    #[serde(borrow)]
    event_id: LenientStr<'a>,
    #[serde(borrow)]
    timestamp: LenientStr<'a>,
    #[serde(borrow)]
    payload: LenientMap<CanonicalPayload<'a>>,
}

impl Default for CanonicalEvent<'_> {
    fn default() -> Self {
        Self {
            kind: LenientStr(None),
            event_id: LenientStr(None),
            timestamp: LenientStr(None),
            payload: LenientMap(CanonicalPayload::default()),
        }
    }
}

/// Read the canonical event log.
///
/// Usage is keyed by the append event's id so a revision replaces the message
/// it supersedes instead of counting a second time. A report can cover every
/// session on the machine, so a line without a usage record is skipped before
/// it is parsed: transcripts are mostly message text and tool payloads, and
/// deserializing all of it would make a usage screen cost seconds.
fn read_canonical(path: &Path, mut groups: Option<&mut local::Groups>) -> Reading {
    let Ok(file) = std::fs::File::open(path) else {
        return Reading::default();
    };
    let mut reader = BufReader::new(file);
    let mut line = String::with_capacity(512);
    let mut reading = Reading::default();
    let mut usage: HashMap<Rc<str>, UsageRecord> = HashMap::new();
    let mut model_rc: Option<Rc<str>> = None;
    let mut provider_rc: Option<Rc<str>> = None;
    let mut seen_events: HashSet<Rc<str>> = HashSet::new();
    let track_groups = groups.is_some();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if !(line.contains("\"token_usage\"")
            || line.contains("\"model\"")
            || line.contains("\"message_revised\"")
            || (track_groups && line.contains("\"provider_key\"")))
        {
            continue;
        }
        let Ok(event) = serde_json::from_str::<CanonicalEvent<'_>>(&line) else {
            continue;
        };
        let kind = event.kind.as_deref();
        let event_key = if matches!(kind, Some("message_appended" | "message_revised"))
            && let Some(id) = event.event_id.as_deref()
        {
            if seen_events.contains(id) {
                continue;
            }
            let key: Rc<str> = Rc::from(id);
            seen_events.insert(Rc::clone(&key));
            Some(key)
        } else {
            None
        };
        match kind {
            Some("session_created" | "metadata_changed") => {
                if let Some(model) = event
                    .payload
                    .0
                    .metadata
                    .0
                    .model
                    .as_deref()
                    .filter(|model| *model != MODEL_UNKNOWN)
                    && model_rc.as_deref() != Some(model)
                {
                    model_rc = Some(Rc::from(model));
                }
                if track_groups && let Some(value) = event.payload.0.metadata.0.provider_key.as_deref() {
                    let normalized = (!value.is_empty()).then_some(value);
                    if provider_rc.as_deref() != normalized {
                        provider_rc = normalized.map(Rc::from);
                    }
                }
            }
            Some("message_appended") => {
                let Some(key) = event_key else {
                    continue;
                };
                let message = &event.payload.0.message.0;
                if let Some(value) = message.token_usage.0 {
                    usage.entry(key).or_insert_with(|| UsageRecord {
                        usage: value,
                        draft: track_groups.then(|| local::GroupDraft {
                            day: local::parse_day_index(
                                message.timestamp.as_deref().or_else(|| event.timestamp.as_deref()),
                            ),
                            provider: provider_rc.clone(),
                            model: model_rc.clone(),
                        }),
                    });
                }
            }
            Some("message_revised") => {
                let Some(target) = event.payload.0.target_event_id.as_deref() else {
                    continue;
                };
                let message = &event.payload.0.replacement.0;
                match message.token_usage.0 {
                    Some(value) => {
                        if let Some(record) = usage.get_mut(target) {
                            record.usage = value;
                            if let Some(draft) = record.draft.as_mut() {
                                if let Some(replacement_ts) = message.timestamp.as_deref() {
                                    draft.day = local::parse_day_index(Some(replacement_ts));
                                }
                                draft.provider = provider_rc.clone();
                                draft.model = model_rc.clone();
                            }
                        } else {
                            let revised_ts = message.timestamp.as_deref().or_else(|| event.timestamp.as_deref());
                            let key: Rc<str> = Rc::from(target);
                            usage.insert(
                                key,
                                UsageRecord {
                                    usage: value,
                                    draft: track_groups.then(|| local::GroupDraft {
                                        day: local::parse_day_index(revised_ts),
                                        provider: provider_rc.clone(),
                                        model: model_rc.clone(),
                                    }),
                                },
                            );
                        }
                    }
                    None => {
                        usage.remove(target);
                    }
                }
            }
            _ => {}
        }
    }
    reading.model = model_rc.map(|model| model.to_string());
    for record in usage.into_values() {
        reading.sums.record(&record.usage);
        if let (Some(draft), Some(groups)) = (record.draft, groups.as_deref_mut()) {
            groups.record_draft(draft, &record.usage);
        }
    }
    reading
}

/// Read the preserved legacy snapshot plus its append journal.
///
/// Messages are keyed by message id so a message that appears in both the
/// snapshot and the journal is counted once.
fn read_legacy(path: &Path, mut groups: Option<&mut local::Groups>) -> Reading {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Reading::default();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Reading::default();
    };
    let mut reading = Reading::default();
    if let Some(model) = value["model"].as_str().filter(|model| *model != MODEL_UNKNOWN) {
        reading.model = Some(model.to_string());
    }
    let track_groups = groups.is_some();
    let provider_rc = track_groups
        .then(|| value["provider_key"].as_str().filter(|v| !v.is_empty()).map(Rc::from))
        .flatten();
    let model_rc = track_groups.then(|| reading.model.as_deref().map(Rc::from)).flatten();
    let mut usage: HashMap<String, UsageRecord> = HashMap::new();
    let draft = |message: &Value| {
        track_groups.then(|| local::GroupDraft {
            day: local::parse_day_index(message["timestamp"].as_str()),
            provider: provider_rc.clone(),
            model: model_rc.clone(),
        })
    };
    collect_messages(&value["messages"], "snapshot", &mut usage, &draft);
    let journal = path.with_extension("journal.jsonl");
    if let Ok(lines) = std::fs::read_to_string(&journal) {
        for (index, line) in lines.lines().enumerate() {
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            collect_messages(
                &entry["append_messages"],
                &format!("journal{index}"),
                &mut usage,
                &draft,
            );
        }
    }
    for record in usage.into_values() {
        reading.sums.record(&record.usage);
        if let (Some(draft), Some(groups)) = (record.draft, groups.as_deref_mut()) {
            groups.record_draft(draft, &record.usage);
        }
    }
    reading
}

fn collect_messages(
    messages: &Value,
    source: &str,
    usage: &mut HashMap<String, UsageRecord>,
    draft: &impl Fn(&Value) -> Option<local::GroupDraft>,
) {
    let Some(messages) = messages.as_array() else {
        return;
    };
    for (index, message) in messages.iter().enumerate() {
        let key = message["id"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| format!("{source}:{index}"));
        match <StoredUsage as Deserialize>::deserialize(&message["token_usage"]) {
            Ok(value) => {
                usage.insert(
                    key,
                    UsageRecord {
                        usage: value,
                        draft: draft(message),
                    },
                );
            }
            Err(_) => {
                usage.remove(&key);
            }
        }
    }
}
