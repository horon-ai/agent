//! A stricter approval mode for one run, chosen on the command line.
//!
//! The floor is recorded per session and can only raise the mode in force: a
//! deployment that already asks for every call keeps asking, whatever the flag
//! says, and a session with no floor is untouched. The configured and the
//! ACP-chosen modes are combined with the floor by [`stricter`], never replaced.

use crate::config::ApprovalMode;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, PoisonError};

static FLOORS: LazyLock<Mutex<HashMap<String, ApprovalMode>>> = LazyLock::new(Default::default);

fn rank(mode: ApprovalMode) -> u8 {
    match mode {
        ApprovalMode::Yolo | ApprovalMode::Off => 0,
        ApprovalMode::Write => 1,
        ApprovalMode::All => 2,
    }
}

/// Require at least `mode` for every call in `session_id`.
pub fn set_floor(session_id: &str, mode: ApprovalMode) {
    FLOORS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(session_id.to_owned(), mode);
}

/// Drop any per-session approval floor recorded for `session_id`.
pub fn clear_floor(session_id: &str) {
    FLOORS.lock().unwrap_or_else(PoisonError::into_inner).remove(session_id);
}

/// The stricter of `current` and this session's floor, if it has one.
pub(crate) fn stricter(session_id: &str, current: ApprovalMode) -> ApprovalMode {
    let floor = FLOORS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(session_id)
        .copied();
    match floor {
        Some(floor) if rank(floor) > rank(current) => floor,
        _ => current,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_floor_raises_a_permissive_mode_for_its_own_session_only() {
        set_floor("run-approval-a", ApprovalMode::Write);
        assert_eq!(stricter("run-approval-a", ApprovalMode::Yolo), ApprovalMode::Write);
        assert_eq!(stricter("run-approval-a", ApprovalMode::Off), ApprovalMode::Write);
        assert_eq!(stricter("run-approval-b", ApprovalMode::Yolo), ApprovalMode::Yolo);
    }

    #[test]
    fn a_floor_never_weakens_a_stricter_mode() {
        set_floor("run-approval-c", ApprovalMode::Write);
        assert_eq!(stricter("run-approval-c", ApprovalMode::All), ApprovalMode::All);
        assert_eq!(stricter("run-approval-c", ApprovalMode::Write), ApprovalMode::Write);
    }
}
