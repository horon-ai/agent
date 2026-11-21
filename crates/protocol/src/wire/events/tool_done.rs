//! Constructors for [`ServerEvent`] variants whose payload is easy to misread.

use super::ServerEvent;

impl ServerEvent {
    /// A `ToolDone` whose verdict no layer recorded. Producing the event this
    /// way keeps the absent verdict explicit at the call site instead of
    /// leaving a field for a later reader to misread as "not considered".
    pub fn tool_done(
        id: impl Into<String>,
        name: impl Into<String>,
        output: impl Into<String>,
        error: Option<String>,
    ) -> Self {
        Self::ToolDone {
            id: id.into(),
            name: name.into(),
            output: output.into(),
            error,
            verdict: None,
        }
    }
}
