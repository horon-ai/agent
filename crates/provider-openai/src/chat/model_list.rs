use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use horon_agent_provider_core::{HORON_AGENT_USER_AGENT, http_tls::with_platform_tls};
use reqwest::{Client, Url, redirect::Policy};
use serde::Deserialize;

use super::{OpenAiCompatibleProvider, WireProtocol, transport::apply_auth};

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Deserialize)]
struct ModelList {
    data: Vec<ModelId>,
}

#[derive(Deserialize)]
struct ModelId {
    id: String,
}

pub(super) async fn fetch(provider: &OpenAiCompatibleProvider) -> Result<Vec<String>> {
    if !matches!(provider.wire, WireProtocol::ChatCompletions | WireProtocol::Responses) {
        bail!("the wire does not support OpenAI model discovery");
    }
    let mut url = Url::parse(&provider.base_url).map_err(|_| anyhow!("invalid model endpoint"))?;
    let base = url.path().trim_end_matches('/');
    let base = ["/chat/completions", "/responses", "/models"]
        .iter()
        .find_map(|suffix| base.strip_suffix(suffix))
        .unwrap_or(base);
    let path = format!("{base}/models");
    url.set_path(&path);
    url.set_fragment(None);

    // Redirects could send configured custom credential headers elsewhere.
    // Report only a fixed reason or status, never an upstream body or URL.
    let client = with_platform_tls(Client::builder())
        .redirect(Policy::none())
        .timeout(Duration::from_secs(10))
        .user_agent(HORON_AGENT_USER_AGENT)
        .build()
        .map_err(|_| anyhow!("cannot initialize model discovery HTTP client"))?;
    let mut request = apply_auth(client.get(url), &provider.auth, provider.api_key.as_deref())?;
    for (name, value) in &provider.headers {
        request = request.header(name, value);
    }
    let mut response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            anyhow!("model list request timed out")
        } else {
            anyhow!("model list request could not be sent")
        }
    })?;
    if !response.status().is_success() {
        bail!("model list returned HTTP {}", response.status().as_u16());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("cannot read model list response"))?
    {
        if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
            bail!("model list response exceeds 4 MiB");
        }
        body.extend_from_slice(&chunk);
    }
    let result: ModelList = serde_json::from_slice(&body).map_err(|_| anyhow!("invalid model list response"))?;
    let mut models: Vec<_> = result
        .data
        .into_iter()
        .map(|entry| entry.id)
        .filter(|id| !id.trim().is_empty() && !id.chars().any(char::is_control))
        .collect();
    models.sort_unstable();
    models.dedup();
    Ok(models)
}
