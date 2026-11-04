use anyhow::Result;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::Duration;

/// Claude Code OAuth configuration
/// OpenAI Codex OAuth configuration
pub mod openai {
    pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
    pub const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
    pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
    pub const DEFAULT_PORT: u16 = 1455;
    pub const CALLBACK_PATH: &str = "/auth/callback";
    pub const SCOPES: &str = "openid profile email offline_access api.connectors.read api.connectors.invoke";

    pub fn redirect_uri(port: u16) -> String {
        format!("http://localhost:{}{}", port, CALLBACK_PATH)
    }

    pub fn default_redirect_uri() -> String {
        redirect_uri(DEFAULT_PORT)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
}

fn generate_pkce() -> (String, String) {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    let verifier: String = (0..64)
        .map(|_| {
            let idx = rng.random_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect();

    let mut hasher = Sha256::new();
    hasher.update(verifier.as_bytes());
    let hash = hasher.finalize();
    let challenge = URL_SAFE_NO_PAD.encode(hash);

    (verifier, challenge)
}

/// Generate random state for CSRF protection
fn generate_state() -> String {
    let bytes: [u8; 16] = rand::random();
    hex::encode(bytes)
}

pub fn generate_pkce_public() -> (String, String) {
    generate_pkce()
}

pub fn generate_state_public() -> String {
    generate_state()
}

const CALLBACK_READ_TIMEOUT_SECS: u64 = 5;

fn bad_request_response(message: &str) -> String {
    let body = format!(
        "<html><body><h1>Authentication not completed</h1><p>{}</p><p>You can close this tab and return to horon-agent.</p></body></html>",
        message
    );
    format!(
        "HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}

fn is_socket_read_timeout(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    )
}

fn read_http_request_line_blocking<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let mut request_line = String::new();
    match reader.read_line(&mut request_line) {
        Ok(0) => Ok(None),
        Ok(_) => Ok(Some(request_line)),
        Err(err) if is_socket_read_timeout(&err) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn drain_http_headers_blocking<R: BufRead>(reader: &mut R) -> Result<bool> {
    let mut header_line = String::new();
    loop {
        header_line.clear();
        match reader.read_line(&mut header_line) {
            Ok(0) => return Ok(false),
            Ok(_) if header_line.trim().is_empty() => return Ok(true),
            Ok(_) => {}
            Err(err) if is_socket_read_timeout(&err) => return Ok(false),
            Err(err) => return Err(err.into()),
        }
    }
}

async fn read_http_request_line_async<R>(reader: &mut tokio::io::BufReader<R>) -> Result<Option<String>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut request_line = String::new();
    match tokio::time::timeout(
        Duration::from_secs(CALLBACK_READ_TIMEOUT_SECS),
        tokio::io::AsyncBufReadExt::read_line(reader, &mut request_line),
    )
    .await
    {
        Ok(Ok(0)) => Ok(None),
        Ok(Ok(_)) => Ok(Some(request_line)),
        Ok(Err(err)) => Err(err.into()),
        Err(_) => Ok(None),
    }
}

async fn drain_http_headers_async<R>(reader: &mut tokio::io::BufReader<R>) -> Result<bool>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut header_line = String::new();
    loop {
        header_line.clear();
        match tokio::time::timeout(
            Duration::from_secs(CALLBACK_READ_TIMEOUT_SECS),
            tokio::io::AsyncBufReadExt::read_line(reader, &mut header_line),
        )
        .await
        {
            Ok(Ok(0)) => return Ok(false),
            Ok(Ok(_)) if header_line.trim().is_empty() => return Ok(true),
            Ok(Ok(_)) => {}
            Ok(Err(err)) => return Err(err.into()),
            Err(_) => return Ok(false),
        }
    }
}

/// Start local server and wait for OAuth callback
pub fn wait_for_callback(port: u16, expected_state: &str) -> Result<String> {
    let listener = TcpListener::bind(format!("127.0.0.1:{}", port))?;
    eprintln!("Waiting for OAuth callback on port {}...", port);

    loop {
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(CALLBACK_READ_TIMEOUT_SECS)))?;
        let mut reader = BufReader::new(&stream);
        let Some(request_line) = read_http_request_line_blocking(&mut reader)? else {
            continue;
        };
        if !drain_http_headers_blocking(&mut reader)? {
            continue;
        }

        let parts: Vec<&str> = request_line.split_whitespace().collect();
        if parts.len() < 2 {
            let _ = stream.write_all(bad_request_response("Invalid HTTP request.").as_bytes());
            continue;
        }

        let path = parts[1];
        let url = match url::Url::parse(&format!("http://localhost{}", path)) {
            Ok(url) => url,
            Err(_) => {
                let _ = stream.write_all(bad_request_response("Could not parse OAuth callback URL.").as_bytes());
                continue;
            }
        };

        if let Some(error) = url
            .query_pairs()
            .find(|(k, _)| k == "error")
            .map(|(_, v)| v.to_string())
        {
            let _ = stream.write_all(bad_request_response("Authentication was denied or cancelled.").as_bytes());
            anyhow::bail!("OAuth provider returned error: {}", error);
        }

        let code = match url.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v.to_string()) {
            Some(code) => code,
            None => {
                let _ = stream
                    .write_all(bad_request_response("No authorization code was included in this request.").as_bytes());
                continue;
            }
        };

        let state = match url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string())
        {
            Some(state) => state,
            None => {
                let _ =
                    stream.write_all(bad_request_response("No OAuth state was included in this request.").as_bytes());
                continue;
            }
        };

        if state != expected_state {
            let _ = stream.write_all(
                bad_request_response("OAuth state mismatch. Please retry the latest login flow.").as_bytes(),
            );
            continue;
        }

        let body = "<html><body><h1>Success!</h1><p>You can close this window.</p></body></html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(response.as_bytes())?;

        return Ok(code);
    }
}

/// Async version of wait_for_callback using tokio (for use from TUI context)
pub async fn wait_for_callback_async(port: u16, expected_state: &str) -> Result<String> {
    let listener = bind_callback_listener(port)?;
    wait_for_callback_async_on_listener(listener, expected_state).await
}

pub fn bind_callback_listener(port: u16) -> Result<tokio::net::TcpListener> {
    let std_listener = std::net::TcpListener::bind(format!("127.0.0.1:{port}"))?;
    std_listener.set_nonblocking(true)?;
    Ok(tokio::net::TcpListener::from_std(std_listener)?)
}

pub async fn wait_for_callback_async_on_listener(
    listener: tokio::net::TcpListener,
    expected_state: &str,
) -> Result<String> {
    let expected_state = expected_state.to_string();

    use tokio::io::{AsyncWriteExt, BufReader};

    loop {
        let (stream, _) = listener.accept().await?;
        let (reader, mut writer) = stream.into_split();
        let mut reader = BufReader::new(reader);
        let Some(request_line) = read_http_request_line_async(&mut reader).await? else {
            continue;
        };
        if !drain_http_headers_async(&mut reader).await? {
            continue;
        }

        let parts: Vec<&str> = request_line.split_whitespace().collect();
        if parts.len() < 2 {
            let _ = writer
                .write_all(bad_request_response("Invalid HTTP request.").as_bytes())
                .await;
            continue;
        }

        let path = parts[1];
        let url = match url::Url::parse(&format!("http://localhost{}", path)) {
            Ok(url) => url,
            Err(_) => {
                let _ = writer
                    .write_all(bad_request_response("Could not parse OAuth callback URL.").as_bytes())
                    .await;
                continue;
            }
        };

        if let Some(error) = url
            .query_pairs()
            .find(|(k, _)| k == "error")
            .map(|(_, v)| v.to_string())
        {
            let _ = writer
                .write_all(bad_request_response("Authentication was denied or cancelled.").as_bytes())
                .await;
            anyhow::bail!("OAuth provider returned error: {}", error);
        }

        let code = match url.query_pairs().find(|(k, _)| k == "code").map(|(_, v)| v.to_string()) {
            Some(code) => code,
            None => {
                let _ = writer
                    .write_all(bad_request_response("No authorization code was included in this request.").as_bytes())
                    .await;
                continue;
            }
        };

        let state = match url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string())
        {
            Some(state) => state,
            None => {
                let _ = writer
                    .write_all(bad_request_response("No OAuth state was included in this request.").as_bytes())
                    .await;
                continue;
            }
        };

        if state != expected_state {
            let _ = writer
                .write_all(bad_request_response("OAuth state mismatch. Please retry the latest login flow.").as_bytes())
                .await;
            continue;
        }

        let body =
            "<html><body><h1>Success!</h1><p>You can close this window and return to horon-agent.</p></body></html>";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        writer.write_all(response.as_bytes()).await?;

        return Ok(code);
    }
}

/// Parse an OAuth callback input.
///
/// Accepted formats:
/// - plain code (`abc123`)
/// - URL/query with `code=`
/// - `code#state` (OpenCode-style)
fn parse_oauth_code_input(input: &str) -> Result<(String, Option<String>)> {
    let trimmed = input.trim();

    let (raw_code, state_from_query) = if trimmed.contains("code=") {
        let url = url::Url::parse(trimmed).or_else(|_| url::Url::parse(&format!("https://example.com?{}", trimmed)))?;
        let code = url
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.to_string())
            .ok_or_else(|| anyhow::anyhow!("No code found in URL"))?;
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string());
        (code, state)
    } else {
        (trimmed.to_string(), None)
    };

    let (code, state) = if raw_code.contains('#') {
        let parts: Vec<&str> = raw_code.splitn(2, '#').collect();
        (parts[0].to_string(), Some(parts[1].to_string()))
    } else {
        (raw_code, state_from_query)
    };

    if code.trim().is_empty() {
        anyhow::bail!("No authorization code provided");
    }

    Ok((code, state))
}

pub fn parse_callback_input_with_state(input: &str) -> Result<(String, String)> {
    let (code, state) = parse_oauth_code_input(input)?;
    let state = state.filter(|value| !value.trim().is_empty()).ok_or_else(|| {
        anyhow::anyhow!("Please paste the full callback URL or query string so horon-agent can verify the login state.")
    })?;
    Ok((code, state))
}

pub fn openai_auth_url(redirect_uri: &str, challenge: &str, state: &str) -> String {
    openai_auth_url_with_prompt(redirect_uri, challenge, state, None)
}

pub fn openai_auth_url_with_prompt(redirect_uri: &str, challenge: &str, state: &str, prompt: Option<&str>) -> String {
    let prompt_param = prompt
        .map(|p| format!("&prompt={}", urlencoding::encode(p)))
        .unwrap_or_default();
    format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=codex_cli_rs{}",
        openai::AUTHORIZE_URL,
        openai::CLIENT_ID,
        urlencoding::encode(redirect_uri),
        urlencoding::encode(openai::SCOPES),
        challenge,
        state,
        prompt_param
    )
}

pub fn callback_listener_available(port: u16) -> bool {
    std::net::TcpListener::bind(format!("127.0.0.1:{port}"))
        .map(|listener| {
            drop(listener);
            true
        })
        .unwrap_or(false)
}

async fn exchange_openai_code_at_url(
    token_url: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<OAuthTokens> {
    let client = crate::provider::shared_http_client();
    let resp = client
        .post(token_url)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=authorization_code&client_id={}&code={}&code_verifier={}&redirect_uri={}",
            openai::CLIENT_ID,
            code,
            verifier,
            urlencoding::encode(redirect_uri)
        ))
        .send()
        .await?;

    if !resp.status().is_success() {
        let text = resp.text().await?;
        anyhow::bail!("Token exchange failed: {}", text);
    }

    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: String,
        expires_in: i64,
        id_token: Option<String>,
    }

    let tokens: TokenResponse = resp.json().await?;
    let expires_at = chrono::Utc::now().timestamp_millis() + (tokens.expires_in * 1000);

    Ok(OAuthTokens {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at,
        id_token: tokens.id_token,
        scopes: Vec::new(),
    })
}

pub async fn exchange_openai_code(code: &str, verifier: &str, redirect_uri: &str) -> Result<OAuthTokens> {
    exchange_openai_code_at_url(openai::TOKEN_URL, code, verifier, redirect_uri).await
}

pub async fn exchange_openai_callback_input(
    verifier: &str,
    input: &str,
    expected_state: &str,
    redirect_uri: &str,
) -> Result<OAuthTokens> {
    let (code, callback_state) = parse_callback_input_with_state(input)?;
    if callback_state != expected_state {
        anyhow::bail!("OAuth state mismatch. Start login again and use the latest callback URL.");
    }
    exchange_openai_code(&code, verifier, redirect_uri).await
}

/// Perform OAuth login for OpenAI/Codex
pub async fn login_openai(no_browser: bool) -> Result<OAuthTokens> {
    let (verifier, challenge) = generate_pkce();
    let state = generate_state();

    let port = openai::DEFAULT_PORT;
    let redirect_uri = openai::redirect_uri(port);
    let auth_url = openai_auth_url_with_prompt(&redirect_uri, &challenge, &state, Some("login"));

    eprintln!("\nOpen this URL in your browser:\n");
    eprintln!("{}\n", auth_url);
    if let Some(qr) = crate::auth::login_ui::qr_section(
        &auth_url,
        "Scan this QR on another device if this machine has no browser:",
        "    ",
        crate::auth::browser_suppressed(no_browser),
    ) {
        eprintln!("{qr}\n");
    }

    let callback_listener = bind_callback_listener(port).ok();
    let browser_opened = if crate::auth::browser_suppressed(no_browser) {
        false
    } else {
        crate::companion_client::open_url(&auth_url)
    };

    if browser_opened {
        if let Some(listener) = callback_listener {
            eprintln!("Waiting up to 300s for automatic callback on {}", redirect_uri);
            match tokio::time::timeout(
                std::time::Duration::from_secs(300),
                wait_for_callback_async_on_listener(listener, &state),
            )
            .await
            {
                Ok(Ok(code)) => return exchange_openai_code(&code, &verifier, &redirect_uri).await,
                Ok(Err(err)) => {
                    eprintln!("Automatic callback failed ({err}). Falling back to manual paste.");
                }
                Err(_) => {
                    eprintln!("Timed out waiting for callback. Falling back to manual paste.");
                }
            }
        } else {
            eprintln!(
                "Local callback port {} is unavailable. Finish login in any browser, then paste the full callback URL here.\n",
                port
            );
        }
    } else if !browser_opened {
        eprintln!(
            "Couldn't open a browser on this machine. Use the QR code above, then paste the full callback URL here.\n"
        );
    }

    eprintln!("Paste the full callback URL (or query string) here:\n");
    eprint!("> ");
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();
    if trimmed.is_empty() {
        anyhow::bail!("No callback URL entered.");
    }
    exchange_openai_callback_input(&verifier, trimmed, &state, &redirect_uri).await
}

pub fn save_openai_tokens(tokens: &OAuthTokens) -> Result<()> {
    let label = crate::auth::codex::login_target_label(None)?;
    save_openai_tokens_for_account(tokens, &label)
}

/// Save OpenAI tokens for a specific stored account label.
pub fn save_openai_tokens_for_account(tokens: &OAuthTokens, label: &str) -> Result<()> {
    crate::auth::codex::upsert_account_from_tokens(
        label,
        &tokens.access_token,
        &tokens.refresh_token,
        tokens.id_token.clone(),
        Some(tokens.expires_at),
    )?;
    Ok(())
}

/// End-to-end budget for one OpenAI token refresh exchange: connection,
/// request upload, response headers, and the full response body. The token
/// endpoint normally answers in well under a second. The shared HTTP client
/// only bounds connection setup, so without this a server that accepts the
/// request and then withholds headers or stalls mid-body would block the
/// refresh (and every caller queued behind the per-account refresh lock)
/// indefinitely.
pub const OPENAI_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

/// Refresh OpenAI/Codex OAuth tokens
pub async fn refresh_openai_tokens(refresh_token: &str) -> Result<OAuthTokens> {
    // External token (not stored in horon-agent auth) has no label: nothing on disk
    // to coordinate against, refresh directly.
    refresh_openai_tokens_at(
        openai::TOKEN_URL,
        OPENAI_REFRESH_TIMEOUT,
        refresh_token,
        crate::auth::codex::active_account_label(),
    )
    .await
}

/// Stored OpenAI tokens for `label`, expressed as [`OAuthTokens`].
fn stored_openai_tokens(label: &str) -> Option<OAuthTokens> {
    let account = crate::auth::codex::list_accounts()
        .ok()?
        .into_iter()
        .find(|account| account.label == label)?;
    Some(OAuthTokens {
        access_token: account.access_token,
        refresh_token: account.refresh_token,
        expires_at: account.expires_at.unwrap_or(0),
        id_token: account.id_token,
        scopes: Vec::new(),
    })
}

/// Refresh OpenAI/Codex OAuth tokens for a specific stored account label.
///
/// Serialized per account via the refresh coordinator: OpenAI rotates
/// refresh tokens, so two concurrent refreshes can otherwise persist a dead
/// refresh token and break the account.
pub async fn refresh_openai_tokens_for_account(refresh_token: &str, label: &str) -> Result<OAuthTokens> {
    refresh_openai_tokens_at(
        openai::TOKEN_URL,
        OPENAI_REFRESH_TIMEOUT,
        refresh_token,
        Some(label.to_string()),
    )
    .await
}

/// Run a refresh transaction so that dropping the caller cannot interrupt it.
///
/// OpenAI rotates refresh tokens. Once the token endpoint has been asked to
/// rotate, cancelling between "rotated tokens received" and "rotated tokens
/// persisted" would leave only the now-dead refresh token on disk. The
/// transaction therefore runs as its own task: the caller may stop waiting,
/// but the task finishes persistence (and releases the per-account refresh
/// lock) on its own. It is not an unbounded detached task: its only network
/// wait is capped by the refresh deadline, and the remaining work is local.
pub async fn run_refresh_transaction<T, F>(transaction: F) -> Result<T>
where
    T: Send + 'static,
    F: std::future::Future<Output = Result<T>> + Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle
            .spawn(transaction)
            .await
            .map_err(|err| anyhow::anyhow!("OAuth token refresh task failed: {err}"))?,
        Err(_) => transaction.await,
    }
}

async fn refresh_openai_tokens_at(
    token_url: &str,
    timeout: Duration,
    refresh_token: &str,
    label: Option<String>,
) -> Result<OAuthTokens> {
    if label.is_some() {
        // A token the provider already rejected as unrecoverable cannot start
        // working again. Fail fast so callers fall through to their fallback
        // instead of paying a doomed round-trip on every catalog sweep and turn.
        crate::auth::refresh_state::ensure_refresh_allowed(
            "openai",
            refresh_token,
            "Run `horon-agent login --provider openai` to mint a fresh token.",
        )?;
    }
    let token_url = token_url.to_string();
    let observed_refresh = refresh_token.to_string();
    // The per-account single-flight lock lives inside the transaction task, so
    // a cancelled caller cannot release it while a rotation is still in
    // flight and let a second refresh race with the same refresh token.
    run_refresh_transaction(async move {
        let Some(label) = label else {
            return refresh_openai_tokens_inner(&token_url, timeout, &observed_refresh, None).await;
        };
        crate::auth::refresh_coordinator::single_flight(
            format!("openai:{label}"),
            {
                let label = label.clone();
                move || stored_openai_tokens(&label)
            },
            {
                let observed = observed_refresh.clone();
                move |stored: &OAuthTokens| {
                    stored.refresh_token != observed
                        && crate::auth::refresh_coordinator::expiry_is_fresh(stored.expires_at)
                }
            },
            move |stored: Option<OAuthTokens>| async move {
                // Prefer the newest stored refresh token over the caller's
                // possibly stale observation.
                let token = stored
                    .map(|tokens| tokens.refresh_token)
                    .filter(|token| !token.is_empty())
                    .unwrap_or(observed_refresh);
                refresh_openai_tokens_inner(&token_url, timeout, &token, Some(&label)).await
            },
        )
        .await
    })
    .await
}

/// POST the refresh grant and read the complete response within `timeout`.
///
/// Errors never include the request body, tokens, or the success response.
async fn request_openai_token_refresh(token_url: &str, timeout: Duration, refresh_token: &str) -> Result<OAuthTokens> {
    use std::sync::atomic::{AtomicBool, Ordering};

    let headers_received = AtomicBool::new(false);
    let exchange = async {
        let resp = crate::provider::shared_http_client()
            .post(token_url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(format!(
                "grant_type=refresh_token&client_id={}&refresh_token={}",
                openai::CLIENT_ID,
                urlencoding::encode(refresh_token)
            ))
            .send()
            .await
            .map_err(|err| anyhow::anyhow!("OpenAI token refresh request failed: {err}"))?;
        headers_received.store(true, Ordering::SeqCst);
        let status = resp.status();
        let body = resp
            .bytes()
            .await
            .map_err(|err| anyhow::anyhow!("OpenAI token refresh response body failed (status {status}): {err}"))?;
        anyhow::Ok((status, body))
    };
    let (status, body) = match tokio::time::timeout(timeout, exchange).await {
        Ok(result) => result?,
        Err(_) => {
            let phase = if headers_received.load(Ordering::SeqCst) {
                "response body"
            } else {
                "response headers"
            };
            // The server may already have rotated the refresh token. The
            // stored token is left untouched: if it was rotated, the next
            // refresh is rejected and recorded as terminal, instead of being
            // guessed at or reset here.
            anyhow::bail!(
                "OpenAI token refresh timed out after {}s waiting for {phase}; \
                 the remote rotation outcome is unknown",
                timeout.as_secs_f32()
            );
        }
    };

    if !status.is_success() {
        anyhow::bail!("OpenAI token refresh failed: {}", String::from_utf8_lossy(&body));
    }

    #[derive(Deserialize)]
    struct TokenResponse {
        access_token: String,
        refresh_token: String,
        expires_in: i64,
        id_token: Option<String>,
    }

    // serde_json errors report only a position, never the credential body.
    let tokens: TokenResponse = serde_json::from_slice(&body)
        .map_err(|err| anyhow::anyhow!("OpenAI token refresh returned malformed JSON: {err}"))?;
    let expires_at = chrono::Utc::now().timestamp_millis() + (tokens.expires_in * 1000);
    Ok(OAuthTokens {
        access_token: tokens.access_token,
        refresh_token: tokens.refresh_token,
        expires_at,
        id_token: tokens.id_token,
        scopes: Vec::new(),
    })
}

async fn refresh_openai_tokens_inner(
    token_url: &str,
    timeout: Duration,
    refresh_token: &str,
    label: Option<&str>,
) -> Result<OAuthTokens> {
    let result: Result<OAuthTokens> = async {
        let oauth_tokens = request_openai_token_refresh(token_url, timeout, refresh_token).await?;

        if let Some(label) = label {
            save_openai_tokens_for_account(&oauth_tokens, label)?;
        } else {
            crate::logging::info(
                "Refreshed OpenAI/Codex tokens from an external source without storing them in horon-agent auth",
            );
        }
        Ok(oauth_tokens)
    }
    .await;

    // Route through the shared outcome recorder so a permanently invalidated
    // OpenAI refresh token lands in the terminal `Rejected` state instead of
    // being force-refreshed by every background catalog sweep forever.
    crate::auth::refresh_state::record_refresh_outcome("openai", refresh_token, &result);

    result
}

/// Build an OpenAI token refresh request (extracted for testability).
#[cfg(test)]
fn build_openai_refresh_request(refresh_token: &str) -> (String, String, Vec<u8>) {
    let body = format!(
        "grant_type=refresh_token&client_id={}&refresh_token={}",
        openai::CLIENT_ID,
        urlencoding::encode(refresh_token)
    );
    (
        openai::TOKEN_URL.to_string(),
        "application/x-www-form-urlencoded".to_string(),
        body.into_bytes(),
    )
}

/// Build an OpenAI token exchange request (extracted for testability).
#[cfg(test)]
fn build_openai_exchange_request(code: &str, verifier: &str, redirect_uri: &str) -> (String, String, Vec<u8>) {
    let body = format!(
        "grant_type=authorization_code&client_id={}&code={}&code_verifier={}&redirect_uri={}",
        openai::CLIENT_ID,
        code,
        verifier,
        urlencoding::encode(redirect_uri)
    );
    (
        openai::TOKEN_URL.to_string(),
        "application/x-www-form-urlencoded".to_string(),
        body.into_bytes(),
    )
}

#[cfg(test)]
#[path = "oauth_tests/mod.rs"]
mod tests;
