use super::*;

#[test]
fn test_usage_data_default() {
    let data = UsageData::default();
    assert!(data.is_stale());
    assert_eq!(data.five_hour_percent(), "0%");
    assert_eq!(data.seven_day_percent(), "0%");
}

#[test]
fn test_usage_percent_format() {
    let data = UsageData {
        five_hour: 0.42,
        seven_day: 0.156,
        ..Default::default()
    };
    assert_eq!(data.five_hour_percent(), "42%");
    assert_eq!(data.seven_day_percent(), "16%");
}

#[test]
fn test_usage_data_becomes_stale_when_reset_time_has_passed() {
    let data = UsageData {
        five_hour: 0.42,
        five_hour_resets_at: Some("2020-01-01T00:00:00Z".to_string()),
        fetched_at: Some(Instant::now()),
        ..Default::default()
    };

    assert!(
        data.is_stale(),
        "usage data should refresh once a reset window has passed"
    );
}

#[test]
fn test_openai_usage_data_becomes_stale_when_reset_time_has_passed() {
    let data = OpenAIUsageData {
        five_hour: Some(OpenAIUsageWindow {
            name: "5-hour".to_string(),
            usage_ratio: 0.42,
            resets_at: Some("2020-01-01T00:00:00Z".to_string()),
        }),
        fetched_at: Some(Instant::now()),
        ..Default::default()
    };

    assert!(
        data.is_stale(),
        "OpenAI usage data should refresh once a reset window has passed"
    );
}

#[test]
fn test_usage_data_display_snapshot_clears_passed_reset_window() {
    let data = UsageData {
        five_hour: 0.73,
        five_hour_resets_at: Some("2020-01-01T00:00:00Z".to_string()),
        seven_day: 0.41,
        seven_day_resets_at: Some("3020-01-01T00:00:00Z".to_string()),
        fetched_at: Some(Instant::now()),
        ..Default::default()
    };

    let snapshot = data.display_snapshot();
    assert_eq!(snapshot.five_hour, 0.0);
    assert!(snapshot.five_hour_resets_at.is_none());
    assert_eq!(snapshot.seven_day, 0.41);
    assert_eq!(snapshot.seven_day_resets_at.as_deref(), Some("3020-01-01T00:00:00Z"));
}

#[test]
fn test_openai_usage_data_display_snapshot_clears_passed_reset_window() {
    let data = OpenAIUsageData {
        five_hour: Some(OpenAIUsageWindow {
            name: "5-hour".to_string(),
            usage_ratio: 0.88,
            resets_at: Some("2020-01-01T00:00:00Z".to_string()),
        }),
        seven_day: Some(OpenAIUsageWindow {
            name: "7-day".to_string(),
            usage_ratio: 0.31,
            resets_at: Some("3020-01-01T00:00:00Z".to_string()),
        }),
        hard_limit_reached: true,
        fetched_at: Some(Instant::now()),
        ..Default::default()
    };

    let snapshot = data.display_snapshot();
    assert_eq!(snapshot.five_hour.as_ref().map(|w| w.usage_ratio), Some(0.0));
    assert_eq!(snapshot.five_hour.as_ref().and_then(|w| w.resets_at.as_deref()), None);
    assert_eq!(snapshot.seven_day.as_ref().map(|w| w.usage_ratio), Some(0.31));
    assert!(!snapshot.hard_limit_reached);
}

#[test]
fn test_provider_usage_cache_is_not_fresh_after_reset_boundary() {
    let report = ProviderUsage {
        provider_name: "OpenAI".to_string(),
        limits: vec![UsageLimit {
            name: "5-hour window".to_string(),
            usage_percent: 100.0,
            resets_at: Some("2020-01-01T00:00:00Z".to_string()),
        }],
        ..Default::default()
    };

    assert!(!provider_usage_cache_is_fresh(Instant::now(), Instant::now(), &report,));
}

#[test]
fn test_format_usage_bar() {
    let bar = format_usage_bar(50.0, 10);
    assert!(bar.contains("█████░░░░░"));
    assert!(bar.contains("50%"));

    let bar = format_usage_bar(0.0, 10);
    assert!(bar.contains("░░░░░░░░░░"));
    assert!(bar.contains("0%"));

    let bar = format_usage_bar(100.0, 10);
    assert!(bar.contains("██████████"));
    assert!(bar.contains("100%"));
}

#[test]
fn test_format_reset_time_past() {
    assert_eq!(format_reset_time("2020-01-01T00:00:00Z"), "now");
}

#[test]
fn test_format_reset_time_under_one_minute_rounds_up() {
    let timestamp = (chrono::Utc::now() + chrono::TimeDelta::seconds(30)).to_rfc3339();
    assert_eq!(format_reset_time(&timestamp), "1m");
}

#[test]
fn test_format_reset_time_uses_days_for_long_windows() {
    let timestamp = (chrono::Utc::now() + chrono::TimeDelta::hours(109) + chrono::TimeDelta::minutes(5)).to_rfc3339();
    assert_eq!(format_reset_time(&timestamp), "4d 13h");
}

#[test]
fn test_account_usage_probe_prefers_best_available_alternative() {
    let probe = AccountUsageProbe {
        provider: MultiAccountProviderKind::OpenAI,
        current_label: "work".to_string(),
        accounts: vec![
            AccountUsageSnapshot {
                label: "work".to_string(),
                email: Some("work@example.com".to_string()),
                exhausted: true,
                primary_label: None,
                five_hour_ratio: Some(1.0),
                secondary_label: None,
                seven_day_ratio: Some(1.0),
                resets_at: Some("2026-01-01T00:00:00Z".to_string()),
                error: None,
            },
            AccountUsageSnapshot {
                label: "backup".to_string(),
                email: Some("backup@example.com".to_string()),
                exhausted: false,
                primary_label: None,
                five_hour_ratio: Some(0.45),
                secondary_label: None,
                seven_day_ratio: Some(0.10),
                resets_at: Some("2026-01-01T01:00:00Z".to_string()),
                error: None,
            },
            AccountUsageSnapshot {
                label: "secondary".to_string(),
                email: Some("secondary@example.com".to_string()),
                exhausted: false,
                primary_label: None,
                five_hour_ratio: Some(0.70),
                secondary_label: None,
                seven_day_ratio: Some(0.20),
                resets_at: Some("2026-01-01T02:00:00Z".to_string()),
                error: None,
            },
        ],
    };

    let best = probe
        .best_available_alternative()
        .expect("expected alternative account");
    assert_eq!(best.label, "backup");

    let guidance = probe.switch_guidance().expect("expected switch guidance");
    assert!(guidance.contains("`backup`"));
    assert!(guidance.contains("/account openai switch backup"));
}

#[test]
fn test_reports_sort_most_recently_used_first() {
    let report = |name: &str, last_used: Option<u64>| ProviderUsage {
        provider_name: name.to_string(),
        last_used_unix_secs: last_used,
        ..Default::default()
    };

    let mut results = vec![
        report("Never Used B", None),
        report("Old", Some(100)),
        report("Never Used A", None),
        report("Recent", Some(2_000_000)),
    ];
    sort_reports_most_recent_first(&mut results);

    let names: Vec<&str> = results.iter().map(|r| r.provider_name.as_str()).collect();
    assert_eq!(names, ["Recent", "Old", "Never Used A", "Never Used B"]);
}

#[test]
fn test_activity_sweeper_skips_sources_with_dedicated_reports() {
    // Direct OpenAI-compatible profiles are reported by the API-key module;
    // every other source falls through to the sweeper.
    assert!(!activity_source_has_dedicated_report("claude:oauth:claude-1"));
    assert!(!activity_source_has_dedicated_report("openai:oauth:openai-1"));
    assert!(!activity_source_has_dedicated_report("bedrock"));
    assert!(!activity_source_has_dedicated_report("some-custom-endpoint"));
}

#[test]
fn anthropic_model_scoped_exhaustion_matches_display_name_to_catalog_id() {
    let usage = UsageData {
        model_scoped: vec![ModelScopedUsageWindow {
            model_name: "Fable".to_string(),
            utilization: 1.0,
            resets_at: Some("2026-08-11T00:00:00Z".to_string()),
        }],
        ..Default::default()
    };

    assert!(usage.model_scoped_exhausted("claude-fable-5"));
    assert!(!usage.model_scoped_exhausted("claude-opus-5"));

    let below_limit = UsageData {
        model_scoped: vec![ModelScopedUsageWindow {
            model_name: "Claude Fable 5".to_string(),
            utilization: 0.98,
            resets_at: None,
        }],
        ..Default::default()
    };
    assert!(!below_limit.model_scoped_exhausted("claude-fable-5"));
}

#[test]
fn kimi_usage_limits_shape_parses_windowed_quotas() {
    let json = serde_json::json!({
        "limits": [
            {"window": {"duration": 5, "timeUnit": "HOUR"},
             "detail": {"limit": 100.0, "used": 8.0, "percentage": 8.0, "resetTime": "2026-10-04T10:00:00Z"}},
            {"window": {"duration": 7, "timeUnit": "DAY"},
             "detail": {"limit": 100.0, "remaining": 28.0, "resetTime": "2026-10-08T00:00:00Z"}},
            {"window": {"duration": 30, "timeUnit": "DAY"},
             "detail": {"limit": 100.0, "used": 45.0, "resetTime": "2026-11-01T00:00:00Z"}}
        ]
    });
    let limits = api_keys::parse_kimi_usage_limits(&json);
    assert_eq!(limits.len(), 3);
    // Windows rank 5-hour < Weekly < Monthly regardless of response order.
    assert_eq!(limits[0].name, "5-hour");
    assert!((limits[0].usage_percent - 8.0).abs() < f32::EPSILON);
    assert_eq!(limits[0].resets_at.as_deref(), Some("2026-10-04T10:00:00Z"));
    assert_eq!(limits[1].name, "Weekly");
    // Remaining-derived usage: 100 - 28 = 72%.
    assert!((limits[1].usage_percent - 72.0).abs() < 0.01);
    assert_eq!(limits[2].name, "Monthly");
    assert!((limits[2].usage_percent - 45.0).abs() < f32::EPSILON);
}

#[test]
fn kimi_usage_legacy_usages_map_shape() {
    let json = serde_json::json!({
        "usages": {
            "limit_5h": {"used_ratio": 0.08, "reset_time": "2026-10-04T10:00:00Z"},
            "limit_7d": {"used_ratio": 0.92, "reset_time": "2026-10-08T00:00:00Z"},
            "limit_30d": {"used_ratio": 0.10}
        }
    });
    let limits = api_keys::parse_kimi_usage_limits(&json);
    assert_eq!(limits.len(), 3);
    assert_eq!(limits[0].name, "5-hour");
    assert!((limits[0].usage_percent - 8.0).abs() < 0.01);
    assert_eq!(limits[1].name, "Weekly");
    assert!((limits[1].usage_percent - 92.0).abs() < 0.01);
    assert_eq!(limits[1].resets_at.as_deref(), Some("2026-10-08T00:00:00Z"));
    assert_eq!(limits[2].name, "Monthly");
    assert_eq!(limits[2].resets_at, None);
}

#[test]
fn kimi_usage_summary_row_falls_back_to_weekly_label() {
    let json = serde_json::json!({
        "usage": {"limit": 100.0, "used": 72.0, "resetTime": "2026-10-08T00:00:00Z"}
    });
    let limits = api_keys::parse_kimi_usage_limits(&json);
    assert_eq!(limits.len(), 1);
    assert_eq!(limits[0].name, "Weekly");
    assert!((limits[0].usage_percent - 72.0).abs() < 0.01);
}

#[test]
fn reset_timestamp_from_json_value_handles_all_encodings() {
    // protobuf int64 JSON-encodes as a decimal string of milliseconds.
    let millis = serde_json::json!("1792835722000");
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(&reset_timestamp_from_json_value(&millis).unwrap())
            .unwrap()
            .timestamp(),
        1792835722
    );
    let seconds = serde_json::json!(1792835722u64);
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(&reset_timestamp_from_json_value(&seconds).unwrap())
            .unwrap()
            .timestamp(),
        1792835722
    );
    let iso = serde_json::json!("2026-10-24T10:15:22Z");
    assert_eq!(
        reset_timestamp_from_json_value(&iso).as_deref(),
        Some("2026-10-24T10:15:22Z")
    );
    assert_eq!(reset_timestamp_from_json_value(&serde_json::json!(0)), None);
}

#[test]
fn zai_coding_plan_limits_parse_token_and_time_windows() {
    let json = serde_json::json!({
        "data": {
            "limits": [
                {"type": "TOKENS_LIMIT", "unit": 6, "percentage": 72.0, "nextResetTime": "2026-10-08T00:00:00Z"},
                {"type": "TOKENS_LIMIT", "unit": 5, "percentage": 4.8, "nextResetTime": "2026-10-04T10:00:00Z"},
                {"type": "TIME_LIMIT", "currentValue": 3.0, "usage": 20.0, "nextResetTime": "2026-11-01T00:00:00Z"}
            ]
        }
    });
    let limits = api_keys::parse_zai_coding_plan_limits(&json);
    assert_eq!(limits.len(), 3);
    // 5-hour ranks before Weekly before MCP monthly.
    assert_eq!(limits[0].name, "5-hour");
    assert!((limits[0].usage_percent - 4.8).abs() < 0.01);
    assert_eq!(limits[0].resets_at.as_deref(), Some("2026-10-04T10:00:00Z"));
    assert_eq!(limits[1].name, "Weekly");
    assert!((limits[1].usage_percent - 72.0).abs() < f32::EPSILON);
    assert_eq!(limits[2].name, "MCP monthly");
    // TIME_LIMIT without percentage falls back to currentValue/usage.
    assert!((limits[2].usage_percent - 15.0).abs() < 0.01);
}

#[test]
fn zai_coding_plan_limits_tolerate_unwrapped_payload() {
    let json = serde_json::json!({
        "limits": [
            {"type": "TOKENS_LIMIT", "unit": 6, "percentage": 10.0}
        ]
    });
    let limits = api_keys::parse_zai_coding_plan_limits(&json);
    assert_eq!(limits.len(), 1);
    assert_eq!(limits[0].name, "Weekly");
    assert_eq!(limits[0].resets_at, None);
    // Unknown rows and empty payloads yield no limits (caller falls back).
    let empty = api_keys::parse_zai_coding_plan_limits(&serde_json::json!({"data": {"limits": []}}));
    assert!(empty.is_empty());
}

#[test]
fn kimi_usage_explicit_weekly_window_beats_summary() {
    let json = serde_json::json!({
        "usage": {"limit": 100.0, "used": 12.0, "resetTime": "2026-10-08T00:00:00Z"},
        "limits": [
            {"window": {"duration": 7, "timeUnit": "DAY"},
             "detail": {"limit": 100.0, "used": 72.0, "resetTime": "2026-10-09T00:00:00Z"}}
        ]
    });
    let limits = api_keys::parse_kimi_usage_limits(&json);
    assert_eq!(limits.len(), 1);
    assert_eq!(limits[0].name, "Weekly");
    assert!((limits[0].usage_percent - 72.0).abs() < 0.01);
    assert_eq!(limits[0].resets_at.as_deref(), Some("2026-10-09T00:00:00Z"));
}

#[test]
fn zai_quota_url_follows_profile_region() {
    assert_eq!(
        api_keys::zai_quota_url("https://api.z.ai/api/coding/paas/v4"),
        "https://api.z.ai/api/monitor/usage/quota/limit"
    );
    assert_eq!(
        api_keys::zai_quota_url("https://open.bigmodel.cn/api/coding/paas/v4"),
        "https://open.bigmodel.cn/api/monitor/usage/quota/limit"
    );
    // Custom proxies and unparsable bases default to the international host.
    assert_eq!(
        api_keys::zai_quota_url("https://proxy.example.com/v1"),
        "https://api.z.ai/api/monitor/usage/quota/limit"
    );
    assert_eq!(
        api_keys::zai_quota_url("not a url"),
        "https://api.z.ai/api/monitor/usage/quota/limit"
    );
}
