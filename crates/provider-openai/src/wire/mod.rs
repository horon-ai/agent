//! Wire protocol selection and the parts every wire shares.
//!
//! One provider struct serves four request shapes. Only the endpoint, the body,
//! the stream parser, the error label, the usage semantics and the reported
//! `api_method` differ, so they are selected here from a single enum instead of
//! being forked into separate providers. [`WireSseStream`](sse::WireSseStream)
//! carries the SSE framing every wire reads the same way, and
//! [`turns::normalize`] prepares a conversation whose tool calls and results do
//! not pair up the way the non-OpenAI wires require.

use horon_agent_schema_dialect::{DialectSpec, registry};

pub mod anthropic;
pub mod gemini;
pub mod responses;
pub mod sse;
pub mod turns;

/// Which request and response shape a profile speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WireProtocol {
    /// `POST {base}/chat/completions`. The shape the provider has always used.
    #[default]
    ChatCompletions,
    /// `POST {base}/responses`, OpenAI's Responses API.
    Responses,
    /// `POST {base}/messages`, the Anthropic Messages API.
    AnthropicMessages,
    /// `POST {base}/models/{model}:streamGenerateContent?alt=sse`.
    GeminiGenerateContent,
}

impl WireProtocol {
    /// Display label that opens a failure message for this wire.
    ///
    /// The shared failover classifier keys on the words after the label, so all
    /// four keep one sentence shape and only the opening words change.
    pub fn label(self) -> &'static str {
        match self {
            Self::ChatCompletions => "OpenAI-compatible",
            Self::Responses => "OpenAI Responses",
            Self::AnthropicMessages => "Anthropic-compatible",
            Self::GeminiGenerateContent => "Gemini-compatible",
        }
    }

    /// The `api_method` this wire reports in model-route metadata.
    pub fn api_method(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Responses => "responses",
            Self::AnthropicMessages => "messages",
            Self::GeminiGenerateContent => "generate_content",
        }
    }

    /// The absolute URL this wire posts to.
    ///
    /// `base_url` normally arrives already trimmed of a trailing `/`; it is
    /// trimmed again here so a profile that was written with one still reaches
    /// the same path. A base that already ends with the wire's path is used
    /// unchanged, which is how a profile may point at the full endpoint.
    ///
    /// Gemini's path carries the model, so naming the method on the base has two
    /// accepted forms: the full `…:streamGenerateContent?alt=sse` is returned
    /// unchanged, and a bare `…:streamGenerateContent` gets only `?alt=sse`
    /// appended. Either way the model in the base wins over the `model`
    /// argument, since the path can name only one.
    pub fn endpoint(self, base_url: &str, model: &str) -> String {
        let base = base_url.trim_end_matches('/');
        match self {
            Self::ChatCompletions => joined(base, crate::chat::request::CHAT_COMPLETIONS_PATH),
            Self::Responses => joined(base, "responses"),
            Self::AnthropicMessages => joined(base, "messages"),
            Self::GeminiGenerateContent => gemini_endpoint(base, model),
        }
    }

    /// Whether the reported input tokens already include the cached ones.
    ///
    /// Every wire here reports an inclusive input count except Anthropic, whose
    /// `input_tokens` excludes the cache reads and writes it reports separately,
    /// so a caller that adds them up has to know which arithmetic applies.
    pub fn usage_is_inclusive(self) -> bool {
        !matches!(self, Self::AnthropicMessages)
    }

    /// The dialect this wire's tool schemas are normalized with.
    ///
    /// Responses carries the same function-parameters subset as Chat Completions,
    /// so it shares the OpenAI allow-list; the other two are narrower and reject
    /// constructs the OpenAI list accepts.
    pub fn dialect(self) -> &'static DialectSpec {
        match self {
            Self::ChatCompletions | Self::Responses => &registry::OPENAI,
            Self::AnthropicMessages => &registry::ANTHROPIC,
            Self::GeminiGenerateContent => &registry::GEMINI,
        }
    }

    /// What a failed response body is described as in an error message.
    pub fn error_context(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat completions",
            Self::Responses => "responses",
            Self::AnthropicMessages => "messages",
            Self::GeminiGenerateContent => "generate content",
        }
    }
}

/// `base` plus `path`, unless `base` already ends with that path.
fn joined(base: &str, path: &str) -> String {
    match base.strip_suffix(path) {
        Some(head) if head.ends_with('/') => base.to_string(),
        _ => format!("{base}/{path}"),
    }
}

/// The Gemini streaming URL for `model`, honouring a base that names the method.
fn gemini_endpoint(base: &str, model: &str) -> String {
    const METHOD: &str = ":streamGenerateContent";
    const QUERY: &str = "?alt=sse";
    if base.ends_with(&format!("{METHOD}{QUERY}")) {
        base.to_string()
    } else if base.ends_with(METHOD) {
        format!("{base}{QUERY}")
    } else {
        joined(base, &format!("models/{model}{METHOD}{QUERY}"))
    }
}

#[cfg(test)]
mod tests {
    use super::WireProtocol;
    use horon_agent_schema_dialect::registry;

    const BASE: &str = "https://api.example.test/v1";
    const MODEL: &str = "test-model";

    fn all() -> [WireProtocol; 4] {
        [
            WireProtocol::ChatCompletions,
            WireProtocol::Responses,
            WireProtocol::AnthropicMessages,
            WireProtocol::GeminiGenerateContent,
        ]
    }

    fn path_of(wire: WireProtocol) -> &'static str {
        match wire {
            WireProtocol::ChatCompletions => "/v1/chat/completions",
            WireProtocol::Responses => "/v1/responses",
            WireProtocol::AnthropicMessages => "/v1/messages",
            WireProtocol::GeminiGenerateContent => "/v1/models/test-model:streamGenerateContent?alt=sse",
        }
    }

    #[test]
    fn each_wire_posts_to_its_own_path() {
        for wire in all() {
            assert_eq!(
                wire.endpoint(BASE, MODEL),
                format!("https://api.example.test{}", path_of(wire)),
                "{wire:?}"
            );
        }
    }

    #[test]
    fn a_trailing_slash_on_the_base_does_not_double() {
        for wire in all() {
            assert_eq!(
                wire.endpoint(&format!("{BASE}/"), MODEL),
                wire.endpoint(BASE, MODEL),
                "{wire:?}"
            );
        }
    }

    #[test]
    fn a_base_that_already_ends_with_the_path_is_used_unchanged() {
        // A profile may point at the full endpoint; the path must not be
        // appended twice, which is the rule Chat Completions already follows.
        for wire in all() {
            let full = format!("https://api.example.test{}", path_of(wire));
            assert_eq!(wire.endpoint(&full, MODEL), full, "{wire:?}");
            assert_eq!(wire.endpoint(&format!("{full}/"), MODEL), full, "{wire:?}");
        }
    }

    #[test]
    fn the_gemini_endpoint_names_the_model_it_streams() {
        // Unlike the other three, this path carries the model, so two models on
        // one base URL are two URLs.
        let first = WireProtocol::GeminiGenerateContent.endpoint(BASE, "gemini-2.5-pro");
        let second = WireProtocol::GeminiGenerateContent.endpoint(BASE, "gemini-3-pro");
        assert_eq!(
            first,
            "https://api.example.test/v1/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            second,
            "https://api.example.test/v1/models/gemini-3-pro:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn a_gemini_base_naming_the_method_takes_only_the_query() {
        // A profile may name the streaming method without the query; appending
        // the whole path again would corrupt the URL.
        let gemini = WireProtocol::GeminiGenerateContent;
        let method = "https://api.example.test/v1beta/models/gemini-2.5-pro:streamGenerateContent";
        assert_eq!(gemini.endpoint(method, MODEL), format!("{method}?alt=sse"));
        // The model argument cannot override a path that already names one.
        assert_eq!(gemini.endpoint(method, "other-model"), format!("{method}?alt=sse"));
    }

    #[test]
    fn a_gemini_base_naming_the_method_and_the_query_is_used_unchanged() {
        let gemini = WireProtocol::GeminiGenerateContent;
        let full = "https://api.example.test/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse";
        assert_eq!(gemini.endpoint(full, MODEL), full);
        assert_eq!(gemini.endpoint(&format!("{full}/"), MODEL), full);
    }

    #[test]
    fn only_the_gemini_path_interpolates_the_model() {
        for wire in [
            WireProtocol::ChatCompletions,
            WireProtocol::Responses,
            WireProtocol::AnthropicMessages,
        ] {
            assert_eq!(wire.endpoint(BASE, "a"), wire.endpoint(BASE, "b"), "{wire:?}");
        }
    }

    #[test]
    fn each_wire_reports_its_own_label() {
        let labels: Vec<&str> = all().into_iter().map(WireProtocol::label).collect();
        assert_eq!(
            labels,
            vec![
                "OpenAI-compatible",
                "OpenAI Responses",
                "Anthropic-compatible",
                "Gemini-compatible"
            ]
        );
    }

    #[test]
    fn each_wire_reports_its_own_api_method() {
        let methods: Vec<&str> = all().into_iter().map(WireProtocol::api_method).collect();
        assert_eq!(
            methods,
            vec!["chat_completions", "responses", "messages", "generate_content"]
        );
    }

    #[test]
    fn the_error_context_names_the_thing_that_failed() {
        // The failover classifier keys on the sentence that follows the label,
        // so each context has to say which request shape broke.
        let contexts: Vec<&str> = all().into_iter().map(WireProtocol::error_context).collect();
        assert_eq!(
            contexts,
            vec!["chat completions", "responses", "messages", "generate content"]
        );
    }

    #[test]
    fn the_dialect_follows_the_wire() {
        assert_eq!(WireProtocol::ChatCompletions.dialect().id, registry::OPENAI.id);
        assert_eq!(WireProtocol::Responses.dialect().id, registry::OPENAI.id);
        assert_eq!(WireProtocol::AnthropicMessages.dialect().id, registry::ANTHROPIC.id);
        assert_eq!(WireProtocol::GeminiGenerateContent.dialect().id, registry::GEMINI.id);
    }

    #[test]
    fn the_narrower_dialects_reject_what_the_openai_list_accepts() {
        // Not a property of the enum, but the reason `dialect()` exists: if the
        // four shared one spec, passing it as an argument would be pointless.
        assert!(registry::OPENAI.supports("additionalProperties"));
        assert!(!registry::GEMINI.supports("additionalProperties"));
    }

    #[test]
    fn only_anthropic_reports_an_exclusive_input_count() {
        // Anthropic's `input_tokens` excludes the cache reads and writes it
        // reports separately; every other wire has already added them in.
        assert!(WireProtocol::ChatCompletions.usage_is_inclusive());
        assert!(WireProtocol::Responses.usage_is_inclusive());
        assert!(WireProtocol::GeminiGenerateContent.usage_is_inclusive());
        assert!(!WireProtocol::AnthropicMessages.usage_is_inclusive());
    }

    #[test]
    fn chat_completions_is_the_default() {
        assert_eq!(WireProtocol::default(), WireProtocol::ChatCompletions);
    }
}
