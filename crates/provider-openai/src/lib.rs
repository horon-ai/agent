pub mod chat;
mod citations;
pub mod request;
pub mod stream;
pub mod websocket_health;

pub use chat::request::{
    CHAT_COMPLETIONS_PATH, build_chat_completions_request, build_chat_tools, chat_tool_definition,
    responses_input_to_chat_messages,
};
pub use chat::stream::{ChatCompletionsStream, ChatStreamState, parse_chat_chunk};
pub use chat::transport::{ChatAuth, HttpErrorKind, redact_secret};
pub use chat::{OpenAiCompatibleProvider, PROVIDER_NAME};
pub use request::{
    OPENAI_ENCRYPTED_CONTENT_PROVIDER_MAX_CHARS, OPENAI_ENCRYPTED_CONTENT_SAFE_MAX_CHARS, OpenAiRequestLogLevel,
    build_responses_input, build_responses_input_with_logger, build_tool, build_tools, downgrade_web_search_calls,
    insert_additional_tools, is_openai_encrypted_content_too_large_error, openai_encrypted_content_fallback_summary,
    openai_encrypted_content_is_sendable,
};
