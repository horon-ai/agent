//! Directory and identity conventions that differ per operating system.

use std::path::PathBuf;

use crate::Current;
use crate::PlatformAdapter;

/// A short, filesystem-safe discriminator for the current user.
///
/// It keeps a fallback directory private between users that share one temporary
/// directory.
pub fn user_discriminator() -> String {
    Current::user_discriminator()
}

/// The per-user temporary directory this platform provides, when it has one.
pub fn user_runtime_dir() -> Option<PathBuf> {
    Current::user_runtime_dir()
}

/// The per-user configuration directory for `app`.
///
/// macOS keeps application support outside the dotfile namespace, Windows
/// keeps roaming data in `%APPDATA%`, and the other Unix systems follow the XDG
/// base-directory convention. A platform whose environment names no home
/// directory returns `None` rather than a path relative to the working
/// directory, which would put a config file somewhere surprising.
pub fn user_config_dir(app: &str) -> Option<PathBuf> {
    Current::user_config_home().map(|home| home.join(app))
}

/// Whether this desktop can launch a browser for the user.
///
/// `Some(true)` when a launcher is known to exist, `Some(false)` when the
/// platform proves it cannot show anything, and `None` when it cannot tell.
pub fn browser_launcher_state() -> Option<bool> {
    Current::browser_launcher_state()
}

/// Whether this process runs inside a container that hides the desktop.
pub fn is_container() -> bool {
    Current::is_container()
}

/// The path that discards anything written to it.
///
/// Tools that must not touch a real file (Git's global config, for one) point
/// their file variables here.
pub fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}
