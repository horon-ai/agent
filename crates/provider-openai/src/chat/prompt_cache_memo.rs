//! Memoized `prompt_cache_key` derivation.
//!
//! The key covers the cacheable prefix of a request: the lineage scope, the
//! static system prompt and the tool catalog. The conversation turns below that
//! prefix change on every request, so re-deriving costs one SHA-256 pass over
//! the whole prefix plus a serialization of the tool catalog every time.
//! [`PromptCacheKeyMemo`] keeps the inputs beside the key and returns the key
//! unchanged while they compare equal, which avoids that serialization entirely
//! on a steady-state request.

use serde_json::Value;
use sha2::{Digest, Sha256};

/// Prefix of a derived prompt cache key, matching OpenAI's documented format.
const PROMPT_CACHE_KEY_PREFIX: &str = "pck_";

/// Number of digest bytes a prompt cache key keeps (24 hex characters).
const PROMPT_CACHE_KEY_DIGEST_BYTES: usize = 12;

/// Lowercase hex digits, so formatting a digest byte never allocates or panics.
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// A derived key together with the cacheable prefix it was derived from.
pub(super) struct PromptCacheKeyMemo {
    scope: String,
    system: String,
    tools: Option<Value>,
    key: String,
    /// Keys this memo derived; tests assert a steady-state hit does not grow it.
    #[cfg(test)]
    derivations: u64,
}

impl PromptCacheKeyMemo {
    /// Derive the key for these inputs.
    pub(super) fn derive(scope: &str, system: &str, tools: Option<&Value>) -> Self {
        Self {
            scope: scope.to_string(),
            system: system.to_string(),
            tools: tools.cloned(),
            key: derive_prompt_cache_key(scope, system, tools),
            #[cfg(test)]
            derivations: 1,
        }
    }

    /// Whether this memo already holds the key for these inputs.
    pub(super) fn matches(&self, scope: &str, system: &str, tools: Option<&Value>) -> bool {
        self.scope == scope && self.system == system && self.tools.as_ref() == tools
    }

    /// Replace the memoized inputs with a new derivation, in place.
    pub(super) fn refresh(&mut self, scope: &str, system: &str, tools: Option<&Value>) {
        self.scope.clear();
        self.scope.push_str(scope);
        self.system.clear();
        self.system.push_str(system);
        self.tools = tools.cloned();
        self.key = derive_prompt_cache_key(scope, system, tools);
        #[cfg(test)]
        {
            self.derivations += 1;
        }
    }

    /// The memoized key.
    pub(super) fn key(&self) -> &str {
        &self.key
    }

    /// Keys this memo derived so far.
    #[cfg(test)]
    pub(super) fn derivations(&self) -> u64 {
        self.derivations
    }
}

/// Derive the lineage-stable cache key from the parts about to be sent.
///
/// The key covers the cacheable prefix only: the scope, the static system
/// prompt and the tool catalog. Conversation turns, and a compaction that
/// rewrites them, leave it unchanged, so the same lineage keeps one slot while a
/// different lineage gets its own.
fn derive_prompt_cache_key(scope: &str, system: &str, tools: Option<&Value>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(scope.as_bytes());
    hasher.update([0]);
    hasher.update(system.as_bytes());
    hasher.update([0]);
    // An absent catalog contributes no bytes, matching the empty serialization
    // the byte-identical pre-memo derivation hashed.
    if let Some(tools) = tools {
        hasher.update(tools.to_string().as_bytes());
    }
    let digest = hasher.finalize();

    let mut key = String::with_capacity(PROMPT_CACHE_KEY_PREFIX.len() + PROMPT_CACHE_KEY_DIGEST_BYTES * 2);
    key.push_str(PROMPT_CACHE_KEY_PREFIX);
    for byte in &digest[..PROMPT_CACHE_KEY_DIGEST_BYTES] {
        key.push(HEX_DIGITS[usize::from(byte >> 4)] as char);
        key.push(HEX_DIGITS[usize::from(byte & 0x0f)] as char);
    }
    key
}
