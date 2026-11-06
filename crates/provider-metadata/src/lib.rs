#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenAiCompatibleProfile {
    pub id: &'static str,
    pub display_name: &'static str,
    pub api_base: &'static str,
    pub api_key_env: &'static str,
    pub env_file: &'static str,
    pub setup_url: &'static str,
    pub default_model: Option<&'static str>,
    pub requires_api_key: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedOpenAiCompatibleProfile {
    pub id: String,
    pub display_name: String,
    pub api_base: String,
    pub api_key_env: String,
    pub env_file: String,
    pub setup_url: String,
    pub default_model: Option<String>,
    pub requires_api_key: bool,
}

mod catalog;

use catalog::OPENAI_COMPAT_PROFILES;
pub use catalog::*;

pub fn openai_compatible_profiles() -> &'static [OpenAiCompatibleProfile] {
    &OPENAI_COMPAT_PROFILES
}

/// Resolve a built-in OpenAI-compatible profile by its id.
///
/// This is the only provider-id resolver: the CLI `--provider` value, profile
/// selection, and auth metadata all resolve through profile ids.
pub fn openai_compatible_profile_by_id(id: &str) -> Option<OpenAiCompatibleProfile> {
    let normalized = id.trim().to_ascii_lowercase();
    openai_compatible_profiles()
        .iter()
        .copied()
        .find(|profile| profile.id == normalized)
}

pub fn is_safe_env_key_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

pub fn is_safe_env_file_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('/')
        && !name.contains('\\')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

pub fn normalize_api_base(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }

    let parsed = url::Url::parse(trimmed).ok()?;
    let scheme = parsed.scheme();
    if scheme != "https" && scheme != "http" {
        return None;
    }

    if scheme == "http" {
        let host = parsed.host_str()?;
        if !allows_insecure_http_host(host) {
            return None;
        }
    }

    Some(trimmed.trim_end_matches('/').to_string())
}

fn allows_insecure_http_host(host: &str) -> bool {
    let host = host.trim();
    let host = host.strip_prefix('[').and_then(|s| s.strip_suffix(']')).unwrap_or(host);
    let host_lower = host.to_ascii_lowercase();
    if host_lower == "localhost" || host_lower.ends_with(".local") {
        return true;
    }

    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => {
                let raw = u32::from(v4);
                let is_carrier_grade_nat = (raw & 0xffc0_0000) == 0x6440_0000;
                v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified() || is_carrier_grade_nat
            }
            std::net::IpAddr::V6(v6) => {
                v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local() || v6.is_unspecified()
            }
        };
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn matrix_profiles_have_unique_ids_and_safe_metadata() {
        let mut ids = HashSet::new();
        for profile in openai_compatible_profiles() {
            assert!(ids.insert(profile.id), "duplicate provider profile id: {}", profile.id);
            assert!(is_safe_env_key_name(profile.api_key_env));
            assert!(is_safe_env_file_name(profile.env_file));
            assert_eq!(normalize_api_base(profile.api_base).as_deref(), Some(profile.api_base));
        }
    }

    #[test]
    fn profile_ids_resolve_back_to_the_same_profile() {
        for profile in openai_compatible_profiles() {
            assert_eq!(
                openai_compatible_profile_by_id(profile.id).map(|found| found.id),
                Some(profile.id)
            );
            assert_eq!(
                openai_compatible_profile_by_id(&profile.id.to_ascii_uppercase()).map(|found| found.id),
                Some(profile.id)
            );
        }
        assert_eq!(openai_compatible_profile_by_id(""), None);
        assert_eq!(openai_compatible_profile_by_id("no-such-profile"), None);
    }

    #[test]
    fn zai_profile_keeps_its_coding_plan_endpoint() {
        let profile = openai_compatible_profile_by_id("zai").expect("Z.AI profile");
        assert_eq!(profile.api_base, "https://api.z.ai/api/coding/paas/v4");
        assert_eq!(profile.setup_url, "https://docs.z.ai/devpack/quick-start");
    }

    #[test]
    fn orcarouter_profile_uses_its_openai_compatible_endpoint() {
        let profile = openai_compatible_profile_by_id("orcarouter").expect("OrcaRouter profile");
        assert_eq!(profile.api_base, "https://api.orcarouter.ai/v1");
        assert_eq!(profile.api_key_env, "ORCAROUTER_API_KEY");
        assert!(profile.requires_api_key);
    }

    #[test]
    fn normalize_api_base_accepts_private_http_hosts() {
        assert_eq!(
            normalize_api_base("http://192.168.1.25:8000/v1/").as_deref(),
            Some("http://192.168.1.25:8000/v1")
        );
        assert_eq!(
            normalize_api_base("http://10.0.0.8:11434/v1").as_deref(),
            Some("http://10.0.0.8:11434/v1")
        );
        assert_eq!(
            normalize_api_base("http://100.103.78.84:11434/v1").as_deref(),
            Some("http://100.103.78.84:11434/v1")
        );
        assert_eq!(
            normalize_api_base("http://hsv.local:11434/v1").as_deref(),
            Some("http://hsv.local:11434/v1")
        );
        assert_eq!(
            normalize_api_base("http://[fd00::1]:8080/v1").as_deref(),
            Some("http://[fd00::1]:8080/v1")
        );
    }

    #[test]
    fn normalize_api_base_rejects_public_http_hosts() {
        assert_eq!(normalize_api_base("http://example.com/v1"), None);
        assert_eq!(normalize_api_base("http://8.8.8.8/v1"), None);
    }

    #[test]
    fn alibaba_coding_plan_uses_current_international_endpoint() {
        assert_eq!(
            ALIBABA_CODING_PLAN_PROFILE.api_base,
            "https://coding-intl.dashscope.aliyuncs.com/v1"
        );
    }

    #[test]
    fn minimax_profile_uses_official_openai_compatible_configuration() {
        assert_eq!(MINIMAX_PROFILE.api_base, "https://api.minimax.io/v1");
        assert_eq!(MINIMAX_PROFILE.api_key_env, "MINIMAX_API_KEY");
    }

    #[test]
    fn novita_profile_uses_its_documented_openai_compatible_configuration() {
        assert_eq!(NOVITA_PROFILE.api_base, "https://api.novita.ai/openai");
        assert_eq!(NOVITA_PROFILE.api_key_env, "NOVITA_API_KEY");
        assert_eq!(NOVITA_PROFILE.env_file, "novita.env");
        assert_eq!(NOVITA_PROFILE.default_model, Some("zai-org/glm-5.3"));
        const { assert!(NOVITA_PROFILE.requires_api_key) };
        assert!(openai_compatible_profiles().contains(&NOVITA_PROFILE));
    }

    #[test]
    fn nvidia_nim_profile_uses_hosted_openai_compatible_configuration() {
        assert_eq!(NVIDIA_NIM_PROFILE.api_base, "https://integrate.api.nvidia.com/v1");
        assert_eq!(NVIDIA_NIM_PROFILE.api_key_env, "NVIDIA_API_KEY");
        assert_eq!(NVIDIA_NIM_PROFILE.env_file, "nvidia-nim.env");
        assert_eq!(
            NVIDIA_NIM_PROFILE.default_model,
            Some("nvidia/llama-3.1-nemotron-ultra-253b-v1")
        );
    }

    #[test]
    fn cerebras_profile_uses_official_openai_compatible_configuration() {
        assert_eq!(CEREBRAS_PROFILE.id, "cerebras");
        assert_eq!(CEREBRAS_PROFILE.display_name, "Cerebras");
        assert_eq!(CEREBRAS_PROFILE.api_base, "https://api.cerebras.ai/v1");
        assert_eq!(CEREBRAS_PROFILE.api_key_env, "CEREBRAS_API_KEY");
        assert_eq!(CEREBRAS_PROFILE.env_file, "cerebras.env");
        assert_eq!(
            CEREBRAS_PROFILE.setup_url,
            "https://inference-docs.cerebras.ai/introduction"
        );
        assert_eq!(CEREBRAS_PROFILE.default_model, Some("gpt-oss-120b"));
        const { assert!(CEREBRAS_PROFILE.requires_api_key) };
    }

    #[test]
    fn belvedir_profile_uses_official_inference_router_configuration() {
        assert_eq!(BELVEDIR_PROFILE.id, "belvedir");
        assert_eq!(BELVEDIR_PROFILE.display_name, "Belvedir");
        assert_eq!(BELVEDIR_PROFILE.api_base, "https://platform.belvedir.ai/api/v1/route");
        assert_eq!(BELVEDIR_PROFILE.api_key_env, "BELVEDIR_API_KEY");
        assert_eq!(BELVEDIR_PROFILE.env_file, "belvedir.env");
        assert_eq!(BELVEDIR_PROFILE.default_model, Some("auto"));
        const { assert!(BELVEDIR_PROFILE.requires_api_key) };
    }

    #[test]
    fn ollama_profile_is_local_openai_compatible_without_required_api_key() {
        assert_eq!(OLLAMA_PROFILE.id, "ollama");
        assert_eq!(OLLAMA_PROFILE.api_base, "http://localhost:11434/v1");
        assert_eq!(OLLAMA_PROFILE.api_key_env, "OLLAMA_API_KEY");
        assert_eq!(OLLAMA_PROFILE.env_file, "ollama.env");
        assert_eq!(
            OLLAMA_PROFILE.setup_url,
            "https://docs.ollama.com/api/openai-compatibility"
        );
        assert_eq!(OLLAMA_PROFILE.default_model, None);
        const {
            assert!(!OLLAMA_PROFILE.requires_api_key);
        }
    }
}
