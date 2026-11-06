//! Lenient serde helper for string-valued configuration enums.
//!
//! A single unrecognized enum value used to abort parsing of the *entire*
//! `config.toml`, so `Config::load` silently fell back to `Config::default()`
//! and every unrelated setting looked like it was ignored (issue #689).
//! Degrading the one field that is actually wrong is far better than discarding
//! the user's configuration.

use serde::Deserialize;

/// `Option` variant of a lenient enum parse: an unrecognized value becomes
/// `None` ("not configured") rather than a config-wide parse failure.
pub(crate) fn lenient_optional_enum<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    let Some(raw) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let value = serde::de::value::StrDeserializer::<serde::de::value::Error>::new(raw.as_str());
    Ok(T::deserialize(value).ok())
}

/// Regression tests for issue #689: an unrecognized value in one `[display]`
/// enum field must not discard the rest of the user's configuration.
#[cfg(test)]
mod tests {
    use crate::{DisplayConfig, ReasoningDisplayMode};

    fn parse(json: &str) -> DisplayConfig {
        serde_json::from_str(json).expect("display config must still parse")
    }

    #[test]
    fn unknown_reasoning_display_falls_back_without_losing_other_fields() {
        let display = parse(r#"{"emoji": false, "reasoning_display": "nope"}"#);
        assert!(!display.emoji);
        assert!(
            !display.has_explicit_reasoning_display(),
            "an unparseable value means 'not configured'"
        );
    }

    /// Removed TUI-only keys must not break existing configs.
    #[test]
    fn legacy_tui_keys_are_ignored() {
        let display = parse(
            r#"{"emoji": false, "centered": true, "diff_mode": "inline",
                "diagram_mode": "pinned", "markdown_spacing": "document",
                "latex_rendering": "image", "overscroll_status": "on"}"#,
        );
        assert!(!display.emoji);
    }

    /// Valid values must still round-trip (leniency must not swallow them).
    #[test]
    fn valid_values_are_unaffected() {
        let display = parse(r#"{"reasoning_display": "current"}"#);
        assert_eq!(display.reasoning_display(), ReasoningDisplayMode::Current);
    }
}
