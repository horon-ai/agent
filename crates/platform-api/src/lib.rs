//! Contract shared by the platform adapter crates.
//!
//! `horon-agent-platform` compiles exactly one implementation - macOS, Linux,
//! or Windows - and exposes it as free functions, so core crates never branch
//! on the operating system. The trait deliberately stays object-free: every
//! caller is compiled against a concrete `Current` adapter and nothing needs
//! dynamic dispatch.

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;

use rustls::ClientConfig;

/// How hard a durability flush pushes the data to the device.
///
/// The setting describes a durability level, not a syscall: the adapter picks
/// the cheapest call on each operating system that meets it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SyncMode {
    /// The platform's strongest guarantee that the write survives a power loss.
    ///
    /// On macOS this is `F_FULLFSYNC` through std's `sync_all`, which also
    /// flushes the drive's write cache.
    #[default]
    Full,
    /// The cheapest flush that still survives an operating-system crash.
    ///
    /// On macOS this is a plain `fsync(2)`. Linux and Windows have no cheaper
    /// call than `Full`, so the mode does not change their behaviour.
    Fast,
}

/// One operating system's implementation of the primitives core code shares.
///
/// The methods are associated functions: the adapter is a namespace for one
/// platform's syscalls, carries no state, and is never used behind a trait
/// object. The process-wide TLS configuration is cached by
/// `horon-agent-platform`.
pub trait PlatformAdapter {
    /// Flush `file`'s contents according to `mode`.
    fn sync_file(file: &File, mode: SyncMode) -> io::Result<()>;

    /// Flush the directory entry of an existing directory so a file created or
    /// renamed inside it survives a crash.
    ///
    /// The caller has already checked that `path` names a directory. Windows
    /// cannot open a directory for a flush without backup semantics and NTFS
    /// journals directory entries itself, so its implementation returns
    /// `Ok(())` and leaves the identity check to the caller.
    fn sync_dir(path: &Path, mode: SyncMode) -> io::Result<()>;

    /// Build the client TLS configuration this operating system trusts.
    ///
    /// The trust set is the bundled webpki roots plus the roots the operating
    /// system trusts, and it is never narrower than that union.
    fn tls_client_config() -> Arc<ClientConfig>;
}
