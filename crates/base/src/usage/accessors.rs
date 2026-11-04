use super::provider_fetch::fetch_openai_usage_report;
use super::*;

static OPENAI_USAGE: tokio::sync::OnceCell<Arc<RwLock<OpenAIUsageData>>> = tokio::sync::OnceCell::const_new();
static OPENAI_REFRESH_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

pub(super) async fn get_openai_usage_cell() -> Arc<RwLock<OpenAIUsageData>> {
    OPENAI_USAGE
        .get_or_init(|| async { Arc::new(RwLock::new(OpenAIUsageData::default())) })
        .await
        .clone()
}

async fn fetch_openai_usage_data() -> OpenAIUsageData {
    match fetch_openai_usage_report().await {
        Some(report) => openai_usage_data_from_provider_report(&report),
        None => OpenAIUsageData {
            fetched_at: Some(Instant::now()),
            last_error: Some("No OpenAI/Codex OAuth credentials found".to_string()),
            ..Default::default()
        },
    }
}

async fn refresh_openai_usage(usage: Arc<RwLock<OpenAIUsageData>>) {
    let generation = openai_usage_generation();
    let new_data = fetch_openai_usage_data().await;
    let mut cached = usage.write().await;
    if generation == openai_usage_generation() {
        *cached = new_data;
    }
}

fn try_spawn_openai_refresh(usage: Arc<RwLock<OpenAIUsageData>>) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    if OPENAI_REFRESH_IN_FLIGHT
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    runtime.spawn(async move {
        refresh_openai_usage(usage).await;
        OPENAI_REFRESH_IN_FLIGHT.store(false, Ordering::SeqCst);
    });
}

pub async fn get_openai_usage() -> OpenAIUsageData {
    let usage = get_openai_usage_cell().await;

    let (should_refresh, current_data) = {
        let data = usage.read().await;
        (data.is_stale(), data.clone())
    };

    if should_refresh {
        try_spawn_openai_refresh(usage.clone());
    }

    current_data.display_snapshot()
}

pub fn get_openai_usage_sync() -> OpenAIUsageData {
    if let Some(usage) = OPENAI_USAGE.get()
        && let Ok(data) = usage.try_read()
    {
        if data.is_stale() {
            try_spawn_openai_refresh(usage.clone());
        }
        return data.display_snapshot();
    }

    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(async {
            let _ = get_openai_usage().await;
        });
    }

    OpenAIUsageData::default()
}

pub fn fetch_openai_usage_for_account_sync(
    label: &str,
    email: Option<String>,
    creds: auth::codex::CodexCredentials,
) -> Result<AccountUsageSnapshot> {
    let cache_key = openai_usage_cache_key(&creds.access_token, Some(label));
    if let Some(cached) = cached_openai_usage(&cache_key) {
        return Ok(openai_snapshot_from_usage(label.to_string(), email, &cached));
    }

    if tokio::runtime::Handle::try_current().is_err() {
        anyhow::bail!("OpenAI usage refresh requires a Tokio runtime")
    }

    let generation = openai_usage_generation();
    let report = tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(fetch_openai_usage_for_account(
            openai_provider_display_name(label, email.as_deref(), 2, false),
            creds,
            Some(label),
        ))
    });
    let data = openai_usage_data_from_provider_report(&report);
    anyhow::ensure!(
        generation == openai_usage_generation(),
        "OpenAI usage changed during a banked reset. Retry the usage check."
    );
    Ok(openai_snapshot_from_usage(label.to_string(), email, &data))
}

pub fn account_usage_probe_sync(provider: MultiAccountProviderKind) -> Option<AccountUsageProbe> {
    match provider {
        MultiAccountProviderKind::OpenAI => openai_account_usage_probe_sync(),
    }
}

fn openai_account_usage_probe_sync() -> Option<AccountUsageProbe> {
    let accounts = auth::codex::list_accounts().ok()?;
    if accounts.is_empty() {
        return None;
    }

    let current_label =
        auth::codex::active_account_label().or_else(|| accounts.first().map(|account| account.label.clone()))?;
    let active_cached = get_openai_usage_sync();

    let mut snapshots = Vec::with_capacity(accounts.len());
    for account in &accounts {
        let usage = if account.label == current_label && active_cached.fetched_at.is_some() {
            Ok(openai_snapshot_from_usage(
                account.label.clone(),
                account.email.clone(),
                &active_cached,
            ))
        } else {
            fetch_openai_usage_for_account_sync(
                &account.label,
                account.email.clone(),
                auth::codex::CodexCredentials {
                    access_token: account.access_token.clone(),
                    refresh_token: account.refresh_token.clone(),
                    id_token: account.id_token.clone(),
                    account_id: account.account_id.clone(),
                    expires_at: account.expires_at,
                },
            )
        };

        match usage {
            Ok(snapshot) => snapshots.push(snapshot),
            Err(err) => snapshots.push(AccountUsageSnapshot {
                label: account.label.clone(),
                email: account.email.clone(),
                exhausted: false,
                primary_label: None,
                five_hour_ratio: None,
                secondary_label: None,
                seven_day_ratio: None,
                resets_at: None,
                error: Some(err.to_string()),
            }),
        }
    }

    Some(AccountUsageProbe {
        provider: MultiAccountProviderKind::OpenAI,
        current_label,
        accounts: snapshots,
    })
}
