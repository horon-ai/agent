//! Per-call session identity a provider may forward to its backend.

/// Per-call session identity a provider may forward to its backend.
///
/// It exists so a provider can pin a request to the session lineage it belongs to,
/// without writing anything into the shared provider state: two concurrent turns of
/// different sessions must never see each other's identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderRequestContext {
    /// The session issuing this call.
    pub session_id: Option<String>,
    /// The session this one was forked from, when it was forked.
    pub parent_session_id: Option<String>,
    /// The lineage root shared by a whole fork family.
    pub cache_root_id: Option<String>,
    /// Per-call reasoning effort override for this request only; `None` keeps the
    /// configured effort and the provider must not store it.
    pub reasoning_effort: Option<String>,
}

impl ProviderRequestContext {
    /// True when at least one identity member is present.
    pub fn is_present(&self) -> bool {
        self.session_id.is_some() || self.parent_session_id.is_some() || self.cache_root_id.is_some()
    }
}
