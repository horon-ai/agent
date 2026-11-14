//! Whether this build can import the old SQLite session index, and what an
//! operator does when it cannot.
//!
//! The answer is a compile-time fact, so it lives beside the code it gates
//! instead of being re-derived as a `cfg!` at every call site. The CLI reads
//! [`AVAILABLE`] to decide whether `--legacy-index` is offered at all, and
//! [`remedy`] to name the build that can finish the job when it is not.

/// True when this build was compiled with `legacy-session-index-import`.
///
/// The released binaries omit that feature and therefore carry no database
/// handle at all; a build from source can add it with the feature flag.
pub const AVAILABLE: bool = cfg!(feature = "legacy-session-index-import");

/// The one instruction that actually resolves an old index in this build.
///
/// A build without the feature hides the flag from `--help`, so telling the
/// operator to re-run with the flag would name a step whose next hop refuses.
/// The remedy there is the build itself.
pub fn remedy() -> &'static str {
    "install a build compiled with `--features legacy-session-index-import`, then \
     run `horon-agent sessions migrate --apply --legacy-index`. Released binaries \
     omit that feature; they open no database and cannot import it"
}