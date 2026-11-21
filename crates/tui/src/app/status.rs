//! Status line data and animation state.

use super::App;
use crate::statusline::StatusData;

impl App {
    /// The one-line status: what we are attached to and what it is doing.
    pub fn status_line(&self) -> String {
        self.status_parts().join(" · ")
    }

    /// The status line's parts, for a renderer that chooses its own separator
    /// and colours.
    pub fn status_parts(&self) -> Vec<String> {
        let mut parts = vec![self.description.clone()];
        if let Some(model) = &self.model {
            parts.push(model.clone());
        }
        if let Some(provider) = &self.provider {
            parts.push(provider.clone());
        }
        let state = if self.disconnected {
            "disconnected"
        } else if self.pending_turn {
            "running"
        } else {
            self.session_status.as_deref().unwrap_or("idle")
        };
        parts.push(state.to_string());
        parts
    }

    /// The state the status-line segments read.
    ///
    /// The clock is left empty here: the renderer fills it once per frame, so
    /// the app state never holds a time that goes stale.
    pub fn status_data(&self) -> StatusData<'_> {
        StatusData {
            spinner: self.spinner_glyph(),
            provider: self.provider.as_deref(),
            model: self.model.as_deref(),
            effort: self.reasoning_effort.as_deref(),
            session_id: self.session_id.as_deref(),
            pending_turn: self.pending_turn,
            disconnected: self.disconnected,
            session_status: self.session_status.as_deref(),
            prompt: self.prompt_label(),
            cwd: self.cwd.as_deref(),
            home: self.home.as_deref(),
            tokens: self.tokens,
            now: None,
        }
    }

    /// Whether the screen animates on its own right now.
    ///
    /// A turn in flight is the only source of motion, and only a `full` preset
    /// draws a moving spinner for it: `reduced` leaves one still mark, `static`
    /// none, and a parked prompt or a lost connection draws none, so the loop
    /// has no reason to add frames for any of them.
    pub fn animating(&self) -> bool {
        self.motion.turns() && self.spinner_glyph().is_some()
    }

    /// The spinner glyph the status segment draws, if any.
    ///
    /// Only a turn in flight animates, and only while the preset allows it: a
    /// prompt or a lost connection already says more than a turning mark, and
    /// `reduced` freezes on the first frame while `static` draws none.
    fn spinner_glyph(&self) -> Option<&'static str> {
        if !self.pending_turn || self.disconnected || self.prompt.is_some() || !self.running {
            return None;
        }
        self.motion.glyph(self.symbols, self.motion_frame)
    }
}
