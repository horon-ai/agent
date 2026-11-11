//! The typed verdict of a tool call.
//!
//! A tool result used to be text plus an error flag, so a caller that wanted to
//! know whether a call ran, ran in part, or had its result cut had to read the
//! sentence. The verdict states that as a value. The text stays what the model
//! reads; the verdict is what code reads.

use std::fmt;

/// What became of one tool call.
///
/// The names are the wire names: every surface that carries a verdict writes it
/// in snake case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolVerdict {
    /// The tool ran and its whole result was returned.
    Ok,
    /// The tool ran, but only part of the work finished.
    Partial,
    /// The tool ran to the end; the returned text is a cut of the full text.
    Truncated,
    /// The tool ran, and a refusal sentence was returned in place of its
    /// result. Side effects did happen, which is what separates this from
    /// [`ToolVerdict::Rejected`].
    Withheld,
    /// The call was refused before the tool ran.
    Rejected,
    /// The tool ran and returned an error.
    Failed,
    /// The call was skipped or cut short by the user, a reload, or a
    /// cancellation.
    Interrupted,
}

impl ToolVerdict {
    /// The wire name of this verdict.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Partial => "partial",
            Self::Truncated => "truncated",
            Self::Withheld => "withheld",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }

    /// The verdict of a result whose text was cut after `earlier` was decided.
    ///
    /// A cut only replaces "ran and returned everything". A result that already
    /// says the call was partial or failed keeps saying so, because that is the
    /// fact its caller has to act on first.
    pub const fn after_cut(earlier: Option<Self>) -> Self {
        match earlier {
            None | Some(Self::Ok) => Self::Truncated,
            Some(other) => other,
        }
    }
}

impl fmt::Display for ToolVerdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The error of a call that was refused before its tool ran.
///
/// It prints exactly the refusal message, so text built from the error is the
/// same as when the message was a plain string. Its type is the signal: a
/// caller holding the error can tell "never ran" from "ran and failed" without
/// reading the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRejected(pub String);

impl fmt::Display for ToolRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ToolRejected {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolOutput;

    const ALL: [(ToolVerdict, &str); 7] = [
        (ToolVerdict::Ok, "ok"),
        (ToolVerdict::Partial, "partial"),
        (ToolVerdict::Truncated, "truncated"),
        (ToolVerdict::Withheld, "withheld"),
        (ToolVerdict::Rejected, "rejected"),
        (ToolVerdict::Failed, "failed"),
        (ToolVerdict::Interrupted, "interrupted"),
    ];

    #[test]
    fn every_verdict_keeps_its_wire_name_in_both_directions() {
        for (verdict, name) in ALL {
            assert_eq!(serde_json::json!(verdict), serde_json::json!(name));
            assert_eq!(verdict.as_str(), name);
            assert_eq!(verdict.to_string(), name);
            assert!(
                matches!(serde_json::from_value::<ToolVerdict>(serde_json::json!(name)), Ok(read) if read == verdict),
                "{name} must read back as {verdict:?}"
            );
        }
    }

    #[test]
    fn an_unknown_wire_name_is_an_error_not_a_guess() {
        assert!(serde_json::from_value::<ToolVerdict>(serde_json::json!("done")).is_err());
    }

    #[test]
    fn a_new_output_states_no_verdict_until_one_is_set() {
        let plain = ToolOutput::new("text");
        assert_eq!(plain.verdict, None);

        let marked = ToolOutput::new("text").with_verdict(ToolVerdict::Partial);
        assert_eq!(marked.verdict, Some(ToolVerdict::Partial));
        assert_eq!(marked.output, plain.output, "a verdict does not touch the text");
    }

    #[test]
    fn a_cut_replaces_only_a_whole_result() {
        assert_eq!(ToolVerdict::after_cut(None), ToolVerdict::Truncated);
        assert_eq!(ToolVerdict::after_cut(Some(ToolVerdict::Ok)), ToolVerdict::Truncated);
        assert_eq!(ToolVerdict::after_cut(Some(ToolVerdict::Partial)), ToolVerdict::Partial);
        assert_eq!(ToolVerdict::after_cut(Some(ToolVerdict::Failed)), ToolVerdict::Failed);
    }

    #[test]
    fn a_rejection_prints_its_message_and_nothing_else() {
        let rejected = ToolRejected("Tool 'bash' is disabled".to_string());
        assert_eq!(rejected.to_string(), "Tool 'bash' is disabled");
        assert!(std::error::Error::source(&rejected).is_none());
    }
}
