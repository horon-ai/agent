//! `horon-agent companion`: the per-login helper for desktop-only effects.
//!
//! The harness server and agent runtime stay free of screens, keyboards,
//! microphones and lids. This process runs as the user, inside their graphical
//! session, and handles those on request over a local socket
//! ([`horon_agent_base::companion_client`]):
//!
//! * `open` / `reveal`: URLs and files with the desktop's own tools.
//! * `spawn_terminal`: new terminal windows in the user's session.
//! * `power_activity`: the macOS/Windows lid-close override while agents work.
//! * global voice: the Copilot-key push-to-talk listener (Linux), so voice
//!   reaches a focused HoronAgent CLI even when HoronAgent Desktop is not open.
//! * browser bridge: periodic auto-update, off the browser tool's hot path.
//!
//! Test with `justrust test -p horon-agent-companion`.

pub mod lid_override;
mod server;
#[cfg(target_os = "linux")]
mod voice;

pub use server::{Companion, CompanionOptions, run};
