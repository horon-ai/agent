//! Receipts for schema or parameter degradations applied to an outgoing request.
//!
//! horon-agent sends the whole tool catalog on every request, so a construct a
//! provider rejects is narrowed before the request goes out: keywords and
//! formats outside the provider's dialect are dropped, a tool that cannot be
//! expressed in strict mode is sent with `strict: false`, and a second
//! definition of the same tool name is skipped. Each of those is a real loss of
//! information the model never sees, and until now nothing recorded it.
//!
//! One [`DegradationReceipt`] is the record of one such loss: what kind it was,
//! which tool it affected, and what specifically was lost. Receipts are stored
//! on the message that carried the turn and mirrored as an additive event, so a
//! caller can tell a reduced tool from a fully honoured one without parsing the
//! provider payloads.

use serde::{Deserialize, Serialize};

/// What kind of information a request lost to provider constraints.
///
/// The set is closed on purpose: a new kind is a protocol change, so it is
/// added deliberately rather than discovered in a payload later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DegradationKind {
    /// A JSON Schema keyword or string format outside the provider's dialect was
    /// removed from the tool's parameters.
    SchemaKeywordDropped,
    /// The tool's parameters could not be expressed in the strict subset, so it
    /// was sent with `strict: false` and structured output is not guaranteed.
    StrictDisabled,
    /// Another definition earlier in the catalog already used this name, so this
    /// definition was not sent.
    DuplicateToolDropped,
}

impl DegradationKind {
    /// Stable wire name, so a reader that stores the string does not have to
    /// depend on the enum's serde representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SchemaKeywordDropped => "schema_keyword_dropped",
            Self::StrictDisabled => "strict_disabled",
            Self::DuplicateToolDropped => "duplicate_tool_dropped",
        }
    }
}

/// One degradation applied while building a request.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DegradationReceipt {
    /// Which kind of information was lost.
    pub kind: DegradationKind,
    /// The tool whose declaration was affected.
    pub tool: String,
    /// What specifically was lost: the dropped keyword, `format:<value>`,
    /// `strict`, or the duplicated tool name.
    pub target: String,
}

impl DegradationReceipt {
    pub fn new(kind: DegradationKind, tool: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            kind,
            tool: tool.into(),
            target: target.into(),
        }
    }
}
