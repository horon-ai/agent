//! Filesystem values the adapter returns to callers that never branch on the
//! operating system themselves.

/// The identity and timestamps a cache stamp is built from.
///
/// Unix fills every field from the inode; Windows has no device, inode or
/// change time on the stable standard library surface and reports its own
/// replacements for the last two fields (see [`PlatformAdapter::file_stamp`]).
///
/// [`PlatformAdapter::file_stamp`]: crate::PlatformAdapter::file_stamp
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileStamp {
    /// Device the file lives on; `0` where the platform does not expose one.
    pub device: u64,
    /// Inode number; `0` where the platform does not expose one.
    pub inode: u64,
    /// File length in bytes.
    pub len: u64,
    /// Modification time as `(seconds, nanoseconds)`.
    pub modified: (i64, i64),
    /// Inode change time as `(seconds, nanoseconds)`.
    pub changed: (i64, i64),
}

/// The ownership identity of a file.
///
/// Unix fills every field from the inode. Windows reports zeros: it has no
/// device, inode, uid or gid on the stable standard library surface, and a
/// caller comparing identities there compares what it got instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FileIds {
    /// Device the file lives on.
    pub device: u64,
    /// Inode number.
    pub inode: u64,
    /// Owning user id.
    pub uid: u32,
    /// Owning group id.
    pub gid: u32,
}
