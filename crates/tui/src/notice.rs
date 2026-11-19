//! What the client says when the harness behind it restarts, stops or goes
//! away.
//!
//! Each of these is drawn as its own two-line entry in the transcript: the
//! fixed [`HEADER`] line, then one sentence. The wording is the operator's own
//! (a Korean request in the issue this shipped under), not this file's
//! invention, so it is kept in one place rather than spelled at each call site.

/// The line that marks a lifecycle entry as an announcement rather than model
/// output or a failure.
pub(crate) const HEADER: &str = "[알림]";

/// The daemon announced a reload and the client is waiting for it to answer
/// again.
pub(crate) const RESTARTING: &str = "서버를 재시작하는 중입니다";

/// The connection came back after a restart and the same session is attached.
pub(crate) const RESTARTED: &str = "서버가 재시작되었습니다";

/// The daemon is gone: it stopped, or a restart never came back.
pub(crate) const STOPPED: &str = "서버가 종료되었습니다";

/// The connection ended without the daemon saying anything about stopping.
pub(crate) const DISCONNECTED: &str = "서버 연결이 끊어졌습니다";
