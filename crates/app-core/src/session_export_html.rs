//! Standalone HTML transcript export.
//!
//! One document, no network: the report inlines its stylesheet, loads no
//! script, font or image, and declares a Content-Security-Policy of
//! `default-src 'none'` so a browser refuses anything a later edit might add.
//! Every transcript character is escaped exactly once, so stored text that
//! looks like markup — a tool result holding `<script>` — renders as the
//! characters an operator wrote, never as a node.
//!
//! The text comes from [`crate::session_export::stream_export`], the same redacted
//! and truncated pipeline the Markdown export renders, so a secret that is
//! absent from one format is absent from the other.

use std::io::Write as _;

use crate::session::{Session, StoredMessage};
use crate::session_export::{ExportSink, stream_export, write_redacted_pretty_json};
use anyhow::Result;

/// The policy the report carries, which also keeps it offline.
///
/// `default-src 'none'` covers script, image, font, frame and connection
/// directives, so no element in the document can reach the network even if one
/// is added later; `style-src 'unsafe-inline'` is the only allowance, because
/// the stylesheet is inline.
pub const REPORT_CSP: &str = "default-src 'none'; style-src 'unsafe-inline'";

struct HtmlSink(Vec<u8>);

struct HtmlEscapeWriter<'a>(&'a mut Vec<u8>);

impl std::io::Write for HtmlEscapeWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        push_escaped_bytes(self.0, buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl ExportSink for HtmlSink {
    fn begin_message(&mut self, role: &'static str) {
        let class = if role == "User" { "user" } else { "assistant" };
        self.0.extend_from_slice(b"<section class=\"turn ");
        self.0.extend_from_slice(class.as_bytes());
        self.0.extend_from_slice(b"\">\n<h2>");
        self.0.extend_from_slice(role.as_bytes());
        self.0.extend_from_slice(b"</h2>\n");
    }

    fn end_message(&mut self) {
        self.0.extend_from_slice(b"</section>\n");
    }

    fn text(&mut self, text: &str) {
        self.0.extend_from_slice(b"<div class=\"text\">");
        push_escaped_bytes(&mut self.0, text.as_bytes());
        self.0.extend_from_slice(b"</div>\n");
    }

    fn thinking(&mut self, text: &str) {
        self.0
            .extend_from_slice(b"<div class=\"thinking\"><span class=\"label\">Thinking</span>");
        push_escaped_bytes(&mut self.0, text.as_bytes());
        self.0.extend_from_slice(b"</div>\n");
    }

    fn tool_call(&mut self, name: &str, input: &serde_json::Value) {
        self.0
            .extend_from_slice(b"<div class=\"tool\"><div class=\"label\">Tool: <code>");
        push_escaped_bytes(&mut self.0, name.as_bytes());
        self.0.extend_from_slice(b"</code></div><pre><code>");
        let mut writer = HtmlEscapeWriter(&mut self.0);
        write_redacted_pretty_json(&mut writer, input);
        self.0.extend_from_slice(b"</code></pre></div>\n");
    }

    fn tool_result(&mut self, label: &'static str, content: &str, truncated_total: Option<usize>) {
        self.0.extend_from_slice(b"<div class=\"result\"><div class=\"label\">");
        self.0.extend_from_slice(label.as_bytes());
        self.0.extend_from_slice(b"</div><pre><code>");
        push_escaped_bytes(&mut self.0, content.as_bytes());
        if let Some(total) = truncated_total {
            let _write_res = write!(self.0, "\n… (truncated, {total} bytes total)");
        }
        self.0.extend_from_slice(b"</code></pre></div>\n");
    }

    fn placeholder(&mut self, label: &'static str) {
        self.0.extend_from_slice(b"<div class=\"placeholder\">[");
        push_escaped_bytes(&mut self.0, label.as_bytes());
        self.0.extend_from_slice(b"]</div>\n");
    }
}

/// Render visible history as one self-contained HTML document.
///
/// Each message becomes a `<section>` with an `<h2>` naming the role and one
/// element per content block: prose and reasoning keep their line breaks in a
/// `pre-wrap` block, a tool call and its result sit in `<pre>` blocks, and a
/// block with no readable content becomes a bracketed placeholder.
pub fn render_html(messages: &[StoredMessage]) -> String {
    let mut html = Vec::with_capacity(messages.len().saturating_mul(384).saturating_add(1024));
    html.extend_from_slice(
        b"<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta http-equiv=\"Content-Security-Policy\" content=\"",
    );
    html.extend_from_slice(REPORT_CSP.as_bytes());
    html.extend_from_slice(b"\">\n<title>Horon session transcript</title>\n");
    html.extend_from_slice(STYLESHEET.as_bytes());
    html.extend_from_slice(b"</head>\n<body>\n<main>\n");
    let mut sink = HtmlSink(html);
    stream_export(messages, &mut sink);
    sink.0.extend_from_slice(b"</main>\n</body>\n</html>\n");
    String::from_utf8(sink.0).unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned())
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

/// Escape every character that could start or close markup, copying
/// unescaped spans directly into `out`.
///
/// Both text and attribute positions go through here, so no transcript value
/// is trusted by the element that holds it.
fn push_escaped_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let mut start = 0usize;
    for (idx, &byte) in bytes.iter().enumerate() {
        let replacement: Option<&[u8]> = match byte {
            b'&' => Some(b"&amp;"),
            b'<' => Some(b"&lt;"),
            b'>' => Some(b"&gt;"),
            b'"' => Some(b"&quot;"),
            b'\'' => Some(b"&#39;"),
            _ => None,
        };
        if let Some(escaped) = replacement {
            if start < idx {
                out.extend_from_slice(&bytes[start..idx]);
            }
            out.extend_from_slice(escaped);
            start = idx + 1;
        }
    }
    if start < bytes.len() {
        out.extend_from_slice(&bytes[start..]);
    }
}

#[cfg(test)]
#[path = "session_export_html_tests.rs"]
mod tests;
