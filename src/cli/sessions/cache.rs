//! `session cache`: preview, and with `--apply` remove, the derived cache files of
//! sessions no writer owns.
//!
//! Only files the store rebuilds from the original event log are listed. Originals,
//! blobs, the runtime journal and sessions a running agent owns are never touched,
//! and without `--apply` nothing is written.

use std::path::Path;

use anyhow::Result;
use serde_json::json;

use crate::cli::args::CacheArgs;
use crate::session::event_store_api as store;

pub(super) fn run(home: &Path, args: &CacheArgs) -> Result<()> {
    let report = store::cache_reclaim::reclaim_derived(&super::store_paths(home), args.apply)?;
    if args.json {
        let files: Vec<_> = report
            .files
            .iter()
            .map(|file| json!({"session": file.session_id.as_str(), "file": file.name, "bytes": file.bytes}))
            .collect();
        let active: Vec<_> = report.active.iter().map(|id| id.as_str()).collect();
        let output = json!({"applied": args.apply, "bytes": report.bytes(), "files": files, "active_sessions": active});
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }
    for file in &report.files {
        println!("{}\t{}\t{} bytes", file.session_id.as_str(), file.name, file.bytes);
    }
    let verb = if args.apply { "Removed" } else { "Would remove" };
    println!("{verb} {} file(s), {} bytes.", report.files.len(), report.bytes());
    if !report.active.is_empty() {
        println!("Skipped {} session(s) a running agent owns.", report.active.len());
    }
    if !args.apply && !report.files.is_empty() {
        println!("Run again with --apply to remove them. The store rebuilds them from the original logs when needed.");
    }
    Ok(())
}
