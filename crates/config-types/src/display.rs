//! `[display]` section of the config: headless presentation settings.

use crate::ReasoningDisplayMode;
use serde::{Deserialize, Serialize};

/// Display configuration that headless clients and the server both consume.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    /// Enable debug socket for external control (default: false)
    pub debug_socket: bool,
    /// Render emoji in server-rendered output (default: true)
    pub emoji: bool,
    /// Request thinking/reasoning content by default (default: true)
    pub show_thinking: bool,
    /// How to display reasoning/thinking content (off/full/current).
    /// When unset, falls back to `show_thinking` (true => full, false => off).
    #[serde(default, deserialize_with = "crate::serde_lenient::lenient_optional_enum")]
    pub(crate) reasoning_display: Option<ReasoningDisplayMode>,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            debug_socket: false,
            emoji: true,
            show_thinking: true,
            reasoning_display: Some(ReasoningDisplayMode::Full),
        }
    }
}

impl DisplayConfig {
    /// Resolve the effective reasoning display mode. Prefers the explicit
    /// `reasoning_display` field, falling back to the legacy `show_thinking`
    /// boolean (true => Full, false => Off) when unset.
    pub fn reasoning_display(&self) -> ReasoningDisplayMode {
        self.reasoning_display.unwrap_or(if self.show_thinking {
            ReasoningDisplayMode::Full
        } else {
            ReasoningDisplayMode::Off
        })
    }

    /// Whether the user explicitly chose a reasoning display mode, as opposed
    /// to inheriting the legacy `show_thinking` fallback. Front-ends use this
    /// to apply their own default without overriding a deliberate choice.
    pub fn has_explicit_reasoning_display(&self) -> bool {
        self.reasoning_display.is_some()
    }

    /// Set the reasoning display mode and keep `show_thinking` in sync so the
    /// provider request path (which still keys off `show_thinking`) requests
    /// reasoning whenever any display mode is active.
    pub fn set_reasoning_display(&mut self, mode: ReasoningDisplayMode) {
        self.reasoning_display = Some(mode);
        self.show_thinking = !matches!(mode, ReasoningDisplayMode::Off);
    }
}

#[cfg(test)]
mod tests {
    use super::DisplayConfig;
    use crate::ReasoningDisplayMode;

    #[test]
    fn thinking_is_shown_in_full_by_default() {
        let default = DisplayConfig::default();
        assert!(default.show_thinking);
        assert_eq!(default.reasoning_display(), ReasoningDisplayMode::Full);

        let missing: DisplayConfig = serde_json::from_str("{}").expect("display config");
        assert!(missing.show_thinking);
        assert_eq!(missing.reasoning_display(), ReasoningDisplayMode::Full);
    }

    #[test]
    fn removed_tui_fields_are_ignored() {
        let parsed: DisplayConfig =
            serde_json::from_str(r#"{"diff_mode":"inline","centered":true,"emoji":false}"#).expect("display config");
        assert!(!parsed.emoji, "a kept field must still apply");
    }
}
