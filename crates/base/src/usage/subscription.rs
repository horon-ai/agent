use super::*;

pub(crate) fn usage_percent_from_used_limit(used: f64, limit: f64) -> f32 {
    if !used.is_finite() || !limit.is_finite() || limit <= 0.0 {
        return 0.0;
    }
    ((used.max(0.0) / limit) * 100.0) as f32
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
