//! Search-engine configuration for the local `websearch` tool.

use super::*;

/// Search engine used by the websearch tool.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchEngine {
    /// DuckDuckGo HTML search, no API key required.
    #[default]
    Duckduckgo,
    /// Bing search. Uses the Bing API when configured, otherwise Bing HTML search.
    Bing,
    /// SearXNG metasearch instance (JSON API). Requires `searxng_url` (or the
    /// `HORON_AGENT_SEARXNG_URL` env var) to point at a SearXNG instance. Useful on
    /// hosts where DuckDuckGo/Bing block the request via TLS fingerprinting.
    Searxng,
    /// User-configured HTTP JSON endpoint.
    Custom,
    /// Provider-native server-side search as a selection value. This build has
    /// no hosted search, so the engine yields no results of its own and the
    /// local `fallback_engines` run instead.
    Native,
}

impl WebSearchEngine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Duckduckgo => "duckduckgo",
            Self::Bing => "bing",
            Self::Searxng => "searxng",
            Self::Custom => "custom",
            Self::Native => "native",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "duckduckgo" | "ddg" => Some(Self::Duckduckgo),
            "bing" => Some(Self::Bing),
            "searxng" | "searx" => Some(Self::Searxng),
            "custom" => Some(Self::Custom),
            "native" | "provider" => Some(Self::Native),
            _ => None,
        }
    }

    /// True for engines the local `websearch` tool can run itself.
    pub fn is_local(self) -> bool {
        !matches!(self, Self::Native)
    }
}

/// HTTP method for a custom JSON search endpoint.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum CustomWebSearchMethod {
    #[default]
    #[serde(rename = "GET", alias = "get")]
    Get,
    #[serde(rename = "POST", alias = "post")]
    Post,
}

/// Configurable HTTP JSON search. Pointers use RFC 6901 syntax.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomWebSearchConfig {
    pub endpoint: String,
    pub method: CustomWebSearchMethod,
    pub query_param: String,
    pub count_param: Option<String>,
    /// Static URL query parameters for either method.
    pub params: std::collections::BTreeMap<String, String>,
    /// Static JSON object for POST requests.
    pub body: serde_json::Value,
    pub headers: std::collections::BTreeMap<String, String>,
    /// Header name to environment variable containing its complete value.
    pub header_env: std::collections::BTreeMap<String, String>,
    pub results_pointer: String,
    pub title_pointer: String,
    pub url_pointer: String,
    pub snippet_pointer: String,
}

impl Default for CustomWebSearchConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            method: CustomWebSearchMethod::Get,
            query_param: "q".into(),
            count_param: None,
            params: Default::default(),
            body: serde_json::json!({}),
            headers: Default::default(),
            header_env: Default::default(),
            results_pointer: "/results".into(),
            title_pointer: "/title".into(),
            url_pointer: "/url".into(),
            snippet_pointer: "/snippet".into(),
        }
    }
}

/// Configuration for the websearch tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebSearchConfig {
    /// HTTP JSON provider configuration for engine = "custom".
    pub custom: Option<CustomWebSearchConfig>,
    /// Preferred engine when the tool input does not specify one.
    pub engine: WebSearchEngine,
    /// Keyless HTML engines to try after the preferred engine fails.
    pub fallback_engines: Vec<WebSearchEngine>,
    /// Optional Bing API key for primary Bing searches. Fallback Bing uses keyless HTML search.
    pub bing_api_key: Option<String>,
    /// Environment variable containing the Bing API key.
    pub bing_api_key_env: String,
    /// Bing market, e.g. "en-US" or "zh-CN".
    pub bing_market: String,
    /// Base URL of a SearXNG instance (e.g. "https://searx.example.org"), used
    /// by the `searxng` engine. When empty, the `searxng_url_env` variable is
    /// consulted instead.
    pub searxng_url: Option<String>,
    /// Environment variable containing the SearXNG base URL.
    pub searxng_url_env: String,
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            custom: None,
            engine: WebSearchEngine::Duckduckgo,
            fallback_engines: vec![WebSearchEngine::Bing],
            bing_api_key: None,
            bing_api_key_env: "HORON_AGENT_BING_API_KEY".to_string(),
            bing_market: "en-US".to_string(),
            searxng_url: None,
            searxng_url_env: "HORON_AGENT_SEARXNG_URL".to_string(),
        }
    }
}
