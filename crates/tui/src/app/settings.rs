//! The settings screen and the two settings that change behavior as they are
//! applied, rather than on the next drawing call.

use super::App;
use crate::overlays::settings::{SettingChange, SettingsOverlay};
use crate::settings::Settings;

impl App {
    /// Show the settings screen, seeded from the settings in force.
    pub fn open_settings(&mut self) {
        let mut overlay = SettingsOverlay::new(self.themes_dir.clone());
        overlay.refresh(&self.settings);
        self.overlay = Some(overlay);
    }

    /// Take the change the settings screen confirmed, for the caller to write.
    pub fn take_setting_change(&mut self) -> Option<SettingChange> {
        self.pending_change.take()
    }

    /// Replace the settings snapshot the screen draws, keeping the selection.
    ///
    /// The two settings that change behavior are applied to the parts that read
    /// them here rather than in each drawing call: the transcript keeps or drops
    /// reasoning as it streams, and the editor asks before it sends.
    pub fn refresh_settings(&mut self, settings: &Settings) {
        self.settings = settings.clone();
        let show_reasoning = settings.boolean("transcript.show_reasoning").unwrap_or(false);
        self.transcript.set_show_reasoning(show_reasoning);
        // The setting is a number, so it can hold a `-1` that is not a row
        // count: anything that does not fit falls back to the default instead
        // of reading as "hide the preview".
        let output_lines = match settings.number("transcript.tool_output_lines") {
            Some(lines) => usize::try_from(lines).unwrap_or(crate::tool_line::DEFAULT_OUTPUT_LINES),
            None => crate::tool_line::DEFAULT_OUTPUT_LINES,
        };
        self.transcript.set_tool_output_lines(output_lines);
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.refresh(settings);
        }
    }
}
