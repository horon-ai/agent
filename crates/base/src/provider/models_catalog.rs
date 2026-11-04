use super::*;

/// Total per-request timeout for model catalog fetches. The shared HTTP
/// client only sets a connect timeout, so without this a hung catalog request
/// keeps the scope's refresh marked in-flight and the picker stays stale.
const CATALOG_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// HTTP status carried inside model catalog fetch errors so callers can
/// distinguish auth rejections (401/403) from transient failures and recover
/// by force-refreshing OAuth tokens, mirroring the chat request path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelCatalogHttpStatus(pub u16);

impl std::fmt::Display for ModelCatalogHttpStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP status {}", self.0)
    }
}

impl std::error::Error for ModelCatalogHttpStatus {}

fn catalog_status_error(status: reqwest::StatusCode, context: String) -> anyhow::Error {
    anyhow::Error::new(ModelCatalogHttpStatus(status.as_u16())).context(context)
}

#[derive(Debug, Clone, Default)]
pub struct OpenAIModelCatalog {
    pub available_models: Vec<String>,
    pub context_limits: HashMap<String, usize>,
    /// Ordered reasoning-effort values advertised by each Codex model.
    pub reasoning_efforts: HashMap<String, Vec<String>>,
}

pub(crate) fn parse_openai_model_catalog(data: &serde_json::Value) -> OpenAIModelCatalog {
    let models = data
        .get("models")
        .and_then(|m| m.as_array())
        .or_else(|| {
            data.get("data")
                .and_then(|d| d.get("models"))
                .and_then(|m| m.as_array())
        })
        .or_else(|| data.get("data").and_then(|d| d.as_array()))
        .or_else(|| data.as_array());

    let mut available: HashSet<String> = HashSet::new();
    let mut limits: HashMap<String, usize> = HashMap::new();
    let mut reasoning_efforts: HashMap<String, Vec<String>> = HashMap::new();

    for model in models.into_iter().flatten() {
        let Some(slug) = model
            .get("slug")
            .or_else(|| model.get("id"))
            .or_else(|| model.get("model"))
            .and_then(|s| s.as_str())
        else {
            continue;
        };

        let slug = normalize_model_id(slug);
        if slug.is_empty() {
            continue;
        }

        available.insert(slug.clone());

        if let Some(ctx) = model
            .get("context_window")
            .or_else(|| model.get("context_length"))
            .and_then(|c| c.as_u64())
        {
            limits.insert(slug.clone(), ctx as usize);
        }

        if let Some(values) = model
            .get("supported_reasoning_levels")
            .or_else(|| model.get("supported_reasoning_efforts"))
            .and_then(|value| value.as_array())
        {
            let mut efforts = Vec::new();
            for value in values {
                let raw = value.as_str().or_else(|| {
                    value
                        .get("reasoning_effort")
                        .or_else(|| value.get("effort"))
                        .or_else(|| value.get("value"))
                        .and_then(|value| value.as_str())
                });
                let Some(effort) = raw.and_then(horon_agent_provider_core::canonical_reasoning_effort) else {
                    continue;
                };
                if !efforts.iter().any(|existing| existing == effort) {
                    efforts.push(effort.to_string());
                }
            }
            if !efforts.is_empty() {
                reasoning_efforts.insert(slug, efforts);
            }
        }
    }

    let mut available_models: Vec<String> = available.into_iter().collect();
    available_models.sort();

    OpenAIModelCatalog {
        available_models,
        context_limits: limits,
        reasoning_efforts,
    }
}

/// Fetch model availability and context windows from the Codex backend API.
pub async fn fetch_openai_model_catalog(access_token: &str) -> Result<OpenAIModelCatalog> {
    note_openai_model_catalog_refresh_attempt();

    let client = shared_http_client();
    let resp = client
        .get("https://chatgpt.com/backend-api/codex/models?client_version=1.0.0")
        .header("Authorization", format!("Bearer {}", access_token))
        .timeout(CATALOG_REQUEST_TIMEOUT)
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(catalog_status_error(
            resp.status(),
            format!("Failed to fetch model context limits: {}", resp.status()),
        ));
    }

    let data: serde_json::Value = resp.json().await?;
    Ok(parse_openai_model_catalog(&data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_catalog_parses_string_and_object_reasoning_efforts() {
        let catalog = parse_openai_model_catalog(&serde_json::json!({
            "models": [
                {
                    "slug": "gpt-5.6",
                    "supported_reasoning_levels": [
                        { "reasoning_effort": "minimal" },
                        { "reasoning_effort": "high" },
                        { "reasoning_effort": "max" }
                    ]
                },
                {
                    "slug": "gpt-5.4",
                    "supported_reasoning_efforts": ["none", "low", "xhigh"]
                }
            ]
        }));

        assert_eq!(
            catalog.reasoning_efforts.get("gpt-5.6"),
            Some(&vec!["minimal".to_string(), "high".to_string(), "max".to_string()])
        );
        assert_eq!(
            catalog.reasoning_efforts.get("gpt-5.4"),
            Some(&vec!["none".to_string(), "low".to_string(), "xhigh".to_string()])
        );
    }
}

#[cfg(test)]
mod status_error_tests {
    use super::*;

    #[test]
    fn catalog_status_error_round_trips_through_anyhow_downcast() {
        let err = catalog_status_error(
            reqwest::StatusCode::UNAUTHORIZED,
            "Failed to fetch model context limits: 401 Unauthorized".to_string(),
        );
        let status = err
            .downcast_ref::<ModelCatalogHttpStatus>()
            .expect("status must survive the context wrapper");
        assert_eq!(status.0, 401);
        // The human-readable context must stay the outermost message.
        assert!(err.to_string().contains("Failed to fetch model context limits"));
    }
}
