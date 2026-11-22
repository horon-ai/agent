//! System-prompt text one run appended on the command line.
//!
//! The text is kept per session id in this process and is never written to the
//! session, so another session, a resumed session and every default prompt are
//! untouched. It is bounded, and it is added after the composed prompt, so the
//! base prompt and the tool contracts stay in place.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

/// Largest appended text a run may carry, in bytes.
pub const MAX_APPEND_BYTES: usize = 16 * 1024;

static APPENDED: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);

pub fn set_appended(session_id: &str, text: &str) -> anyhow::Result<()> {
    let text = text.trim();
    anyhow::ensure!(!text.is_empty(), "the appended system text is empty");
    anyhow::ensure!(
        text.len() <= MAX_APPEND_BYTES,
        "the appended system text is {} bytes, over the {MAX_APPEND_BYTES} byte limit",
        text.len()
    );
    APPENDED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(session_id.to_owned(), text.to_owned());
    Ok(())
}

pub(super) fn appended(session_id: &str) -> Option<String> {
    APPENDED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(session_id)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appended_text_is_trimmed_bounded_and_kept_per_session() {
        set_appended("run-prompt-a", "  be brief  ").unwrap();
        assert_eq!(appended("run-prompt-a").as_deref(), Some("be brief"));
        assert_eq!(appended("run-prompt-b"), None);
        assert!(set_appended("run-prompt-c", "   ").is_err());
        let error = set_appended("run-prompt-c", &"x".repeat(MAX_APPEND_BYTES + 1)).unwrap_err();
        assert!(error.to_string().contains("16384 byte limit"), "{error}");
        assert_eq!(appended("run-prompt-c"), None);
    }
}
