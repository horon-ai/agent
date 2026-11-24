//! Recognize standalone text tool attempts so the model can retry natively.

use regex::Regex;
use std::sync::LazyLock;

const TAG_PATTERN: &str = r"(?i)<\s*(/)?\s*(?:[｜|]\s*)?(?:DSML\s*[｜|]?\s*)?(?:antml:)?(function_calls|tool_calls|calls|invoke|parameter)\b([^<>]*)(?:>|$)";
const TAG_AT_START_PATTERN: &str = r"(?i)\A<\s*(/)?\s*(?:[｜|]\s*)?(?:DSML\s*[｜|]?\s*)?(?:antml:)?(function_calls|tool_calls|calls|invoke|parameter)\b([^<>]*)(?:>|$)";

static TAGS: LazyLock<(Regex, Regex)> = LazyLock::new(|| {
    // Accept namespace spellings seen in DeepSeek-compatible responses, and
    // anchor the start-only matcher when walking standalone markup so prose avoids scanning.
    Regex::new(TAG_PATTERN)
        .and_then(|any| Regex::new(TAG_AT_START_PATTERN).map(|at_start| (any, at_start)))
        .expect("valid tool markup tag regex")
});

static NAME_ATTR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)(?:^|\s)name\s*=\s*"[^"\s]+""#).expect("valid tool markup name regex"));

pub(super) fn text_tool_attempt(text: &str) -> Option<&'static str> {
    let text = text.trim();
    if let Some(call) = text.strip_prefix("to=functions.") {
        let args = call.trim_start_matches(|ch: char| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
        if args.len() < call.len() {
            let args = args.trim_start();
            if args.is_empty() {
                return Some("to=functions marker");
            }
            if args.starts_with('{') {
                let mut json = serde_json::Deserializer::from_str(args).into_iter::<serde::de::IgnoredAny>();
                match json.next() {
                    Some(Ok(_)) if args[json.byte_offset()..].trim().is_empty() => return Some("to=functions marker"),
                    Some(Err(error)) if error.is_eof() => return Some("to=functions marker"),
                    _ => {}
                }
            }
        }
    }
    (text.starts_with('<') && standalone_markup(text)).then_some("tool-call markup")
}

fn standalone_markup(mut text: &str) -> bool {
    // Text between tags is allowed only inside a parameter. This keeps prose,
    // including fenced, quoted and inline examples, out of the retry path.
    let mut saw_named_attempt = false;
    while !text.is_empty() {
        if !text.starts_with('<') {
            return false;
        }
        let Some(tag) = TAGS.1.captures(text) else {
            return false;
        };
        let whole = tag.get_match();
        let kind = &tag[2];
        let parameter = kind.eq_ignore_ascii_case("parameter");
        let opening = tag.get(1).is_none();
        if opening && (parameter || kind.eq_ignore_ascii_case("invoke")) && NAME_ATTR.is_match(&tag[3]) {
            saw_named_attempt = true;
        }
        text = text[whole.end()..].trim_start();
        if opening && parameter {
            let Some(end) = TAGS
                .0
                .captures_iter(text)
                .find(|tag| tag.get(1).is_some() && tag[2].eq_ignore_ascii_case("parameter"))
            else {
                return saw_named_attempt;
            };
            text = text[end.get_match().end()..].trim_start();
        }
    }
    saw_named_attempt
}

pub(super) fn unparsed_markup_reminder(reason: &str) -> String {
    format!(
        "<system-reminder>Your previous response contained a tool attempt as assistant text ({reason}). No tool ran for that attempt. If you intended to use a tool, re-issue it now using the native tool-calling interface with the tool name and a complete JSON object of arguments. If you meant to show an example, present it in a fenced code block. Do not repeat completed work.</system-reminder>"
    )
}

pub(super) fn unparsed_markup_notice(reason: &str) -> String {
    format!(
        "[tool-call recovery] The model wrote a tool attempt as text ({reason}). No tool ran for that attempt. Asking the model to re-issue it as a native tool call."
    )
}
