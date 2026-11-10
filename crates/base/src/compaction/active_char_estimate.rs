//! Rolling character-count estimate for the active (non-compacted) suffix.

/// Rolling character-count estimate for the active (non-compacted) message
/// suffix.
///
/// Token estimation needs the size of the live message tail without rescanning
/// the entire history on every call, so this caches that sum next to a dirty
/// flag. The value and the flag must always move together: previously they were
/// two independent `CompactionManager` fields, and a code path that updated one
/// without the other silently corrupted token accounting. Keeping the raw
/// fields private and forcing every mutation through these named operations
/// makes that class of bug unrepresentable.
#[derive(Debug, Clone, Default)]
pub(super) struct ActiveCharEstimate {
    chars: usize,
    dirty: bool,
}

impl ActiveCharEstimate {
    /// The currently cached character count. Only trustworthy when not dirty;
    /// readers must consult [`Self::is_dirty`] (and any external invariants)
    /// before relying on it.
    pub(super) fn value(&self) -> usize {
        self.chars
    }

    /// Whether the cached value is stale and must be recomputed from history.
    pub(super) fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Mark the cached value stale so the next read recomputes from history.
    pub(super) fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Record an exact, freshly computed count as the trusted value.
    pub(super) fn set_exact(&mut self, chars: usize) {
        self.chars = chars;
        self.dirty = false;
    }

    /// Extend a trusted count by a newly appended message's characters.
    ///
    /// Mirrors the append-only fast path: the prior value is assumed accurate,
    /// so the running sum stays trusted (dirty cleared).
    pub(super) fn append_exact(&mut self, chars: usize) {
        self.chars = self.chars.saturating_add(chars);
        self.dirty = false;
    }

    /// Reset to zero after a restore/clamp. Stays dirty when there may be active
    /// messages whose characters have not been measured yet.
    pub(super) fn reset_pending(&mut self, maybe_has_active: bool) {
        self.chars = 0;
        self.dirty = maybe_has_active;
    }
}
