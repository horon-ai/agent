//! Credential obfuscation with a session-scoped round trip.
//!
//! [`crate::message::redact_secrets`] protects the disk one way: it rewrites a
//! credential into `[REDACTED_SECRET]` and never keeps the original. This module
//! keeps a reversible placeholder (`«SECRET:<label>:<base>»`) for the wire, so
//! the provider reads the placeholder while the tool that needs the value gets
//! the real bytes back. The mapping lives only in process memory, but the
//! placeholder's `<base>` is derived from a key file that outlives the process:
//! a resumed transcript still resolves its placeholders after a restart, and a
//! session vault is pre-seeded with the credentials this process inherited from
//! its environment.
//!
//! Detection is not a second rule set: [`rules`] holds the one pattern table, and
//! `redact_secrets` reads it too, so a pattern added for the disk path is also a
//! pattern for the wire path.
//!
//! The dispatch order is the invariant: placeholders are restored *before* the
//! `pre_tool` gate runs, so an approver reviews the command that will actually
//! execute. [`restore_then_gate`] keeps both steps in one unit so a call site
//! cannot reorder them, and the registry calls it rather than restoring by hand.

use crate::hooks::GateDecision;
use crate::message::{ContentBlock, Message};
use horon_agent_config_types::SecretsMode;
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// The one credential-pattern table, shared with `redact_secrets`.
pub(crate) mod rules {
    use super::*;

    /// Whole-match token shapes, paired with the label their placeholders carry.
    pub(crate) const DIRECT: &[(&str, &str)] = &[
        ("anthropic", r"sk-ant-(?:oat|ort)01-[A-Za-z0-9_-]{20,}"),
        ("openrouter", r"sk-or-v1-[A-Za-z0-9_-]{20,}"),
        ("github", r"ghp_[A-Za-z0-9]{20,}"),
        ("github_pat", r"github_pat_[A-Za-z0-9_]{20,}"),
        ("google_oauth", r"ya29\.[A-Za-z0-9._-]{20,}"),
        ("google_api", r"AIza[0-9A-Za-z_-]{20,}"),
        ("slack", r"xox[baprs]-[A-Za-z0-9-]{10,}"),
        ("aws", r"AKIA[0-9A-Z]{16}"),
        ("bearer", r"(?i)Bearer\s+[A-Za-z0-9._~+/=-]{20,}"),
        ("jwt", r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}"),
        (
            "private_key",
            r"(?s)-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----.*?-----END (?:RSA |EC |OPENSSH )?PRIVATE KEY-----",
        ),
    ];

    /// `KEY=VALUE` shapes: group 1 is the key prefix, group 2 the credential.
    pub(crate) const ASSIGNMENT: &[&str] = &[
        r"(?m)^\s*(OPENROUTER_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(CONIFER_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(OPENCODE_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(OPENCODE_GO_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(ZHIPU_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(ZAI_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(302AI_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(BASETEN_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(CORTECS_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(DEEPSEEK_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(FIRMWARE_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(HF_TOKEN\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(MOONSHOT_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(NEBIUS_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(SCALEWAY_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(STACKIT_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(GROQ_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(MISTRAL_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(PERPLEXITY_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(TOGETHER_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(DEEPINFRA_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(XAI_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(LMSTUDIO_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(OLLAMA_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(CHUTES_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(CEREBRAS_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(BELVEDIR_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(OPENAI_COMPAT_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(ANTHROPIC_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(OPENAI_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(AZURE_OPENAI_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(CURSOR_API_KEY\s*=\s*)([^\r\n]+)",
        r"(?m)^\s*(GITHUB_TOKEN\s*=\s*)([^\r\n]+)",
        r"(?im)^\s*([A-Z][A-Z0-9_]*(?:API_KEY|TOKEN|SECRET|PASSWORD|COOKIE)\s*=\s*)([^\r\n]+)",
        r"(?im)^\s*(AUTHORIZATION\s*[:=]\s*)([^\r\n]+)",
    ];

    /// Key names [`ASSIGNMENT`] already covers, so a runtime key name is not
    /// compiled twice.
    pub(crate) const STATIC_ASSIGNMENT_KEYS: &[&str] = &[
        "OPENROUTER_API_KEY",
        "CONIFER_API_KEY",
        "OPENCODE_API_KEY",
        "OPENCODE_GO_API_KEY",
        "ZHIPU_API_KEY",
        "ZAI_API_KEY",
        "302AI_API_KEY",
        "BASETEN_API_KEY",
        "CORTECS_API_KEY",
        "DEEPSEEK_API_KEY",
        "FIRMWARE_API_KEY",
        "HF_TOKEN",
        "MOONSHOT_API_KEY",
        "NEBIUS_API_KEY",
        "SCALEWAY_API_KEY",
        "STACKIT_API_KEY",
        "GROQ_API_KEY",
        "MISTRAL_API_KEY",
        "PERPLEXITY_API_KEY",
        "TOGETHER_API_KEY",
        "DEEPINFRA_API_KEY",
        "XAI_API_KEY",
        "LMSTUDIO_API_KEY",
        "OLLAMA_API_KEY",
        "CHUTES_API_KEY",
        "CEREBRAS_API_KEY",
        "BELVEDIR_API_KEY",
        "OPENAI_COMPAT_API_KEY",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "AZURE_OPENAI_API_KEY",
        "CURSOR_API_KEY",
        "GITHUB_TOKEN",
    ];

    pub(crate) fn direct_regexes() -> &'static [(&'static str, Regex)] {
        static COMPILED: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
        COMPILED.get_or_init(|| {
            DIRECT
                .iter()
                .filter_map(|(label, pattern)| Regex::new(pattern).ok().map(|regex| (*label, regex)))
                .collect()
        })
    }

    pub(crate) fn assignment_regexes() -> &'static [Regex] {
        static COMPILED: OnceLock<Vec<Regex>> = OnceLock::new();
        COMPILED.get_or_init(|| {
            ASSIGNMENT
                .iter()
                .filter_map(|pattern| Regex::new(pattern).ok())
                .collect()
        })
    }

    /// Assignment shapes for the API key names configured at runtime.
    pub(crate) fn custom_assignment_regexes() -> Vec<(String, Regex)> {
        let mut compiled = Vec::new();
        let mut seen: std::collections::HashSet<String> =
            STATIC_ASSIGNMENT_KEYS.iter().map(|key| (*key).to_string()).collect();
        for source in [
            "HORON_AGENT_OPENROUTER_API_KEY_NAME",
            "HORON_AGENT_OPENAI_COMPAT_API_KEY_NAME",
        ] {
            let Some(key_name) = std::env::var(source)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            if !key_name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                crate::logging::warn(&format!("ignoring invalid custom secret key name from {source}"));
                continue;
            }
            if !seen.insert(key_name.clone()) {
                continue;
            }
            let pattern = format!(r"(?m)^\s*({}\s*=\s*)([^\r\n]+)", regex::escape(&key_name));
            if let Ok(regex) = Regex::new(&pattern) {
                crate::logging::debug(&format!("adding custom secret pattern for key={key_name}"));
                compiled.push((key_name, regex));
            }
        }
        compiled
    }
}

/// Cheap pre-check so the regex table is only walked for text that could hold a
/// credential. The shapes named here mirror [`rules::DIRECT`].
pub(crate) fn may_contain_secret(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    text.contains("sk-")
        || text.contains("ghp_")
        || text.contains("github_pat_")
        || text.contains("AIza")
        || text.contains("ya29.")
        || text.contains("xox")
        || text.contains("AKIA")
        || text.contains("-----BEGIN ")
        || text.contains("eyJ")
        || lower.contains("api_key")
        || lower.contains("token")
        || lower.contains("bearer ")
        || lower.contains("password")
        || lower.contains("secret")
        || lower.contains("authorization")
        || lower.contains("cookie")
}

/// File in the horon-agent home that holds the placeholder HMAC key.
const PLACEHOLDER_KEY_FILE: &str = "secret-placeholder.key";
/// Digits used for the placeholder suffix, emitted least-significant first.
const PLACEHOLDER_BASE_ALPHABET: &[u8; 36] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// The process-wide placeholder key, loaded from [`PLACEHOLDER_KEY_FILE`] on
/// first use.
static PLACEHOLDER_KEY: std::sync::LazyLock<[u8; 32]> = std::sync::LazyLock::new(|| {
    let loaded = placeholder_key_path().and_then(|path| load_or_create_key(&path));
    match loaded {
        Ok(key) => key,
        Err(err) => {
            crate::logging::warn(&format!(
                "secret placeholder key not persisted: {err}; placeholders will not survive a restart"
            ));
            random_key()
        }
    }
});

/// Environment variable names whose values are treated as credentials when a
/// session vault is pre-seeded. The plan's pattern is a plain alternation, so
/// `PASS` also matches names like `COMPASS`.
static ENV_SECRET_NAME: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"(?i)KEY|SECRET|TOKEN|PASSWORD|PASS|AUTH|CREDENTIAL|PRIVATE").expect("static regex")
});

fn random_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut key);
    key
}

/// Path of the persistent placeholder key, creating its directory when missing.
fn placeholder_key_path() -> std::io::Result<PathBuf> {
    let dir = crate::storage::horon_agent_dir().map_err(std::io::Error::other)?;
    std::fs::create_dir_all(&dir)?;
    if let Err(err) = horon_agent_platform::fs::set_dir_permissions_owner_only(&dir) {
        crate::logging::warn(&format!("could not tighten permissions on {}: {err}", dir.display()));
    }
    Ok(dir.join(PLACEHOLDER_KEY_FILE))
}

/// Read the placeholder key at `path`, creating it with a fresh random key when
/// it does not exist yet.
///
/// Split from [`placeholder_key`] so a test can exercise the file round trip and
/// the derivation against an explicit key instead of the process-wide one.
fn load_or_create_key(path: &Path) -> std::io::Result<[u8; 32]> {
    use base64::Engine as _;

    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    horon_agent_platform::fs::request_owner_only_mode(&mut options);

    match options.open(path) {
        Ok(mut file) => {
            let key = random_key();
            file.write_all(engine.encode(key).as_bytes())?;
            file.sync_all()?;
            horon_agent_platform::fs::set_file_permissions_owner_only(path)?;
            Ok(key)
        }
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
            let encoded = std::fs::read_to_string(path)?;
            let bytes = engine
                .decode(encoded.trim())
                .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
            let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "placeholder key file must hold 32 bytes",
                )
            })?;
            Ok(key)
        }
        Err(err) => Err(err),
    }
}

/// The key that makes placeholders stable across restarts.
fn placeholder_key() -> &'static [u8; 32] {
    &PLACEHOLDER_KEY
}

/// The 12-character placeholder suffix for `secret`, derived from `key`.
///
/// HMAC-SHA256 with a per-machine key means the provider never sees the value,
/// the same value always renders the same suffix, and two different credentials
/// practically never collide. Digits are emitted least-significant first.
fn placeholder_base(key: &[u8; 32], secret: &str) -> String {
    use hmac::{Hmac, Mac as _};
    use sha2::Sha256;

    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(secret.as_bytes());
    let digest = mac.finalize().into_bytes();
    let mut value = u64::from_be_bytes(digest[..8].try_into().expect("8 digest bytes"));
    let mut out = String::with_capacity(12);
    for _ in 0..12 {
        out.push(PLACEHOLDER_BASE_ALPHABET[(value % 36) as usize] as char);
        value /= 36;
    }
    out
}

/// Real credential → placeholder, plus the reverse, for one session.
///
/// The mapping lives only in process memory. `Debug` prints counts instead of
/// values, so a log line or crash bundle cannot leak a credential through the
/// vault itself.
#[derive(Default, Clone)]
pub struct SecretVault {
    forward: HashMap<String, String>,
    reverse: HashMap<String, String>,
}

impl std::fmt::Debug for SecretVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretVault")
            .field("entries", &self.forward.len())
            .finish_non_exhaustive()
    }
}

impl SecretVault {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.forward.is_empty()
    }

    pub fn len(&self) -> usize {
        self.forward.len()
    }

    pub fn clear(&mut self) {
        self.forward.clear();
        self.reverse.clear();
    }

    /// Map every credential this process inherited from its environment, so a
    /// resumed transcript's placeholders for those values restore after a
    /// restart even before any tool output re-detects them.
    fn preseed_env(&mut self) {
        for (name, value) in std::env::vars_os() {
            let (Some(name), Some(value)) = (name.to_str(), value.to_str()) else {
                continue;
            };
            if value.chars().count() < 8 || !ENV_SECRET_NAME.is_match(name) {
                continue;
            }
            self.placeholder_for(value, "env");
        }
    }

    /// The same real value always yields the same placeholder, so a repeated
    /// credential cannot shift the cached prompt prefix, and a restarted process
    /// (holding the same key file) renders the same placeholder again.
    fn placeholder_for(&mut self, secret: &str, label: &str) -> String {
        if let Some(existing) = self.forward.get(secret) {
            return existing.clone();
        }
        let label = sanitize_label(label);
        let placeholder = format!("«SECRET:{label}:{}»", placeholder_base(placeholder_key(), secret));
        self.forward.insert(secret.to_string(), placeholder.clone());
        self.reverse.insert(placeholder.clone(), secret.to_string());
        placeholder
    }
}

fn sanitize_label(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('_') {
            out.push('_');
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "secret".to_string()
    } else {
        trimmed
    }
}

/// Already-masked text must never be masked again: a later pattern in the table
/// would otherwise wrap a placeholder in a second placeholder, and restoring the
/// outer one would only yield the inner one back.
fn is_placeholder(text: &str) -> bool {
    text.contains("«SECRET:")
}

/// Replace every detected credential in `text` with a stable placeholder.
///
/// `extra` carries caller-supplied patterns; their matches are labelled
/// `extra<n>` so they never collide with the shared table's labels.
pub fn obfuscate(text: &str, vault: &mut SecretVault, extra: &[Regex]) -> String {
    if !may_contain_secret(text) {
        return text.to_string();
    }

    let mut out = text.to_string();
    for (label, regex) in rules::direct_regexes() {
        out = replace_whole(&out, regex, label, vault);
    }
    for regex in rules::assignment_regexes() {
        out = replace_group(&out, regex, 2, None, vault);
    }
    for (key_name, regex) in rules::custom_assignment_regexes() {
        out = replace_group(&out, &regex, 2, Some(&key_name), vault);
    }
    for (index, regex) in extra.iter().enumerate() {
        out = replace_whole(&out, regex, &format!("extra{}", index + 1), vault);
    }
    out
}

/// Replace every placeholder in `text` with the value it stands for.
pub fn restore(text: &str, vault: &SecretVault) -> String {
    if vault.reverse.is_empty() {
        return text.to_string();
    }
    let mut entries: Vec<(&String, &String)> = vault.reverse.iter().collect();
    entries.sort_by(|left, right| right.0.len().cmp(&left.0.len()).then_with(|| left.0.cmp(right.0)));
    let mut out = text.to_string();
    for (placeholder, secret) in entries {
        if out.contains(placeholder.as_str()) {
            out = out.replace(placeholder.as_str(), secret);
        }
    }
    out
}

/// Labels of the credentials [`obfuscate`] would replace.
pub fn detect(text: &str) -> Vec<String> {
    if !may_contain_secret(text) {
        return Vec::new();
    }
    let mut found: Vec<(String, String)> = Vec::new();
    let mut record = |label: String, value: &str| {
        if value.trim().is_empty() || is_placeholder(value) {
            return;
        }
        if found.iter().any(|(_, seen)| seen == value) {
            return;
        }
        found.push((label, value.to_string()));
    };

    for (label, regex) in rules::direct_regexes() {
        for caps in regex.captures_iter(text) {
            if let Some(value) = caps.get(0) {
                record((*label).to_string(), value.as_str());
            }
        }
    }
    for regex in rules::assignment_regexes() {
        for caps in regex.captures_iter(text) {
            if let (Some(key), Some(value)) = (caps.get(1), caps.get(2)) {
                record(
                    sanitize_label(key.as_str().trim_end_matches(['=', ' ', ':'])),
                    value.as_str(),
                );
            }
        }
    }
    for (key_name, regex) in rules::custom_assignment_regexes() {
        for caps in regex.captures_iter(text) {
            if let Some(value) = caps.get(2) {
                record(sanitize_label(&key_name), value.as_str());
            }
        }
    }
    found.into_iter().map(|(label, _)| label).collect()
}

/// The named refusal returned by [`SecretsMode::Block`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretBlocked {
    pub count: usize,
    pub labels: Vec<String>,
}

impl std::fmt::Display for SecretBlocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SECRET_BLOCK: refusing to send {} credential(s) to the provider ({}); \
             set [tools] secrets_mode = \"obfuscate\" to send stable placeholders instead",
            self.count,
            self.labels.join(", ")
        )
    }
}

impl std::error::Error for SecretBlocked {}

/// Apply the configured mode to one outbound string.
pub fn apply_outbound(
    text: &str,
    vault: &mut SecretVault,
    mode: SecretsMode,
    extra: &[Regex],
) -> Result<String, SecretBlocked> {
    match mode {
        SecretsMode::Off => Ok(text.to_string()),
        SecretsMode::Obfuscate => Ok(obfuscate(text, vault, extra)),
        SecretsMode::Block => {
            let labels = detect(text);
            if labels.is_empty() {
                Ok(text.to_string())
            } else {
                Err(SecretBlocked {
                    count: labels.len(),
                    labels,
                })
            }
        }
    }
}

/// Apply the configured mode to every string a provider would read.
pub fn obfuscate_messages(
    messages: &[Message],
    vault: &mut SecretVault,
    mode: SecretsMode,
    extra: &[Regex],
) -> Result<Vec<Message>, SecretBlocked> {
    if mode == SecretsMode::Off {
        return Ok(messages.to_vec());
    }
    if mode == SecretsMode::Block {
        let mut labels = Vec::new();
        for message in messages {
            for block in &message.content {
                labels.extend(detect_block(block));
            }
        }
        if !labels.is_empty() {
            return Err(SecretBlocked {
                count: labels.len(),
                labels,
            });
        }
        return Ok(messages.to_vec());
    }

    let mut out = messages.to_vec();
    for message in &mut out {
        for block in &mut message.content {
            obfuscate_block(block, vault, extra);
        }
    }
    Ok(out)
}

/// The outbound request copy for one session.
///
/// `Ok(None)` means "send the messages you already hold": the default `off` mode
/// never copies the transcript.
pub fn outbound_messages(
    messages: &[Message],
    session_id: &str,
    mode: SecretsMode,
) -> Result<Option<Vec<Message>>, SecretBlocked> {
    if mode == SecretsMode::Off {
        return Ok(None);
    }
    match session_vault_with(session_id, |vault| obfuscate_messages(messages, vault, mode, &[])) {
        Some(result) => result.map(Some),
        None => obfuscate_messages(messages, &mut SecretVault::new(), mode, &[]).map(Some),
    }
}

/// Restore placeholders in a tool input so the tool runs the real command.
pub fn restore_tool_input(mut input: Value, session_id: &str) -> Value {
    let restored = session_vault_with(session_id, |vault| {
        restore_value(&mut input, vault);
        input.clone()
    });
    restored.unwrap_or(input)
}

/// Restore the input, then let `gate` decide on the restored payload.
///
/// The order is the reason this function exists: a gate that reviews
/// placeholders approves a command nobody runs, because the tool itself receives
/// the restored input.
pub async fn restore_then_gate<F, Fut>(input: Value, session_id: &str, gate: F) -> (Value, GateDecision)
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = GateDecision>,
{
    let restored = restore_tool_input(input, session_id);
    let payload = serde_json::to_string(&restored).unwrap_or_default();
    let decision = gate(payload).await;
    (restored, decision)
}

/// Replace credentials in a tool result before it becomes history.
pub fn mask_for_history(text: &str, session_id: &str, mode: SecretsMode) -> String {
    if mode == SecretsMode::Off {
        return text.to_string();
    }
    session_vault_with(session_id, |vault| obfuscate(text, vault, &[])).unwrap_or_else(|| text.to_string())
}

fn replace_whole(text: &str, regex: &Regex, label: &str, vault: &mut SecretVault) -> String {
    if !regex.is_match(text) {
        return text.to_string();
    }
    regex
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let Some(found) = caps.get(0) else {
                return String::new();
            };
            let secret = found.as_str();
            if secret.trim().is_empty() || is_placeholder(secret) {
                return secret.to_string();
            }
            vault.placeholder_for(secret, label)
        })
        .into_owned()
}

fn replace_group(
    text: &str,
    regex: &Regex,
    group: usize,
    label_override: Option<&str>,
    vault: &mut SecretVault,
) -> String {
    if !regex.is_match(text) {
        return text.to_string();
    }
    regex
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let Some(whole) = caps.get(0) else {
                return String::new();
            };
            let Some(value) = caps.get(group) else {
                return whole.as_str().to_string();
            };
            if value.as_str().trim().is_empty() || is_placeholder(value.as_str()) {
                return whole.as_str().to_string();
            }
            let label = match label_override {
                Some(label) => label.to_string(),
                None => caps
                    .get(1)
                    .map(|key| sanitize_label(key.as_str().trim_end_matches(['=', ' ', ':'])))
                    .unwrap_or_else(|| "secret".to_string()),
            };
            let placeholder = vault.placeholder_for(value.as_str(), &label);
            let whole_text = whole.as_str();
            let start = value.start() - whole.start();
            let end = value.end() - whole.start();
            format!("{}{}{}", &whole_text[..start], placeholder, &whole_text[end..])
        })
        .into_owned()
}

fn obfuscate_value(value: &mut Value, vault: &mut SecretVault, extra: &[Regex]) {
    match value {
        Value::String(text) => *text = obfuscate(text, vault, extra),
        Value::Array(entries) => {
            for entry in entries {
                obfuscate_value(entry, vault, extra);
            }
        }
        Value::Object(map) => {
            for entry in map.values_mut() {
                obfuscate_value(entry, vault, extra);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn restore_value(value: &mut Value, vault: &SecretVault) {
    match value {
        Value::String(text) => *text = restore(text, vault),
        Value::Array(entries) => {
            for entry in entries {
                restore_value(entry, vault);
            }
        }
        Value::Object(map) => {
            for entry in map.values_mut() {
                restore_value(entry, vault);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn obfuscate_block(block: &mut ContentBlock, vault: &mut SecretVault, extra: &[Regex]) {
    match block {
        ContentBlock::Text { text, .. } | ContentBlock::Reasoning { text } | ContentBlock::ReasoningTrace { text } => {
            *text = obfuscate(text, vault, extra);
        }
        ContentBlock::AnthropicThinking { thinking, .. } => {
            *thinking = obfuscate(thinking, vault, extra);
        }
        ContentBlock::OpenAIReasoning { summary, .. } => {
            for item in summary {
                *item = obfuscate(item, vault, extra);
            }
        }
        ContentBlock::ToolResult { content, .. } => {
            *content = obfuscate(content, vault, extra);
        }
        ContentBlock::ToolUse { input, .. } => obfuscate_value(input, vault, extra),
        ContentBlock::ProviderNative { item, .. } => obfuscate_value(item, vault, extra),
        ContentBlock::Image { .. }
        | ContentBlock::Audio { .. }
        | ContentBlock::OpenAICompaction { .. }
        | ContentBlock::ToolReference { .. } => {}
    }
}

fn detect_block(block: &ContentBlock) -> Vec<String> {
    let mut labels = Vec::new();
    let mut check = |text: &str| labels.extend(detect(text));
    match block {
        ContentBlock::Text { text, .. } | ContentBlock::Reasoning { text } | ContentBlock::ReasoningTrace { text } => {
            check(text)
        }
        ContentBlock::AnthropicThinking { thinking, .. } => check(thinking),
        ContentBlock::OpenAIReasoning { summary, .. } => {
            for item in summary {
                check(item);
            }
        }
        ContentBlock::ToolResult { content, .. } => check(content),
        ContentBlock::ToolUse { input, .. } => check(&input.to_string()),
        ContentBlock::ProviderNative { item, .. } => check(&item.to_string()),
        ContentBlock::Image { .. }
        | ContentBlock::Audio { .. }
        | ContentBlock::OpenAICompaction { .. }
        | ContentBlock::ToolReference { .. } => {}
    }
    labels
}

static SESSION_VAULTS: OnceLock<RwLock<HashMap<String, SessionVault>>> = OnceLock::new();
static NEXT_VAULT_OWNER: AtomicU64 = AtomicU64::new(1);

struct SessionVault {
    vault: Arc<Mutex<SecretVault>>,
    owner: u64,
}

fn session_vaults() -> &'static RwLock<HashMap<String, SessionVault>> {
    SESSION_VAULTS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Removes an Agent-owned vault when that Agent actually leaves memory.
///
/// The owner token keeps a stale Agent from dropping the vault a successor
/// connection installed for the same persisted session ID.
pub struct SessionSecretVaultRegistration {
    session_id: String,
    owner: u64,
}

impl Drop for SessionSecretVaultRegistration {
    fn drop(&mut self) {
        let mut vaults = session_vaults()
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if vaults
            .get(&self.session_id)
            .is_some_and(|entry| entry.owner == self.owner)
        {
            vaults.remove(&self.session_id);
        }
    }
}

/// Install a fresh vault for `session_id`, replacing any previous one.
pub fn register_session_vault(session_id: &str) -> SessionSecretVaultRegistration {
    let owner = NEXT_VAULT_OWNER.fetch_add(1, Ordering::Relaxed);
    let mut vault = SecretVault::new();
    vault.preseed_env();
    let mut vaults = session_vaults()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    vaults.insert(
        session_id.to_string(),
        SessionVault {
            vault: Arc::new(Mutex::new(vault)),
            owner,
        },
    );
    SessionSecretVaultRegistration {
        session_id: session_id.to_string(),
        owner,
    }
}

/// Run `f` against the session's vault. `None` when the session has none, so a
/// call from a session-less context keeps working unchanged.
pub fn session_vault_with<T>(session_id: &str, f: impl FnOnce(&mut SecretVault) -> T) -> Option<T> {
    let vault = {
        let vaults = session_vaults().read().unwrap_or_else(|poisoned| poisoned.into_inner());
        vaults.get(session_id).map(|entry| Arc::clone(&entry.vault))
    }?;
    let mut guard = vault.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    Some(f(&mut guard))
}

/// Drop the placeholders a cleared conversation minted, keeping the vault entry
/// the Agent's registration owns.
pub fn reset_session_vault(session_id: &str) {
    session_vault_with(session_id, |vault| vault.clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obfuscate_replaces_known_shapes_with_stable_placeholders() {
        let mut vault = SecretVault::new();
        let text = "OPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuvwxyz012345\n\
                    token: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123\n";
        let masked = obfuscate(text, &mut vault, &[]);

        assert!(!masked.contains("sk-proj-abcdefghijklmnopqrstuvwxyz012345"), "{masked}");
        assert!(!masked.contains("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123"), "{masked}");
        assert!(masked.contains("«SECRET:openai_api_key:"), "{masked}");
        assert!(masked.contains("«SECRET:github:"), "{masked}");
        assert_eq!(vault.len(), 2);
    }

    #[test]
    fn restore_returns_the_original_bytes() {
        let mut vault = SecretVault::new();
        // The AWS sample is assembled from two pieces: the masking rule and the
        // repository secret scan both look for `AKIA` followed by sixteen token
        // characters, so a single literal here would read as credential material
        // to the scan while proving nothing more than the round trip below.
        let aws_key = format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP");
        let text = format!(
            "export OPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuvwxyz012345\n\
             aws={aws_key}\n\
             header: Bearer abcdefghijklmnopqrstuvwxyz0123456789\n"
        );
        let masked = obfuscate(&text, &mut vault, &[]);
        assert_ne!(masked, text, "the sample must actually carry credentials");
        assert!(
            masked.contains("«SECRET:aws:"),
            "the AWS shape must be masked: {masked}"
        );
        assert_eq!(restore(&masked, &vault), text);
    }

    #[test]
    fn same_secret_gets_the_same_placeholder_twice() {
        let mut vault = SecretVault::new();
        let secret = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123";
        let first = obfuscate(secret, &mut vault, &[]);
        let second = obfuscate(&format!("again {secret}"), &mut vault, &[]);
        assert!(second.contains(&first), "first={first} second={second}");
        assert_eq!(vault.len(), 1);

        let other = obfuscate("ghp_ZZZZZZZZZZZZZZZZZZZZZZZZZZZZ", &mut vault, &[]);
        assert_ne!(other, first, "distinct credentials must not share a placeholder");
        assert_eq!(vault.len(), 2);
    }

    #[test]
    fn fresh_vaults_render_the_same_placeholder() {
        let secret = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123";
        let mut before = SecretVault::new();
        let mut after = SecretVault::new();
        let first = before.placeholder_for(secret, "github");
        let second = after.placeholder_for(secret, "github");
        assert_eq!(first, second, "a restarted vault must mint the same placeholder");
        let base = first
            .strip_prefix("«SECRET:github:")
            .and_then(|rest| rest.strip_suffix('»'))
            .unwrap_or_else(|| panic!("unexpected placeholder shape: {first}"));
        assert_eq!(base.len(), 12, "unexpected placeholder shape: {first}");
        assert!(
            base.bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit()),
            "unexpected placeholder shape: {first}"
        );
    }

    #[test]
    fn placeholder_key_file_is_reused_and_owner_only() {
        let home = tempfile::Builder::new()
            .prefix("horon-agent-secrets-key-")
            .tempdir()
            .expect("temp home");
        let path = home.path().join(PLACEHOLDER_KEY_FILE);

        let first = load_or_create_key(&path).expect("create the key file");
        let second = load_or_create_key(&path).expect("re-read the key file");
        assert_eq!(first, second, "a restart must reuse the persisted key");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path)
                .expect("key file metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the key file must stay owner-only");
        }

        let secret = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123";
        assert_eq!(placeholder_base(&first, secret), placeholder_base(&second, secret));
    }

    #[test]
    fn session_vault_restores_env_held_credentials_after_a_restart() {
        let _env_lock = crate::storage::lock_test_env();
        let name = "HORON_AGENT_TEST_PRIVATE_TOKEN";
        let value = "env-held-secret-value";
        crate::env::set_var(name, value);

        let session_id = "secrets-test-env-preseed";
        let placeholder = {
            let registration = register_session_vault(session_id);
            let placeholder =
                session_vault_with(session_id, |vault| vault.placeholder_for(value, "env")).expect("first vault");
            drop(registration);
            placeholder
        };
        assert_eq!(
            placeholder,
            format!("«SECRET:env:{}»", placeholder_base(placeholder_key(), value))
        );

        let registration = register_session_vault(session_id);
        let restored = session_vault_with(session_id, |vault| restore(&placeholder, vault)).expect("restarted vault");
        assert_eq!(restored, value, "the pre-seeded vault must resolve the placeholder");
        drop(registration);
        crate::env::remove_var(name);
    }

    #[test]
    fn block_mode_refuses_to_send() {
        let mut vault = SecretVault::new();
        let text = "OPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuvwxyz012345";
        let error = apply_outbound(text, &mut vault, SecretsMode::Block, &[]).expect_err("block must refuse");
        assert!(error.to_string().starts_with("SECRET_BLOCK"), "{error}");
        assert!(error.to_string().contains("openai_api_key"), "{error}");
        assert_eq!(error.count, 1);
        assert!(vault.is_empty(), "a refused send must not mint placeholders");

        let clean = apply_outbound("nothing to see", &mut vault, SecretsMode::Block, &[]);
        assert_eq!(clean.expect("clean text passes"), "nothing to see");

        let session_id = "secrets-test-block";
        let _registration = register_session_vault(session_id);
        let refused = outbound_messages(&[Message::user(text)], session_id, SecretsMode::Block)
            .expect_err("a request carrying a credential must not leave");
        assert!(refused.to_string().starts_with("SECRET_BLOCK"), "{refused}");
        assert!(
            outbound_messages(&[Message::user("nothing to see")], session_id, SecretsMode::Block)
                .expect("a clean request passes")
                .is_some()
        );
    }

    #[test]
    fn off_mode_changes_nothing() {
        let mut vault = SecretVault::new();
        let text = "OPENAI_API_KEY=sk-proj-abcdefghijklmnopqrstuvwxyz012345";
        assert_eq!(apply_outbound(text, &mut vault, SecretsMode::Off, &[]).unwrap(), text);
        assert!(vault.is_empty());
        assert_eq!(mask_for_history(text, "off-mode-session", SecretsMode::Off), text);
        assert!(
            outbound_messages(&[Message::user(text)], "off-mode-session", SecretsMode::Off)
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn tool_input_is_restored_before_the_gate_sees_it() {
        let session_id = "secrets-test-tool-input";
        let _registration = register_session_vault(session_id);
        let real = "sk-proj-abcdefghijklmnopqrstuvwxyz012345";
        let masked_input = session_vault_with(session_id, |vault| {
            let mut value = serde_json::json!({
                "command": format!("curl -H 'Authorization: Bearer {real}' https://example.test"),
            });
            obfuscate_value(&mut value, vault, &[]);
            value
        })
        .expect("session vault");
        assert!(
            !masked_input.to_string().contains(real),
            "the model only ever holds the placeholder: {masked_input}"
        );

        let seen = Arc::new(Mutex::new(String::new()));
        let recorded = Arc::clone(&seen);
        let (input, decision) = restore_then_gate(masked_input, session_id, move |payload| {
            *recorded.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = payload;
            async { GateDecision::Allow }
        })
        .await;

        let seen = seen.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        assert_eq!(decision, GateDecision::Allow);
        assert!(
            seen.contains(real),
            "the gate must review the command that will actually run: {seen}"
        );
        assert!(!seen.contains("«SECRET:"), "the gate must not see placeholders: {seen}");
        assert!(
            input
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| command.contains(real)),
            "the tool receives the restored input too: {input}"
        );
    }

    #[test]
    fn tool_output_is_masked_before_history() {
        let session_id = "secrets-test-tool-output";
        let _registration = register_session_vault(session_id);
        let real = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123";
        let output = format!("remote: token {real}\nremote: done\n");
        let masked = mask_for_history(&output, session_id, SecretsMode::Obfuscate);

        assert!(!masked.contains(real), "history must not hold the credential: {masked}");
        assert!(masked.contains("«SECRET:github:"), "{masked}");
        let restored = session_vault_with(session_id, |vault| restore(&masked, vault)).expect("session vault");
        assert_eq!(restored, output, "the round trip keeps the original bytes");
    }

    #[test]
    fn vault_never_touches_disk() {
        let _env_lock = crate::storage::lock_test_env();
        let home = tempfile::Builder::new()
            .prefix("horon-agent-secrets-home-")
            .tempdir()
            .expect("temp home");
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", home.path());

        let session_id = "secrets-test-no-disk";
        let registration = register_session_vault(session_id);
        let real = "sk-proj-abcdefghijklmnopqrstuvwxyz012345";
        let masked = mask_for_history(&format!("OPENAI_API_KEY={real}\n"), session_id, SecretsMode::Obfuscate);
        assert!(!masked.contains(real));
        assert!(
            !format!(
                "{vault:?}",
                vault = session_vault_with(session_id, |v| v.clone()).unwrap()
            )
            .contains(real)
        );

        std::fs::create_dir_all(home.path().join("sessions")).expect("session dir");
        std::fs::write(home.path().join("sessions").join("history.json"), &masked).expect("write history");
        let restored = session_vault_with(session_id, |vault| restore(&masked, vault)).expect("session vault");
        assert!(restored.contains(real), "the value survives in memory only");
        drop(registration);

        let mut offenders = Vec::new();
        let mut stack = vec![home.path().to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let bytes = std::fs::read(&path).expect("read file");
                if String::from_utf8_lossy(&bytes).contains(real) {
                    offenders.push(path);
                }
            }
        }
        assert!(offenders.is_empty(), "the real value reached disk: {offenders:?}");

        match previous {
            Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
    }
}
