use super::{NAME, ToolOutput};
use crate::tool::artifacts;
use horon_agent_tool_types::ToolVerdict;

const MAX_CAPTURE: usize = 4 * 1024 * 1024;
const INLINE_BUDGET: usize = artifacts::GENERIC_CONDENSE_BUDGET_BYTES;
const CUT_NOTICE: &str = "\n[Python output collection reached the 4 MiB limit.]";

#[derive(Default)]
pub(super) struct Capture {
    bytes: Vec<u8>,
    capped: bool,
}

impl Capture {
    pub(super) fn push(&mut self, text: &str) {
        self.push_native(text.as_bytes());
    }

    pub(super) fn push_native(&mut self, bytes: &[u8]) {
        let end = bytes.len().min(MAX_CAPTURE.saturating_sub(self.bytes.len()));
        self.bytes.extend_from_slice(&bytes[..end]);
        self.capped |= end < bytes.len();
    }

    pub(super) fn finish(self, session_id: &str, notice: &str, status: &str, verdict: ToolVerdict) -> ToolOutput {
        let mut text = String::from_utf8(self.bytes)
            .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned());
        let mode = crate::config::config().tools.secrets_mode;
        if mode != crate::config::SecretsMode::Off {
            text = crate::secrets::mask_for_history(&text, session_id, mode);
        }
        let status = if status.is_empty() || mode == crate::config::SecretsMode::Off {
            std::borrow::Cow::Borrowed(status)
        } else {
            std::borrow::Cow::Owned(crate::secrets::mask_for_history(status, session_id, mode))
        };
        let status = &status[..boundary(&status, 1024)];
        let capped = self.capped || text.len() + status.len() > MAX_CAPTURE;
        let suffix_len = if capped {
            CUT_NOTICE.len() + status.len()
        } else {
            status.len()
        };
        let empty_fallback = if text.is_empty() && suffix_len == 0 {
            "(no output)".len()
        } else {
            0
        };
        let extra = suffix_len.max(empty_fallback);
        if capped {
            text.truncate(boundary(&text, MAX_CAPTURE - CUT_NOTICE.len() - status.len()));
            text.reserve(CUT_NOTICE.len() + status.len());
            text.push_str(CUT_NOTICE);
            text.push_str(status);
        } else {
            if !status.is_empty() || empty_fallback > 0 {
                text.reserve(extra);
            }
            text.push_str(status);
            if text.is_empty() {
                text.push_str("(no output)");
            }
        }
        if text.len() + notice.len() <= INLINE_BUDGET {
            let out = if notice.is_empty() {
                text
            } else {
                let mut combined = String::with_capacity(notice.len() + text.len());
                combined.push_str(notice);
                combined.push_str(&text);
                combined
            };
            let verdict = if capped {
                ToolVerdict::after_cut(Some(verdict))
            } else {
                verdict
            };
            return ToolOutput::new(out).with_title("Python eval").with_verdict(verdict);
        }

        let pointer = match artifacts::store(session_id, NAME, &text) {
            Ok(id) => format!("\n[Python output: artifact://{id}]\n"),
            Err(error) => {
                crate::logging::warn(&format!("[eval] failed to save session output: {error}"));
                "\n[Python output could not be saved as an artifact.]\n".to_owned()
            }
        };
        let remaining = INLINE_BUDGET.saturating_sub(notice.len() + pointer.len());
        let head = artifacts::head_end(&text, remaining / 2);
        let tail = artifacts::tail_start(&text, remaining - head, head);
        let tail_slice = &text[tail..];
        let mut preview = String::with_capacity(notice.len() + head + pointer.len() + tail_slice.len());
        preview.push_str(notice);
        preview.push_str(&text[..head]);
        preview.push_str(&pointer);
        preview.push_str(tail_slice);
        horon_agent_perf_counters::record_tool_output_spill(text.len() as u64, preview.len() as u64);
        ToolOutput::new(preview)
            .with_title("Python eval")
            .with_verdict(ToolVerdict::after_cut(Some(verdict)))
    }
}

fn boundary(text: &str, limit: usize) -> usize {
    text.floor_char_boundary(limit)
}
