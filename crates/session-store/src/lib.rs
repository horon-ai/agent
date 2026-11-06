//! Versioned session event contracts and durable original storage.
//!
//! This crate owns the durable, versioned event log contract for session history:
//! the envelope shape, canonical JSON hashing, event identity, payload/blob record
//! rules, and the typed store error surface. It depends only on
//! `serde`, `serde_json`, `chrono`, `uuid`, `sha2` and
//! the pure `horon-agent-session-types` / `horon-agent-message-types` types.
//!
//! It does not depend on `horon-agent-base`, `horon-agent-app-core`, or any
//! provider runtime. [`WriterHandle`] owns a dedicated blocking I/O thread;
//! [`BlockingWriter`] and the doctor/recovery functions are blocking surfaces.
//! Original blobs and the catalog dirty marker are durable, not disposable caches.
//!
//! # Cross-language canonical form
//!
//! `canonical_json` sorts object keys recursively by UTF-8 byte order and keeps
//! array order. Rust is the only producer of that byte string; consumers (for
//! example the TypeScript SDK) carry the produced string opaquely in
//! [`schema::CanonicalEventRecord`] instead of re-serializing numbers. Object
//! keys are expected to be ASCII identifiers; a key outside ASCII would sort by
//! code point here, which differs from UTF-16 code-unit order above U+FFFF.

#![forbid(unsafe_code)]

pub mod archive;
pub mod blob_exchange;
mod blob_staging;
pub mod blobs;
mod cache_coordination;
mod cache_io;
mod cache_state;
mod cache_tools;
mod cache_values;
pub mod catalog;
mod catalog_row;
pub mod checkpoint;
mod context_cache;
pub mod error;
pub mod heads;
pub mod history_page;
mod index;
pub mod legacy;
mod legacy_bundle;
mod legacy_completion;
mod legacy_events;
mod legacy_index;
mod legacy_parse;
mod legacy_publish;
mod legacy_replay;
mod legacy_runtime;
mod paths;
pub mod projection;
mod projection_boundary;
mod projection_operations;
mod projection_payload;
mod projection_tools;
pub mod reader;
pub mod record_exchange;
mod record_page;
mod record_reference;
pub mod recovery;
pub mod replication;
mod replication_codec;
mod replication_cursor;
mod replication_export;
mod replication_graph;
mod replication_import;
pub mod schema;
pub mod writer;

pub use cache_coordination::with_cache_repair;
pub use cache_io::{ReadProbe, ReadReceipt};
pub use cache_values::INLINE_TAIL_BYTES;
pub use error::{StoreError, StoreErrorCategory, StoreErrorCode, StoreErrorDetails};
pub use heads::EventGraph;
pub use history_page::HistoryPages;
/// The durability level every original publication is confirmed with.
///
/// Derived files are never flushed, so this reaches only original writes.
pub use horon_agent_platform::SyncMode;
pub use paths::SessionPaths;
pub use projection::{ProjectedMessage, Projection};
pub use projection_operations::{EventStamp, ForkRequest, LocalHead, RewindPoint};
pub use recovery::{ScanReport, full_scan, recover_to};
pub use schema::*;
pub use writer::{AppendOutcome, BlockingWriter, WriterHandle};

/// Observable syscall boundaries. Hooks run on the real filesystem backend,
/// immediately before the named sync, or after the named completed operation.
///
/// The derived points (`CacheStaged`, `CacheRenamed`, `CatalogRowStaged`,
/// `CatalogPublished`, `DirtyRemoved`) mark stages of a publication that is
/// intentionally not flushed, so a caller can still inject a failure there
/// even though no syscall follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoPoint {
    LockFileSync,
    DirectorySync,
    DirtyFileSync,
    DirtyDirectorySync,
    BlobWrite,
    BlobFileSync,
    BlobVerified,
    BlobRenamed,
    BlobDirectorySync,
    BlobPublished,
    LogCreated,
    LogWrite,
    LogBodyWritten,
    LogDataSync,
    LogFileSync,
    LogDirectorySync,
    LogPublished,
    QuarantineFileSync,
    QuarantineDirectorySync,
    QuarantinePublished,
    TailTruncated,
    CacheFileSync,
    CacheStaged,
    CacheRenamed,
    CacheDirectorySync,
    CatalogWrite,
    CatalogRowStaged,
    CatalogPublished,
    DirtyRemoved,
}

/// Per-writer instrumentation; no environment variables or global fault state.
pub type IoHook = Box<dyn FnMut(IoPoint) -> std::io::Result<()> + Send>;
