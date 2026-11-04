use super::*;
use anyhow::{Result, anyhow};
use std::collections::HashMap;

fn utf8_body(body: Vec<u8>) -> Result<String> {
    String::from_utf8(body).map_err(|e| anyhow!(e))
}

fn require_param<'a>(pairs: &'a HashMap<String, String>, key: &str) -> Result<&'a str> {
    pairs
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| anyhow!("missing form/query param: {key}"))
}

// ========================
// Claude exchange request body validation
// ========================

// ========================
// Claude refresh request body validation
// ========================

// ========================
// OpenAI exchange request validation
// ========================

#[test]
fn openai_exchange_request_uses_form_urlencoded() -> Result<()> {
    let (_url, content_type, _body) =
        build_openai_exchange_request("code", "verifier", "http://localhost:1455/auth/callback");
    assert_eq!(content_type, "application/x-www-form-urlencoded");
    Ok(())
}

#[test]
fn openai_exchange_request_contains_required_fields() -> Result<()> {
    let (_url, _ct, body) =
        build_openai_exchange_request("oai_code_123", "oai_verifier", "http://localhost:1455/auth/callback");
    let body_str = utf8_body(body)?;
    assert!(body_str.contains("grant_type=authorization_code"));
    assert!(body_str.contains(&format!("client_id={}", openai::CLIENT_ID)));
    assert!(body_str.contains("code=oai_code_123"));
    assert!(body_str.contains("code_verifier=oai_verifier"));
    assert!(body_str.contains("redirect_uri="));
    Ok(())
}

#[test]
fn openai_exchange_request_targets_correct_url() -> Result<()> {
    let (url, _ct, _body) = build_openai_exchange_request("c", "v", "http://localhost/cb");
    assert_eq!(url, "https://auth.openai.com/oauth/token");
    Ok(())
}

// ========================
// OpenAI refresh request validation
// ========================

#[test]
fn openai_refresh_request_uses_form_urlencoded() -> Result<()> {
    let (_url, content_type, _body) = build_openai_refresh_request("rt_oai");
    assert_eq!(content_type, "application/x-www-form-urlencoded");
    Ok(())
}

#[test]
fn openai_refresh_request_contains_required_fields() -> Result<()> {
    let (_url, _ct, body) = build_openai_refresh_request("rt_oai_value");
    let body_str = utf8_body(body)?;
    assert!(body_str.contains("grant_type=refresh_token"));
    assert!(body_str.contains(&format!("client_id={}", openai::CLIENT_ID)));
    assert!(body_str.contains("refresh_token=rt_oai_value"));
    Ok(())
}

#[test]
fn openai_refresh_request_targets_correct_url() -> Result<()> {
    let (url, _ct, _body) = build_openai_refresh_request("rt");
    assert_eq!(url, "https://auth.openai.com/oauth/token");
    Ok(())
}

// ========================
// Auth URL construction
// ========================

#[test]
fn openai_auth_url_contains_required_params() -> Result<()> {
    let (_verifier, challenge) = generate_pkce();
    let state = generate_state();
    let redirect_uri = openai::redirect_uri(openai::DEFAULT_PORT);
    let auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge={}&code_challenge_method=S256&state={}",
        openai::AUTHORIZE_URL,
        openai::CLIENT_ID,
        urlencoding::encode(&redirect_uri),
        urlencoding::encode(openai::SCOPES),
        challenge,
        state,
    );
    let parsed = url::Url::parse(&auth_url).map_err(|e| anyhow!(e))?;
    let params: HashMap<String, String> = parsed
        .query_pairs()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(require_param(&params, "response_type")?, "code");
    assert_eq!(require_param(&params, "client_id")?, openai::CLIENT_ID);
    assert_eq!(require_param(&params, "redirect_uri")?, redirect_uri);
    assert_eq!(require_param(&params, "scope")?, openai::SCOPES);
    assert_eq!(require_param(&params, "code_challenge")?, challenge);
    assert_eq!(require_param(&params, "code_challenge_method")?, "S256");
    assert_eq!(require_param(&params, "state")?, state);
    Ok(())
}

// ========================
// Code parsing (plain code, URL, code#state)
// ========================

#[test]
fn parse_callback_input_requires_state() {
    let err = parse_callback_input_with_state("just-a-code")
        .expect_err("plain code should not satisfy stateful callback parsing");
    assert!(err.to_string().contains("full callback URL"));
}

#[test]
fn parse_callback_input_extracts_code_and_state() -> Result<()> {
    let (code, state) =
        parse_callback_input_with_state("http://localhost:1455/auth/callback?code=mycode&state=mystate")?;
    assert_eq!(code, "mycode");
    assert_eq!(state, "mystate");
    Ok(())
}

// ========================
// Mock server integration: Claude exchange
// ========================

#[tokio::test]
async fn openai_callback_input_rejects_state_mismatch() -> Result<()> {
    let err = exchange_openai_callback_input(
        "verifier",
        "http://localhost:1455/auth/callback?code=abc123&state=wrong_state",
        "expected_state",
        "http://localhost:1455/auth/callback",
    )
    .await
    .expect_err("state mismatch should fail before token exchange");

    assert!(
        err.to_string().contains("OAuth state mismatch"),
        "unexpected error: {err}"
    );
    Ok(())
}

// ========================
// Mock server integration: Claude refresh
// ========================

// ========================
// Regression: Claude Code now sends JSON token exchange bodies
// ========================

// ========================
// Token response parsing
// ========================

// ========================
// Special characters / URL encoding
// ========================

#[test]
fn openai_redirect_uri_format() {
    let uri = openai::redirect_uri(1455);
    assert_eq!(uri, "http://localhost:1455/auth/callback");
    let uri2 = openai::redirect_uri(9999);
    assert_eq!(uri2, "http://localhost:9999/auth/callback");
}

// ========================
// Provider token request content types match their upstream CLIs.
// ========================
