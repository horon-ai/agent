use super::*;

pub(super) fn auto_scriptable_flow_reason(
    provider: LoginProviderDescriptor,
    options: &LoginOptions,
    stdin_is_terminal: bool,
) -> Option<&'static str> {
    if options.print_auth_url || options.complete || options.has_provided_input() {
        return None;
    }

    let supports_scriptable = matches!(provider.target, LoginProviderTarget::OpenAi);
    if !supports_scriptable {
        return None;
    }
    if options.json {
        return Some("json output requested");
    }
    if !stdin_is_terminal {
        return Some("stdin is not a terminal");
    }
    if auth::browser_suppressed(options.no_browser) {
        return Some("no_browser_requested");
    }
    None
}

pub(super) async fn run_scriptable_login_provider(
    provider: LoginProviderDescriptor,
    account_label: Option<&str>,
    options: &LoginOptions,
) -> Result<LoginFlowOutcome> {
    if options.print_auth_url || (options.flow_id.is_some() && !options.complete && !options.has_provided_input()) {
        return start_scriptable_login(provider, account_label, options).await;
    }

    let input = options.resolve_provided_input()?;
    if options.complete && input.is_some() {
        anyhow::bail!("Use either --complete or an explicit --callback-url / --auth-code input, not both.");
    }
    complete_scriptable_login(provider, account_label, options, input).await
}

pub(super) async fn start_scriptable_login(
    provider: LoginProviderDescriptor,
    account_label: Option<&str>,
    options: &LoginOptions,
) -> Result<LoginFlowOutcome> {
    let (pending, auth_url, input_kind, user_code, expires_at_ms) = match provider.target {
        LoginProviderTarget::OpenAi => {
            let label = auth::codex::login_target_label(account_label)?;
            let (verifier, challenge) = auth::oauth::generate_pkce_public();
            let state = auth::oauth::generate_state_public();
            let redirect_uri = auth::oauth::openai::default_redirect_uri();
            let auth_url = auth::oauth::openai_auth_url_with_prompt(&redirect_uri, &challenge, &state, Some("login"));
            (
                PendingScriptableLogin::Openai {
                    account_label: label,
                    verifier,
                    state,
                    redirect_uri,
                },
                auth_url,
                "callback_url",
                None::<String>,
                PendingScriptableLogin::Openai {
                    account_label: String::new(),
                    verifier: String::new(),
                    state: String::new(),
                    redirect_uri: String::new(),
                }
                .default_expires_at_ms(),
            )
        }
        _ => {
            anyhow::bail!(
                "`--print-auth-url` is currently supported for: claude, openai, gemini, antigravity, copilot."
            )
        }
    };

    let pending_path = pending.pending_path(options.flow_id.as_deref())?;
    cleanup_stale_pending_login_files()?;
    let record = PendingScriptableLoginRecord {
        expires_at_ms,
        login: pending,
    };
    persist_pending_login(&pending_path, &record, options.flow_id.is_some())?;
    emit_scriptable_auth_prompt(
        provider.id,
        &auth_url,
        input_kind,
        &pending_path,
        user_code.as_deref(),
        expires_at_ms,
        options.json,
        options.flow_id.as_deref(),
    )?;
    // Browserless CLI login routes here rather than through the interactive
    // provider functions. Keep stdout machine-readable and never add a QR to
    // JSON output, but offer it on stderr for human-readable headless login.
    if !options.json
        && let Some(qr) = super::presenter::url_section(
            &auth_url,
            "Scan this QR on another device to sign in:",
            "    ",
            auth::browser_suppressed(options.no_browser),
        )
    {
        eprintln!("{qr}");
    }
    Ok(LoginFlowOutcome::Deferred)
}

pub(super) async fn complete_scriptable_login(
    provider: LoginProviderDescriptor,
    account_label: Option<&str>,
    options: &LoginOptions,
    input: Option<ProvidedAuthInput>,
) -> Result<LoginFlowOutcome> {
    if account_label.is_some() {
        anyhow::bail!(
            "Do not pass --account when completing a scriptable login. The pending login already stores the target account."
        );
    }

    match provider.target {
        LoginProviderTarget::OpenAi => {
            complete_scriptable_openai_login(provider.id, options, require_scriptable_input(input)?).await
        }
        _ => anyhow::bail!(
            "Scriptable completion is currently supported for: claude, openai, gemini, antigravity, copilot."
        ),
    }
}

pub(super) async fn complete_scriptable_openai_login(
    provider_id: &str,
    options: &LoginOptions,
    input: ProvidedAuthInput,
) -> Result<LoginFlowOutcome> {
    let pending_path = pending_login_path("openai", options.flow_id.as_deref())?;
    let PendingScriptableLogin::Openai {
        account_label,
        verifier,
        state,
        redirect_uri,
    } = load_pending_login(&pending_path, "openai")?
    else {
        anyhow::bail!("Pending OpenAI login state is invalid.");
    };

    let callback_input = match input {
        ProvidedAuthInput::CallbackUrl(value) => value,
        ProvidedAuthInput::AuthCode => {
            anyhow::bail!("OpenAI completion requires --callback-url because state validation is required.")
        }
    };
    let tokens = auth::oauth::exchange_openai_callback_input(&verifier, &callback_input, &state, &redirect_uri).await?;
    auth::oauth::save_openai_tokens_for_account(&tokens, &account_label)?;
    clear_pending_login(&pending_path);
    let credentials_path = crate::storage::horon_agent_dir()?.join("openai-auth.json");
    emit_scriptable_auth_success(
        options.json,
        ScriptableAuthSuccess {
            status: "authenticated",
            provider: provider_id.to_string(),
            account_label: Some(account_label.clone()),
            credentials_path: Some(credentials_path.display().to_string()),
            email: None,
        },
    )?;
    if !options.json {
        eprintln!(
            "Successfully logged in to OpenAI! Account '{}' saved to {}",
            account_label,
            credentials_path.display()
        );
    }
    Ok(LoginFlowOutcome::Completed)
}

pub(super) fn pending_login_path(key: &str, flow_id: Option<&str>) -> Result<PathBuf> {
    pending_login_path_in(&pending_login_dir()?, key, flow_id)
}

pub(super) fn pending_login_path_in(dir: &Path, key: &str, flow_id: Option<&str>) -> Result<PathBuf> {
    anyhow::ensure!(
        matches!(key, "claude" | "openai" | "gemini" | "antigravity" | "copilot"),
        "Unsupported scriptable login provider."
    );
    let dir = match flow_id {
        Some(flow_id) => {
            parse_login_flow_id(flow_id).map_err(anyhow::Error::msg)?;
            dir.join("flows").join(flow_id)
        }
        None => dir.to_path_buf(),
    };
    Ok(dir.join(format!("{key}.json")))
}

pub(super) fn cancel_scriptable_login(provider: LoginProviderDescriptor, options: &LoginOptions) -> Result<()> {
    let flow_id = options.flow_id.as_deref().context("--cancel requires --flow-id.")?;
    let key = match provider.target {
        LoginProviderTarget::OpenAi => "openai",
        _ => anyhow::bail!("This provider does not support scriptable login cancellation."),
    };
    let path = pending_login_path(key, Some(flow_id))?;
    cancel_scoped_pending_login(&path)?;
    if options.json {
        println!(
            "{}",
            serde_json::json!({
                "status": "cancelled", "provider": provider.id, "flow_id": flow_id
            })
        );
    } else {
        eprintln!("Cancelled pending {} login flow {}.", provider.id, flow_id);
    }
    Ok(())
}

pub(super) fn cancel_pending_login(path: &Path) -> Result<()> {
    // Do not sweep expired files or touch credential stores during cancellation.
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).context("Failed to cancel pending login flow"),
    }
}

const CANCELLED_FLOW_TTL_MS: i64 = 30 * 60 * 1000;

fn lock_pending_login(path: &Path) -> Result<std::fs::File> {
    let parent = path.parent().context("Pending login path has no parent")?;
    crate::storage::ensure_dir(parent)?;
    let lock_path = path.with_extension("lock");
    let mut options = std::fs::OpenOptions::new();
    options.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&lock_path).context("Failed to open pending login lock")?;
    crate::storage::harden_secret_file_permissions(&lock_path);
    file.lock().context("Failed to lock pending login flow")?;
    // Keep the lock inode stable. Removing it could split simultaneous waiters.
    Ok(file)
}

pub(super) fn persist_pending_login(path: &Path, record: &PendingScriptableLoginRecord, scoped: bool) -> Result<()> {
    if !scoped {
        return crate::storage::write_json_secret(path, record);
    }
    let _lock = lock_pending_login(path)?;
    let marker = path.with_extension("cancelled");
    match std::fs::read_to_string(&marker) {
        Ok(data) => {
            let expires_at_ms: i64 =
                serde_json::from_str(&data).context("Invalid pending login cancellation marker")?;
            anyhow::ensure!(
                expires_at_ms <= current_time_ms(),
                "This login flow was cancelled. Start again with a fresh --flow-id."
            );
            std::fs::remove_file(&marker).context("Failed to expire login cancellation marker")?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("Failed to read login cancellation marker"),
    }
    anyhow::ensure!(
        record.expires_at_ms > current_time_ms(),
        "Pending login flow expired before it could be saved."
    );
    crate::storage::write_json_secret(path, record)
}

pub(super) fn cancel_scoped_pending_login(path: &Path) -> Result<()> {
    let _lock = lock_pending_login(path)?;
    // No secret data: the marker only fences late begin writers for the flow TTL.
    // Cancellation intentionally does not roll back credentials already exchanged.
    crate::storage::write_json_secret(
        &path.with_extension("cancelled"),
        &(current_time_ms() + CANCELLED_FLOW_TTL_MS),
    )?;
    cancel_pending_login(path)
}

pub(super) fn pending_login_dir() -> Result<PathBuf> {
    Ok(crate::storage::horon_agent_dir()?.join("pending-login"))
}

pub(super) fn require_scriptable_input(input: Option<ProvidedAuthInput>) -> Result<ProvidedAuthInput> {
    input.ok_or_else(|| anyhow::anyhow!("No scriptable auth input was provided."))
}

pub(super) fn load_pending_login(path: &PathBuf, provider: &str) -> Result<PendingScriptableLogin> {
    if !path.exists() {
        anyhow::bail!(
            "No pending {} login state found. Run `horon-agent login --provider {} --print-auth-url` first.",
            provider,
            provider
        );
    }
    crate::storage::harden_secret_file_permissions(path);
    let data = std::fs::read_to_string(path).with_context(|| {
        format!(
            "Failed to read pending {} login state from {}",
            provider,
            path.display()
        )
    })?;
    if let Ok(record) = serde_json::from_str::<PendingScriptableLoginRecord>(&data) {
        if record.expires_at_ms <= current_time_ms() {
            clear_pending_login(path);
            anyhow::bail!(
                "Pending {} login state expired. Run `horon-agent login --provider {} --print-auth-url` again.",
                provider,
                provider
            );
        }
        cleanup_stale_pending_login_files()?;
        return Ok(record.login);
    }
    let login = serde_json::from_str::<PendingScriptableLogin>(&data).with_context(|| {
        format!(
            "Failed to load pending {} login state from {}",
            provider,
            path.display()
        )
    })?;
    cleanup_stale_pending_login_files()?;
    Ok(login)
}

pub(super) fn clear_pending_login(path: &PathBuf) {
    let _ = std::fs::remove_file(path);
}

pub(super) fn cleanup_stale_pending_login_files() -> Result<()> {
    let dir = pending_login_dir()?;
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&dir)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(data) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<PendingScriptableLoginRecord>(&data) else {
            continue;
        };
        if record.expires_at_ms <= current_time_ms() {
            let _ = std::fs::remove_file(&path);
        }
    }
    Ok(())
}

pub(super) fn current_time_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub(super) fn resolve_auth_input(value: &str) -> Result<String> {
    if value != "-" {
        return Ok(value.to_string());
    }

    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("Failed to read auth input from stdin")?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        anyhow::bail!("No auth input was provided on stdin.");
    }
    Ok(trimmed.to_string())
}

// Single caller; the fields map 1:1 onto the emitted prompt payload.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_scriptable_auth_prompt(
    provider: &str,
    auth_url: &str,
    input_kind: &str,
    pending_path: &Path,
    user_code: Option<&str>,
    expires_at_ms: i64,
    json: bool,
    flow_id: Option<&str>,
) -> Result<()> {
    let resume_command = scriptable_resume_command(provider, input_kind, flow_id);
    let prompt = ScriptableAuthPrompt {
        status: "pending",
        provider: provider.to_string(),
        auth_url: auth_url.to_string(),
        input_kind: input_kind.to_string(),
        pending_path: pending_path.display().to_string(),
        user_code: user_code.map(str::to_string),
        expires_at_ms,
        resume_command: resume_command.clone(),
    };
    if json {
        println!("{}", serde_json::to_string(&prompt)?);
    } else {
        println!("{}", auth_url);
        if let Some(user_code) = user_code {
            eprintln!("User code: {}", user_code);
        }
        eprintln!("Auth URL printed to stdout.");
        eprintln!("Complete this login later with `{}`.", resume_command);
        eprintln!("This pending login expires at {} ms since epoch.", expires_at_ms);
        eprintln!("Pending login state saved at {}", pending_path.display());
    }
    Ok(())
}

pub(super) fn scriptable_resume_command(provider: &str, input_kind: &str, flow_id: Option<&str>) -> String {
    let provider = match flow_id {
        Some(flow_id) => format!("{provider} --flow-id {flow_id}"),
        None => provider.to_string(),
    };
    match input_kind {
        "callback_url" => {
            format!(
                "horon-agent login --provider {} --callback-url '<url-or-query>'",
                provider
            )
        }
        "auth_code" => format!("horon-agent login --provider {} --auth-code '<code>'", provider),
        "complete" => format!("horon-agent login --provider {} --complete", provider),
        _ => format!(
            "horon-agent login --provider {} --callback-url '<url>'  # or --auth-code '<code>'",
            provider
        ),
    }
}

pub(super) fn emit_scriptable_auth_success(json: bool, success: ScriptableAuthSuccess) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(&success)?);
    }
    Ok(())
}
