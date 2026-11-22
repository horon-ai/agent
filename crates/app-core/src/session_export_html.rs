//! Standalone HTML transcript export.
//!
//! One document, no network: the report inlines its stylesheet, loads no
//! script, font or image, and declares a Content-Security-Policy of
//! `default-src 'none'` so a browser refuses anything a later edit might add.
//! Every transcript character is escaped exactly once, so stored text that
//! looks like markup — a tool result holding `<script>` — renders as the
//! characters an operator wrote, never as a node.
//!
//! The text comes from [`crate::session_export::project`], the same redacted
//! and truncated projection the Markdown export renders, so a secret that is
//! absent from one format is absent from the other.

use crate::session::{Session, StoredMessage};
use crate::session_export::{ProjectedBlock, project};
use anyhow::Result;

/// The policy the report carries, which also keeps it offline.
///
/// `default-src 'none'` covers script, image, font, frame and connection
/// directives, so no element in the document can reach the network even if one
/// is added later; `style-src 'unsafe-inline'` is the only allowance, because
/// the stylesheet is inline.
pub const REPORT_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'";

/// Render visible history as one self-contained HTML document.
///
/// Each message becomes a `<section>` with an `<h2>` naming the role and one
/// element per content block: prose and reasoning keep their line breaks in a
/// `pre-wrap` block, a tool call and its result sit in `<pre>` blocks, and a
/// block with no readable content becomes a bracketed placeholder.
pub fn render_html(messages: &[StoredMessage]) -> String {
    let mut html = String::from(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n",
    );
    html.push_str(&format!(
        "<meta http-equiv=\"Content-Security-Policy\" content=\"{REPORT_CSP}\">\n<title>Horon session transcript</title>\n"
    ));
    html.push_str(STYLESHEET);
    html.push_str("</head>\n<body>\n<main>\n");
    for message in project(messages) {
        let class = if message.role == "User" { "user" } else { "assistant" };
        html.push_str(&format!(
            "<section class=\"turn {class}\">\n<h2>{}</h2>\n",
            message.role
        ));
        for block in message.blocks {
            match block {
                ProjectedBlock::Text(text) => {
                    html.push_str(&format!("<div class=\"text\">{}</div>\n", escape(&text)));
                }
                ProjectedBlock::Thinking(text) => {
                    html.push_str(&format!(
                        "<div class=\"thinking\"><span class=\"label\">Thinking</span>{}</div>\n",
                        escape(&text)
                    ));
                }
                ProjectedBlock::ToolCall { name, input } => {
                    html.push_str(&format!(
                        "<div class=\"tool\"><div class=\"label\">Tool: <code>{}</code></div><pre><code>{}</code></pre></div>\n",
                        escape(&name),
                        escape(&input)
                    ));
                }
                ProjectedBlock::ToolResult { label, content } => {
                    html.push_str(&format!(
                        "<div class=\"result\"><div class=\"label\">{label}</div><pre><code>{}</code></pre></div>\n",
                        escape(&content)
                    ));
                }
                ProjectedBlock::Placeholder(label) => {
                    html.push_str(&format!("<div class=\"placeholder\">[{}]</div>\n", escape(label)));
                }
            }
        }
        html.push_str("</section>\n");
    }
    html.push_str("</main>\n</body>\n</html>\n");
    html
}

/// Render a session's full history as a standalone HTML report.
pub fn export_session_html(session: &Session) -> Result<String> {
    Ok(render_html(&session.history_messages()?))
}

/// The inline stylesheet: no import, no font file, no image.
const STYLESHEET: &str = "<style>\n\
:root { color-scheme: light dark; }\n\
body { margin: 0; padding: 2rem 1rem 4rem; background: Canvas; color: CanvasText; }\n\
main { max-width: 54rem; margin: 0 auto; font-family: -apple-system, BlinkMacSystemFont, \"Segoe UI\", Roboto, Helvetica, Arial, sans-serif; line-height: 1.5; }\n\
section.turn { border-top: 1px solid color-mix(in srgb, CanvasText 18%, transparent); padding: 1.25rem 0; }\n\
section.turn:first-child { border-top: 0; }\n\
h2 { font-size: 0.8rem; letter-spacing: 0.08em; text-transform: uppercase; opacity: 0.65; margin: 0 0 0.75rem; }\n\
.text, .thinking { white-space: pre-wrap; overflow-wrap: anywhere; margin: 0 0 0.75rem; }\n\
.thinking { opacity: 0.75; font-style: italic; }\n\
.label { display: block; font-size: 0.78rem; font-weight: 600; opacity: 0.65; margin-bottom: 0.25rem; }\n\
.tool, .result { margin: 0 0 0.75rem; }\n\
pre { margin: 0; padding: 0.75rem; border-radius: 0.375rem; background: color-mix(in srgb, CanvasText 6%, transparent); overflow-x: auto; }\n\
code { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 0.9em; }\n\
.placeholder { opacity: 0.65; margin: 0 0 0.75rem; }\n\
</style>\n";

/// Escape every character that could start or close markup.
///
/// Both text and attribute positions go through here, so no transcript value
/// is trusted by the element that holds it.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
#[path = "session_export_html_tests.rs"]
mod tests;
