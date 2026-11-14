//! Whether this build can import the old SQLite session index, and what an
//! operator does when it cannot.
//!
//! The answer is a compile-time fact about *this* crate, so it is read from this
//! crate's own feature and nowhere else. Reading it from a dependency would be
//! wrong: `horon-agent-harness-api-server` turns the feature on as a
//! dev-dependency (`crates/harness-api-server/Cargo.toml`), which unifies it into
//! every test build of this crate. A flag gated that way would still appear in
//! `--help` of a binary that cannot act on it.

/// True when this binary was compiled with the index-import feature.
///
/// The released binaries omit that feature and carry no database handle at all;
/// a build from source adds it with the feature flag.
pub const AVAILABLE: bool = cfg!(feature = "legacy-session-index-import");

/// The one instruction that actually resolves an old index in this build.
///
/// A build without the feature hides the flag from `--help`, so telling the
/// operator to re-run with the flag would name a step whose next hop refuses.
/// The remedy there is the build itself.
pub fn remedy() -> &'static str {
    "install a build compiled with `cargo build -p horon-agent --features legacy-session-index-import`, then run `horon-agent sessions migrate --apply --legacy-index`. Released binaries omit that feature; they open no database and cannot import it"
}
