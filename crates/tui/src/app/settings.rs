//! The settings screen and the settings that change behavior as they are
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
    /// The settings that change behavior are applied to the parts that read them
    /// here rather than in each drawing call: the transcript keeps or drops
    /// reasoning as it streams, the editor asks before it sends, the mouse
    /// wheel belongs to the transcript only while `transcript.mouse_scroll`
    /// says so, the window title carries the turn state only while
    /// `title.state` says so, and the model cycle walks `models.shortlist`.
    /// Whether the terminal is actually captured is the event loop's decision,
    /// because a capture is a terminal mode.
    pub fn refresh_settings(&mut self, settings: &Settings) {
        // Markdown and its visual blocks change how many rows an item takes, and
        // the scrollback keeps the counts it measured: a view held across the
        // switch would be moved against heights that no longer describe what is
        // drawn.
        if crate::item_lines::markdown_enabled(&self.settings) != crate::item_lines::markdown_enabled(settings)
            || crate::item_lines::visual_blocks_enabled(&self.settings)
                != crate::item_lines::visual_blocks_enabled(settings)
        {
            self.scroll.invalidate();
        }
        self.settings = settings.clone();
        let show_reasoning = settings.boolean("transcript.show_reasoning").unwrap_or(false);
        self.transcript.apply_settings_show_reasoning(show_reasoning);
        // The footer is drawn when the turn ends, so this value is what the
        // turn in progress will use: a switch flipped mid-turn applies to it.
        self.transcript
            .set_turn_usage(settings.boolean("transcript.turn_usage").unwrap_or(false));
        // The setting is a number, so it can hold a `-1` that is not a row
        // count: anything that does not fit falls back to the default instead
        // of reading as "hide the preview".
        let output_lines = match settings.number("transcript.tool_output_lines") {
            Some(lines) => usize::try_from(lines).unwrap_or(crate::tool_line::DEFAULT_OUTPUT_LINES),
            None => crate::tool_line::DEFAULT_OUTPUT_LINES,
        };
        self.transcript.set_tool_output_lines(output_lines);
        // Finished tool lines take rows or give them back, which a held view
        // has to recount the same way the expand key does.
        let show_tool_activity = settings.boolean("transcript.show_tool_activity").unwrap_or(true);
        if show_tool_activity != self.transcript.show_tool_activity() {
            self.transcript.set_tool_activity(show_tool_activity);
            self.scroll.reflow();
        }
        // A terminal that already reported the wheel keeps doing so until the
        // loop releases the capture; the events are dropped here once the
        // setting is off, so a stale report cannot scroll a captured terminal.
        self.mouse_scroll = settings.boolean("transcript.mouse_scroll").unwrap_or(true);
        // The list the model cycle keys walk belongs to the settings, not to a
        // session, so every refresh replaces it; the cycle itself reads it on
        // the press, which keeps a file edit one keypress away from the walk.
        // A missing list is the same as an empty one: the press reports it,
        // and a reload must not leave the previous file's names in the walk.
        let shortlist = settings.list("models.shortlist").unwrap_or(&[]);
        self.apply_model_shortlist(shortlist);
        // Read here and acted on where the title is written, which is also the
        // only place that knows whether the terminal keeps a title at all.
        self.title_state = settings.boolean("title.state").unwrap_or(false);
        if let Some(overlay) = self.overlay.as_mut() {
            overlay.refresh(settings);
        }
    }

    /// Show, or hide, the model's reasoning for the rest of this session.
    ///
    /// The choice outranks `transcript.show_reasoning` until that setting value
    /// changes, so an unrelated settings reload keeps the toggle in force. What
    /// was dropped while reasoning was hidden is gone for good: the client
    /// never kept it, and turning reasoning back on starts at the next delta.
    /// The reasoning effort the model is asked for is a different setting and
    /// is not touched.
    pub fn toggle_thinking(&mut self) {
        let show = !self.transcript.show_reasoning();
        self.transcript.set_show_reasoning(show);
        // A live notice rather than a recorded one: the toggle can be pressed
        // while an answer is streaming, and committing that half-written answer
        // to draw a line above it would split the message in two.
        let notice = if show {
            "reasoning shown for this session: it is kept from the next delta, the deltas dropped meanwhile stay gone, and the reasoning effort is unchanged"
        } else {
            "reasoning hidden for this session: later deltas are dropped, the reasoning already committed stays on screen, and the reasoning effort is unchanged"
        };
        self.transcript.push_live_notice(notice.to_string());
    }
}
