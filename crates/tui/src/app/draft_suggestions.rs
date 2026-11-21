//! Local suggestions for the draft composer.

use std::time::Instant;

use super::App;

impl App {
    /// Bring the local draft suggestion in line with the draft and the setting.
    ///
    /// A changed draft starts a new generation and asks the local provider
    /// again; an unchanged one keeps the answer it has, so a dismiss or an
    /// accepted line stays put until the next edit. The provider reads only the
    /// drafts this editor already holds, and the answer is dropped unless it
    /// names the generation and the exact text it was asked about.
    pub fn sync_suggestions(&mut self) {
        if !self.settings.boolean("editor.suggestions").unwrap_or(false) {
            self.suggestions.withdraw();
            return;
        }
        let fingerprint = self.input.fingerprint();
        if self.suggestions.is_synced(fingerprint) {
            return;
        }
        // The cap is checked before the text is built, so a huge draft costs a
        // length read rather than a copy.
        if self.input.char_count() > crate::suggestions::MAX_DRAFT_CHARS {
            self.suggestions.skip(fingerprint);
            return;
        }
        let text = self.input.text();
        let App {
            history, suggestions, ..
        } = self;
        let Some(request) = suggestions.begin(fingerprint, &text) else {
            return;
        };
        let started = Instant::now();
        let suffix = crate::suggestions::local_suffix(&text, history.drafts());
        let elapsed = started.elapsed();
        if let Some(suffix) = suffix {
            suggestions.finish(&request, &suffix, elapsed);
        }
    }

    /// The ghost suffix the editor draws after the caret, when one applies.
    ///
    /// The suffix belongs to the exact draft it completes, so it is looked up
    /// against the text on screen rather than trusted from the last answer.
    pub fn suggestion_text(&self) -> Option<String> {
        if self.prompt.is_some() || self.overlay.is_some() || self.picker.is_some() {
            return None;
        }
        if !self.suggestions.pending() || !self.input.is_at_end() {
            return None;
        }
        let text = self.input.text();
        self.suggestions.visible(&text).map(str::to_string)
    }

    /// Accept the suggestion for the draft on screen.
    ///
    /// This is the only place the suffix reaches the input: until here the
    /// draft is exactly what the user typed, and nothing is sent or queued.
    pub(super) fn accept_suggestion(&mut self) -> bool {
        if !self.input.is_at_end() {
            return false;
        }
        let text = self.input.text();
        let Some(full) = self.suggestions.accept(&text) else {
            return false;
        };
        self.history.reset();
        self.input.set_text(&full);
        true
    }

    /// Whether a suggestion for the draft on screen can take the next key.
    pub(super) fn suggestion_available(&self) -> bool {
        if !self.suggestions.pending() || !self.input.is_at_end() {
            return false;
        }
        let text = self.input.text();
        self.suggestions.visible(&text).is_some()
    }
}
