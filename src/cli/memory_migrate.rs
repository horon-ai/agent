//! `horon memory migrate`: import the pre-wiki JSON memory files.
//!
//! The import itself lives in `horon_agent_base::memory`; this module is the
//! command's report, kept apart so the memory command dispatcher in
//! `commands.rs` does not grow a second concern.

use anyhow::Result;

/// Run the migration and print its report.
///
/// `dry_run` reports what would be imported and writes nothing. Entries the
/// importer could not store are listed on stderr and the command exits non-zero,
/// because a partial migration must be visible to a script that runs it.
pub(crate) fn run(dry_run: bool) -> Result<()> {
    let report = crate::memory::migrate_pre_wiki_memories(dry_run)?;
    if report.files == 0 {
        println!("No pre-wiki JSON memory files found; nothing to migrate.");
        return Ok(());
    }
    if dry_run {
        println!(
            "Would migrate {} file(s): {} entr{} into the memory wiki. Nothing was written.",
            report.files,
            report.imported,
            if report.imported == 1 { "y" } else { "ies" }
        );
        return Ok(());
    }
    println!(
        "Migrated {} file(s): {} page(s) written, {} already current, {} skipped as newer in the wiki.",
        report.files, report.imported, report.unchanged, report.skipped
    );
    if report.refused.is_empty() {
        return Ok(());
    }
    eprintln!(
        "{} entr{} could not be migrated:",
        report.refused.len(),
        if report.refused.len() == 1 { "y" } else { "ies" }
    );
    for refused in &report.refused {
        eprintln!("  - {refused}");
    }
    anyhow::bail!("{} memory entries were not migrated", report.refused.len());
}
