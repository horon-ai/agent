use super::*;

pub(super) fn usage_percent_from_used_limit(used: f64, limit: f64) -> f32 {
    if !used.is_finite() || !limit.is_finite() || limit <= 0.0 {
        return 0.0;
    }
    ((used.max(0.0) / limit) * 100.0) as f32
}

pub(super) async fn fetch_all_openai_usage_reports() -> Vec<ProviderUsage> {
    let accounts = auth::codex::list_accounts().unwrap_or_default();
    if !accounts.is_empty() {
        let active_label = auth::codex::active_account_label();
        let mut reports = Vec::with_capacity(accounts.len());
        for account in &accounts {
            let display_name = openai_provider_display_name(
                &account.label,
                account.email.as_deref(),
                accounts.len(),
                active_label.as_deref() == Some(&account.label),
            );
            reports.push(
                fetch_openai_usage_for_account(
                    display_name,
                    auth::codex::CodexCredentials {
                        access_token: account.access_token.clone(),
                        refresh_token: account.refresh_token.clone(),
                        id_token: account.id_token.clone(),
                        account_id: account.account_id.clone(),
                        expires_at: account.expires_at,
                    },
                    Some(account.label.as_str()),
                )
                .await,
            );
        }
        return reports;
    }

    let creds = match auth::codex::load_credentials() {
        Ok(creds) => creds,
        Err(_) => return Vec::new(),
    };
    let is_chatgpt = !creds.refresh_token.is_empty() || creds.id_token.is_some();
    if !is_chatgpt || creds.access_token.is_empty() {
        return Vec::new();
    }

    vec![fetch_openai_usage_for_account(openai_provider_display_name("default", None, 1, true), creds, None).await]
}

pub(super) async fn fetch_openai_usage_report() -> Option<ProviderUsage> {
    let reports = fetch_all_openai_usage_reports().await;
    active_openai_usage_report(&reports)
        .cloned()
        .or_else(|| reports.into_iter().next())
}

pub(super) async fn fetch_openai_usage_for_account(
    display_name: String,
    mut creds: auth::codex::CodexCredentials,
    account_label: Option<&str>,
) -> ProviderUsage {
    let generation = openai_usage_generation();
    let is_chatgpt = !creds.refresh_token.is_empty() || creds.id_token.is_some();
    if creds.access_token.is_empty() || !is_chatgpt {
        return ProviderUsage {
            provider_name: display_name,
            error: Some("No OpenAI/Codex OAuth credentials found".to_string()),
            ..Default::default()
        };
    }

    let initial_cache_key = openai_usage_cache_key(&creds.access_token, account_label);
    if let Some(cached) = cached_openai_usage(&initial_cache_key) {
        return provider_report_from_openai_usage_data(display_name, &cached);
    }

    if let Some(expires_at) = creds.expires_at {
        let now = chrono::Utc::now().timestamp_millis();
        if expires_at < now + 300_000 && !creds.refresh_token.is_empty() {
            let refreshed = match account_label {
                Some(label) => crate::auth::oauth::refresh_openai_tokens_for_account(&creds.refresh_token, label).await,
                None => crate::auth::oauth::refresh_openai_tokens(&creds.refresh_token).await,
            };
            match refreshed {
                Ok(refreshed) => {
                    creds.access_token = refreshed.access_token;
                    creds.refresh_token = refreshed.refresh_token;
                    creds.id_token = refreshed.id_token.or(creds.id_token);
                    creds.account_id = creds
                        .account_id
                        .clone()
                        .or_else(|| creds.id_token.as_deref().and_then(auth::codex::extract_account_id));
                    creds.expires_at = Some(refreshed.expires_at);
                }
                Err(e) => {
                    let report = ProviderUsage {
                        provider_name: display_name,
                        error: Some(format!(
                            "Token refresh failed: {} - use `/login openai` to re-authenticate",
                            e
                        )),
                        ..Default::default()
                    };
                    store_openai_usage_for_generation(
                        generation,
                        initial_cache_key,
                        openai_usage_data_from_provider_report(&report),
                    );
                    return report;
                }
            }
        }
    }

    let cache_key = openai_usage_cache_key(&creds.access_token, account_label);
    if cache_key != initial_cache_key
        && let Some(cached) = cached_openai_usage(&cache_key)
    {
        return provider_report_from_openai_usage_data(display_name, &cached);
    }

    let client = crate::provider::shared_http_client();
    let mut builder = client
        .get(OPENAI_USAGE_URL)
        .header("Accept", "application/json")
        .header("Authorization", format!("Bearer {}", creds.access_token));

    if let Some(ref account_id) = creds.account_id {
        builder = builder.header("chatgpt-account-id", account_id);
    }

    let response = match builder.send().await {
        Ok(response) => response,
        Err(e) => {
            let report = ProviderUsage {
                provider_name: display_name,
                error: Some(format!("Failed to fetch: {}", e)),
                ..Default::default()
            };
            store_openai_usage_for_generation(generation, cache_key, openai_usage_data_from_provider_report(&report));
            return report;
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let report = ProviderUsage {
            provider_name: display_name,
            error: Some(format!("API error ({}): {}", status, body)),
            ..Default::default()
        };
        store_openai_usage_for_generation(generation, cache_key, openai_usage_data_from_provider_report(&report));
        return report;
    }

    let body_text = match response.text().await {
        Ok(text) => text,
        Err(e) => {
            let report = ProviderUsage {
                provider_name: display_name,
                error: Some(format!("Failed to read response: {}", e)),
                ..Default::default()
            };
            store_openai_usage_for_generation(generation, cache_key, openai_usage_data_from_provider_report(&report));
            return report;
        }
    };

    let json: serde_json::Value = match serde_json::from_str(&body_text) {
        Ok(value) => value,
        Err(e) => {
            let report = ProviderUsage {
                provider_name: display_name,
                error: Some(format!("Failed to parse response: {}", e)),
                ..Default::default()
            };
            store_openai_usage_for_generation(generation, cache_key, openai_usage_data_from_provider_report(&report));
            return report;
        }
    };

    let parsed = parse_openai_usage_payload(&json);

    // Read-only detail lookup. Failure must not hide otherwise valid usage.
    let available_expirations = if parsed.available_reset_count.is_some_and(|count| count > 0) {
        super::openai_reset::fetch_available_expirations(&client, &creds)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let report = ProviderUsage {
        provider_name: display_name,
        limits: parsed.limits,
        extra_info: parsed.extra_info,
        hard_limit_reached: parsed.hard_limit_reached,
        openai_reset_credits: parsed.available_reset_count.map(|available_count| {
            horon_agent_usage_types::OpenAiResetCredits {
                available_count,
                available_expirations,
                account_label: account_label.map(str::to_string),
                ordinary_usage_allowed: parsed.ordinary_usage_allowed,
            }
        }),
        anthropic_limit_reset: None,
        error: None,
        last_used_unix_secs: None,
    };
    store_openai_usage_for_generation(generation, cache_key, openai_usage_data_from_provider_report(&report));
    report
}

/// HoronAgent subscription: included daily feature allowances plus an upgrade hint
/// when an allowance is running low, so users see the limit before a feature
/// stops working rather than after.
pub(super) async fn fetch_horon_agent_usage_report() -> Option<ProviderUsage> {
    match crate::subscription_api::fetch_subscription_me().await {
        Ok(me) => Some(horon_agent_usage_report(&me)),
        Err(error) => Some(ProviderUsage {
            provider_name: "HoronAgent subscription".to_string(),
            error: Some(format!("{error:#}")),
            ..Default::default()
        }),
    }
}

/// Share of a daily allowance at which HoronAgent starts suggesting an upgrade.
pub(crate) const HORON_AGENT_UPGRADE_HINT_PERCENT: f32 = 80.0;

pub(crate) fn horon_agent_usage_report(me: &crate::subscription_api::SubscriptionMe) -> ProviderUsage {
    let mut limits = Vec::new();
    let mut extra_info = vec![(
        "Plan".to_string(),
        crate::subscription_catalog::HoronAgentTier::parse(&me.tier)
            .map(|tier| tier.display_name().to_string())
            .unwrap_or_else(|| me.tier.clone()),
    )];
    let mut hard_limit_reached = false;
    if let Some(jev) = &me.jev_usage {
        let mut worst = 0.0f32;
        for (label, feature) in [
            ("Memory recall (daily)", &jev.memory),
            ("Browser automation (daily)", &jev.browser),
        ] {
            if feature.limit == 0 {
                continue;
            }
            let percent = usage_percent_from_used_limit(feature.used as f64, feature.limit as f64);
            worst = worst.max(percent);
            hard_limit_reached |= feature.used >= feature.limit;
            limits.push(UsageLimit {
                name: label.to_string(),
                usage_percent: percent,
                resets_at: jev.resets_at.clone(),
            });
        }
        let upgrade_link = jev
            .upgrade_url
            .as_deref()
            .filter(|url| url.starts_with("https://horon-agent.sh/") || url.starts_with("https://www.horon-agent.sh/"));
        if worst >= HORON_AGENT_UPGRADE_HINT_PERCENT
            && let (Some(tier), Some(url)) = (jev.upgrade_tier.as_deref(), upgrade_link)
        {
            let name = crate::subscription_catalog::HoronAgentTier::parse(tier)
                .map(|tier| tier.display_name().to_string())
                .unwrap_or_else(|| tier.to_string());
            extra_info.push(("Upgrade".to_string(), format!("{name} raises daily limits: {url}")));
        }
    }
    ProviderUsage {
        provider_name: "HoronAgent subscription".to_string(),
        limits,
        extra_info,
        hard_limit_reached,
        ..Default::default()
    }
}
