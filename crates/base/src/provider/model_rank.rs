use super::{ALL_CLAUDE_MODELS, ALL_OPENAI_MODELS};

/// Curated flagship-first preference tiers.
///
/// Each inner slice is one family ordered best-first; earlier families outrank
/// later ones. Providers without a curated order return an empty slice, which
/// keeps the live-catalog order.
fn provider_preferred_model_orders(provider_id: &str) -> &'static [&'static [&'static str]] {
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-api" | "anthropic" | "anthropic-api" => &[ALL_CLAUDE_MODELS],
        "openai" | "openai-api" => &[ALL_OPENAI_MODELS],
        // Proxies carry both families under their bare canonical ids.
        "copilot" | "cursor" | "openrouter" => &[ALL_CLAUDE_MODELS, ALL_OPENAI_MODELS],
        _ => &[],
    }
}

fn strip_trailing_bedrock_version(id: &str) -> Option<String> {
    let (head, tail) = id.rsplit_once('-')?;
    let is_version_tag = tail.len() >= 2 && tail.starts_with('v') && tail[1..].chars().all(|c| c.is_ascii_digit());
    is_version_tag.then(|| head.to_string())
}

/// Strip a leading Bedrock region routing segment (`us.`, `eu.`, `apac.`,
/// `us-gov.`) from a model id.
fn strip_bedrock_region_prefix(id: &str) -> String {
    for region in ["us-gov.", "us.", "eu.", "apac.", "ap.", "global."] {
        if let Some(rest) = id.strip_prefix(region) {
            return rest.to_string();
        }
    }
    id.to_string()
}

/// Normalize a model id for flagship-preference comparison.
///
/// Live catalogs spell the same release several ways (`accounts/x/models/y`,
/// `us.anthropic.claude-opus-4-20250514-v1:0`, `claude-opus-4.6`), so ranking
/// compares a canonical bare form instead of the raw id.
fn normalize_model_for_preference(model: &str) -> String {
    let mut id = horon_agent_provider_core::model_id::canonical(model);

    // Drop a `/`-qualified path prefix (`accounts/x/models/y`, `models/gemini`).
    if let Some(idx) = id.rfind('/') {
        id = id[idx + 1..].to_string();
    }

    // Drop a trailing version tag (`-v1:0`, `-v2:0`, `:0`).
    if let Some(idx) = id.find(":0") {
        id = id[..idx].to_string();
    }
    if let Some(stripped) = strip_trailing_bedrock_version(&id) {
        id = stripped;
    }

    // Drop a trailing release-date suffix.
    id = horon_agent_provider_core::model_id::strip_date_suffix(&id).to_string();

    // Drop a trailing `-preview`/`-exp`/`-latest` marketing suffix.
    for suffix in ["-preview", "-exp", "-latest"] {
        if let Some(base) = id.strip_suffix(suffix) {
            id = base.to_string();
        }
    }

    // Drop a hosted-vendor segment so `anthropic.claude-opus-4` matches the
    // curated `claude-opus-4`.
    id = strip_bedrock_region_prefix(&id);
    if let Some(rest) = id.strip_prefix("anthropic.") {
        id = rest.to_string();
    }

    // Copilot and OpenRouter spell Claude releases with dots
    // (`claude-opus-4.6`) while the curated catalog uses hyphens
    // (`claude-opus-4-6`).
    if id.starts_with("claude-") {
        id = id.replace('.', "-");
    }

    id
}

/// Rank a model id against the provider's flagship-first tiers. Lower is more
/// preferred; `usize::MAX` means the model is not in any curated tier.
pub fn preferred_model_rank(provider_id: &str, model: &str) -> usize {
    const TIER_STRIDE: usize = 10_000;
    let normalized = normalize_model_for_preference(model);
    for (tier, order) in provider_preferred_model_orders(provider_id).iter().enumerate() {
        if let Some(position) = order
            .iter()
            .position(|candidate| normalize_model_for_preference(candidate) == normalized)
        {
            return tier * TIER_STRIDE + position;
        }
    }
    usize::MAX
}

/// A parsed "frontier flagship" model id: family prefix plus an ordered version
/// vector, used to compare releases within a family.
struct FrontierModel {
    family: String,
    version: Vec<u64>,
}

/// Tier/specialization words that disqualify a bare-flagship id (Claude/OpenAI
/// style) from auto-promotion. Cheaper or non-default variants must never
/// outrank a clean flagship id merely because their version number is higher.
const NON_FLAGSHIP_TIER_WORDS: &[&str] = &[
    "mini", "nano", "haiku", "flash", "lite", "small", "tiny", "instant", "codex", "pro", "chat", "audio", "realtime",
    "image", "tts", "embed", "search", "guard", "deep", "thinking",
];

/// One frontier-eligible family: the canonical id prefix and, optionally, a
/// required flagship tier token. When `flagship_token` is `Some`, version
/// parsing ignores that token and rejects any other trailing word.
#[derive(Clone, Copy)]
struct FrontierFamily {
    prefix: &'static str,
    flagship_token: Option<&'static str>,
}

/// Family descriptors eligible for frontier auto-promotion per provider.
fn frontier_families(provider_id: &str) -> &'static [FrontierFamily] {
    const FABLE: FrontierFamily = FrontierFamily {
        prefix: "claude-fable",
        flagship_token: None,
    };
    const CLAUDE: FrontierFamily = FrontierFamily {
        prefix: "claude-opus",
        flagship_token: None,
    };
    const GPT: FrontierFamily = FrontierFamily {
        prefix: "gpt",
        flagship_token: None,
    };
    match provider_id.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-api" | "anthropic" | "anthropic-api" => &[CLAUDE, FABLE],
        "openai" | "openai-api" => &[GPT],
        // Copilot/Cursor/OpenRouter proxy both families under canonical ids.
        "copilot" | "cursor" | "openrouter" => &[CLAUDE, FABLE, GPT],
        _ => &[],
    }
}

fn parse_version_components(remainder: &str) -> Option<Vec<u64>> {
    if remainder.is_empty() {
        return None;
    }
    let mut version = Vec::new();
    for part in remainder.split(['.', '-']) {
        if part.is_empty() {
            continue;
        }
        let Ok(number) = part.parse::<u64>() else {
            return None;
        };
        version.push(number);
    }
    (!version.is_empty()).then_some(version)
}

fn parse_frontier_version_for_family(
    normalized_model: &str,
    family: FrontierFamily,
    allow_suffix: bool,
) -> Option<Vec<u64>> {
    if !normalized_model.starts_with(family.prefix) {
        return None;
    }
    match family.flagship_token {
        None => {
            if !allow_suffix
                && NON_FLAGSHIP_TIER_WORDS
                    .iter()
                    .any(|word| normalized_model.contains(word))
            {
                return None;
            }
            let remainder = normalized_model[family.prefix.len()..].trim_matches(['-', '.', ' ']);
            if !allow_suffix {
                return parse_version_components(remainder);
            }
            let mut version = Vec::new();
            for part in remainder.split(['.', '-']) {
                if part.is_empty() {
                    continue;
                }
                match part.parse::<u64>() {
                    Ok(number) => version.push(number),
                    Err(_) if !version.is_empty() => break,
                    Err(_) => return None,
                }
            }
            (!version.is_empty()).then_some(version)
        }
        Some(flagship_token) => {
            let remainder = normalized_model[family.prefix.len()..].trim_matches(['-', '.', ' ']);
            if remainder.is_empty() {
                return None;
            }
            let mut version = Vec::new();
            let mut saw_flagship_token = false;
            for part in remainder.split(['.', '-']) {
                if part.is_empty() {
                    continue;
                }
                if part == flagship_token {
                    saw_flagship_token = true;
                    continue;
                }
                let Ok(number) = part.parse::<u64>() else {
                    return None;
                };
                version.push(number);
            }
            if !saw_flagship_token || version.is_empty() {
                return None;
            }
            Some(version)
        }
    }
}

fn parse_frontier_model(model: &str, families: &[FrontierFamily]) -> Option<FrontierModel> {
    let normalized = normalize_model_for_preference(model);
    let family = families
        .iter()
        .filter(|family| normalized.starts_with(family.prefix))
        .max_by_key(|family| family.prefix.len())?;
    let version = parse_frontier_version_for_family(&normalized, *family, false)?;
    Some(FrontierModel {
        family: family.prefix.to_string(),
        version,
    })
}

fn version_cmp(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
    let len = a.len().max(b.len());
    for i in 0..len {
        let av = a.get(i).copied().unwrap_or(0);
        let bv = b.get(i).copied().unwrap_or(0);
        match av.cmp(&bv) {
            std::cmp::Ordering::Equal => continue,
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

/// Auto-detect a brand-new frontier flagship release among `models` that is
/// strictly newer than the curated baseline flagship for the same family.
///
/// This is the new-release robustness layer: the day a provider ships
/// `claude-opus-5-6` or `gpt-5.7`, the live catalog carries it and it
/// auto-promotes to the default without a code change, while cheaper or
/// specialized variants stay excluded by [`parse_frontier_model`].
fn newest_frontier_release(provider_id: &str, models: &[&str]) -> Option<String> {
    let families = frontier_families(provider_id);
    if families.is_empty() {
        return None;
    }

    // Only auto-promote within the strongest frontier family that is actually
    // present, so a newer lower-priority Opus id cannot displace an available
    // Fable default.
    let active_family = families.iter().copied().find(|family| {
        models.iter().any(|model| {
            let normalized = normalize_model_for_preference(model);
            parse_frontier_version_for_family(&normalized, *family, true).is_some()
        })
    })?;

    // Baseline: the strongest curated release in the active family. When no
    // curated model is present, fall back to the shipped catalog baseline so an
    // old unknown id cannot self-promote.
    let curated_baseline = |family: FrontierFamily| -> Option<Vec<u64>> {
        let orders = provider_preferred_model_orders(provider_id);
        let available_baseline = models
            .iter()
            .filter(|model| preferred_model_rank(provider_id, model) != usize::MAX)
            .filter_map(|model| {
                let normalized = normalize_model_for_preference(model);
                parse_frontier_version_for_family(&normalized, family, true)
            })
            .max_by(|a, b| version_cmp(a, b));
        available_baseline.or_else(|| {
            orders
                .iter()
                .flat_map(|order| order.iter())
                .filter_map(|id| {
                    let normalized = normalize_model_for_preference(id);
                    parse_frontier_version_for_family(&normalized, family, true)
                })
                .max_by(|a, b| version_cmp(a, b))
        })
    };

    let mut best: Option<(FrontierModel, String)> = None;
    for model in models {
        let Some(parsed) = parse_frontier_model(model, families) else {
            continue;
        };
        if parsed.family != active_family.prefix {
            continue;
        }
        let Some(baseline) = curated_baseline(active_family) else {
            continue;
        };
        if version_cmp(&parsed.version, &baseline) != std::cmp::Ordering::Greater {
            continue;
        }
        let is_better = match &best {
            None => true,
            Some((current, _)) => match version_cmp(&parsed.version, &current.version) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Equal => {
                    let rank = |prefix: &str| {
                        families
                            .iter()
                            .position(|family| family.prefix == prefix)
                            .unwrap_or(usize::MAX)
                    };
                    rank(&parsed.family) < rank(&current.family)
                }
                std::cmp::Ordering::Less => false,
            },
        };
        if is_better {
            best = Some((parsed, (*model).to_string()));
        }
    }

    best.map(|(_, model)| model)
}

/// Pick the strongest model among `models` for `provider_id`: a brand-new
/// frontier release first, then the curated flagship-first order.
///
/// Returns `None` for providers without a curated order, or when no candidate
/// is recognized, so callers keep their own fallback.
pub fn ranked_flagship_for_provider(provider_id: &str, models: &[&str]) -> Option<String> {
    if let Some(newer) = newest_frontier_release(provider_id, models) {
        return Some(newer);
    }
    if provider_preferred_model_orders(provider_id).is_empty() {
        return None;
    }
    models
        .iter()
        .filter(|model| preferred_model_rank(provider_id, model) != usize::MAX)
        .min_by_key(|model| preferred_model_rank(provider_id, model))
        .map(|model| (*model).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curated_order_prefers_the_flagship() {
        assert_eq!(
            ranked_flagship_for_provider("openai", &["gpt-5.4-mini", "gpt-5.5"]),
            Some("gpt-5.5".to_string())
        );
        assert_eq!(
            ranked_flagship_for_provider("anthropic-api", &["claude-haiku-4-5", "claude-opus-4-8"]),
            Some("claude-opus-4-8".to_string())
        );
    }

    #[test]
    fn dated_ids_match_their_bare_curated_id() {
        assert_ne!(preferred_model_rank("openai", "gpt-5.5-20260101"), usize::MAX);
    }

    #[test]
    fn providers_without_a_curated_order_rank_nothing() {
        assert_eq!(ranked_flagship_for_provider("cerebras", &["gpt-oss-120b"]), None);
    }

    #[test]
    fn unlisted_frontier_release_auto_promotes() {
        assert_eq!(
            ranked_flagship_for_provider("openrouter", &["anthropic/claude-opus-5.5", "anthropic/claude-opus-6"]),
            Some("anthropic/claude-opus-6".to_string())
        );
        assert_eq!(
            ranked_flagship_for_provider("openai", &["gpt-5.5", "gpt-5.7"]),
            Some("gpt-5.7".to_string())
        );
    }

    #[test]
    fn specialized_variants_never_self_promote() {
        assert_eq!(
            ranked_flagship_for_provider("openai", &["gpt-5.5", "gpt-5.7-mini"]),
            Some("gpt-5.5".to_string())
        );
    }

    #[test]
    fn normalize_strips_hosted_prefixes_and_suffixes() {
        assert_eq!(
            normalize_model_for_preference("us.anthropic.claude-opus-4-20250514-v1:0"),
            "claude-opus-4"
        );
        assert_eq!(
            normalize_model_for_preference("accounts/fireworks/models/qwen3-coder"),
            "qwen3-coder"
        );
        assert_eq!(normalize_model_for_preference("claude-opus-4.6"), "claude-opus-4-6");
    }
}
