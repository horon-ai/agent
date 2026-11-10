use super::classify_model_fallback_error_message;

#[test]
fn model_fallback_accepts_capacity_and_availability_errors() {
    for message in [
        "429 Too Many Requests",
        "rate limited: 429",
        "500 Internal Server Error",
        "502 Bad Gateway",
        "503 Service Unavailable",
        "504 Gateway Timeout",
        "the model is currently overloaded, try again",
        "Service Unavailable",
        "model not found: gpt-nonexistent",
        "model_not_found",
        "model unavailable in this region",
        "model does not exist",
        "no such model",
    ] {
        assert!(
            classify_model_fallback_error_message(message),
            "a retryable model availability error must enter the fallback list: {message}"
        );
    }
}

#[test]
fn model_fallback_refuses_credential_and_request_errors() {
    for message in [
        "401 Unauthorized",
        "403 Forbidden",
        "400 Bad Request",
        "invalid_request_error: messages is required",
        "invalid request: unknown field 'tools'",
        "context length exceeded",
        "maximum context length is 128000 tokens",
        "the prompt is too long",
        "input is too long for this model",
        "too many tokens",
        "token limit reached",
        "413 Payload Too Large",
        "invalid api key",
    ] {
        assert!(
            !classify_model_fallback_error_message(message),
            "a non-retryable error must never start the fallback list: {message}"
        );
    }
}

#[test]
fn model_fallback_ignores_embedded_status_digits() {
    assert!(!classify_model_fallback_error_message("model version 4130 failed"));
    assert!(!classify_model_fallback_error_message("request id 5004 rejected"));
    assert!(!classify_model_fallback_error_message(""));
}
