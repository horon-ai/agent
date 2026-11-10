//! Non-destructive session operations: inspect, doctor, migrate, exchange,
//! legacy downgrade export, salvage, and archive records.
//!
//! Every command here is read-only unless it says otherwise: `migrate` is a dry
//! run until `--apply`, `doctor` never repairs, `recover-to` writes only to a
//! new root, and archive/restore append a record instead of deleting anything.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use crate::cli::args::SessionCommand;
use crate::session;
use crate::session::event_store_api as store;

const BUNDLE_KIND: &str = "horon-agent-session-bundle";
const BUNDLE_VERSION: u32 = 1;

pub(crate) fn run_session_command(command: SessionCommand) -> Result<()> {
    match command {
        SessionCommand::Rename {
            session,
            name,
            clear,
            json,
        } => super::commands::run_session_rename_command(&session, name.as_deref(), clear, json),
        SessionCommand::Inspect { session, json } => inspect(&home()?, &session, json),
        SessionCommand::Doctor { session, json } => doctor(&home()?, session.as_deref(), json),
        SessionCommand::Migrate {
            session,
            all,
            from,
            apply,
            dry_run,
            legacy_index,
            json,
        } => migrate(
            &home()?,
            MigrateOptions {
                sessions: session,
                all,
                from,
                apply: apply && !dry_run,
                legacy_index,
            },
            json,
        ),
        SessionCommand::Export { session, output, json } => export(&home()?, &session, &output, json),
        SessionCommand::Import { input, json } => import(&home()?, &input, json),
        SessionCommand::ExportLegacy {
            session,
            output,
            head,
            json,
        } => export_legacy(&home()?, &session, &output, head.as_deref(), json),
        SessionCommand::RecoverTo { session, output, json } => recover_to(&home()?, &session, &output, json),
        SessionCommand::Archive { session, json } => archive(&home()?, &session, true, json),
        SessionCommand::Restore { session, json } => archive(&home()?, &session, false, json),
    }
}

fn home() -> Result<PathBuf> {
    let home = crate::storage::horon_agent_dir()?;
    Ok(if home.exists() { home.canonicalize()? } else { home })
}

fn store_paths(home: &Path) -> store::SessionPaths {
    store::SessionPaths::new(home.join("sessions"))
}

fn parse_id(raw: &str) -> Result<store::SessionId> {
    store::SessionId::parse(raw).map_err(|error| anyhow::anyhow!("{}", error.message))
}

fn resolve_path(raw: &str) -> Result<PathBuf> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        bail!("a path is required");
    }
    let expanded = if let Some(rest) = trimmed.strip_prefix("~/") {
        dirs::home_dir().context("home directory is unavailable")?.join(rest)
    } else if trimmed == "~" {
        dirs::home_dir().context("home directory is unavailable")?
    } else {
        PathBuf::from(trimmed)
    };
    if expanded
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("refusing a path that contains '..': {}", expanded.display());
    }
    Ok(expanded)
}

/// Resolve a user-supplied output path to the real path the OS writes through.
///
/// The part of the path that already exists is canonicalized, so a caller whose
/// path crosses a symlinked ancestor (`/var` on macOS) is compared and written as
/// the same path the strict session-store path checks see. Components that do not
/// exist yet cannot be symlinks, so they are appended unchanged.
fn canonical_output_path(path: PathBuf) -> Result<PathBuf> {
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut current = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .context("the current directory is unavailable")?
            .join(path)
    };
    loop {
        match current.canonicalize() {
            Ok(mut resolved) => {
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = current
                    .file_name()
                    .context("an output path has no existing ancestor")?
                    .to_os_string();
                missing.push(name);
                current = current
                    .parent()
                    .context("an output path has no existing ancestor")?
                    .to_path_buf();
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn create_private_dir(path: &Path) -> Result<()> {
    // The directory is created owner-only and then re-restricted, so a path
    // that already existed with a wider mode ends up private as well.
    horon_agent_platform::create_private_dir(path, true)?;
    horon_agent_platform::set_permissions_private_dir(path)?;
    Ok(())
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn emit(json_output: bool, value: &Value) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

fn code_name(code: store::StoreErrorCode) -> String {
    format!("{code:?}")
}

fn layout_name(layout: store::reader::SourceLayout) -> &'static str {
    match layout {
        store::reader::SourceLayout::Canonical => "canonical",
        store::reader::SourceLayout::Legacy => "legacy",
    }
}

/// Write a session's Markdown transcript to stdout or a file.
///
/// The text comes from the same renderer the `export_session_markdown` harness
/// API reply uses, so a script and an SDK client see identical bytes.
pub(crate) fn export_markdown(session: &str, out: Option<&Path>, json_output: bool) -> Result<()> {
    let id = session::find_session_by_name_or_id(session)?;
    let loaded = session::Session::load(&id)?;
    let markdown = crate::session_export::export_session_markdown(&loaded)?;

    if json_output {
        let value = json!({ "session_id": id, "markdown": markdown });
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    match out {
        Some(path) => {
            if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
            }
            std::fs::write(path, markdown.as_bytes()).with_context(|| format!("failed to write {}", path.display()))?;
        }
        None => {
            let mut stdout = std::io::stdout().lock();
            stdout.write_all(markdown.as_bytes())?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn inspect(home: &Path, session: &str, json_output: bool) -> Result<()> {
    let id = parse_id(session)?;
    let paths = store_paths(home);
    let Some(layout) = store::reader::layout(&paths, &id)? else {
        bail!(
            "session {} has no original under {}",
            id.as_str(),
            paths.root().display()
        );
    };
    let mut report = json!({"session_id": id.as_str(), "layout": layout_name(layout)});
    match layout {
        store::reader::SourceLayout::Canonical => {
            let log = paths.events_log(&id)?;
            let scan = store::full_scan(&paths, &id)?;
            report["log"] = json!(log.display().to_string());
            report["events"] = json!(scan.events.len());
            report["complete_offset"] = json!(scan.complete_offset);
            report["torn_tail_bytes"] = json!(scan.torn_tail.len());
            report["damage"] = match &scan.damage {
                Some(error) => json!({"code": code_name(error.code), "message": error.message}),
                None => Value::Null,
            };
            if scan.damage.is_none() && scan.torn_tail.is_empty() {
                let graph = store::EventGraph::open(&paths, &id)?;
                let heads: Vec<String> = graph.heads().iter().map(|head| head.as_str().to_owned()).collect();
                report["heads"] = json!(heads);
                let view = match graph.replay(None) {
                    Ok(view) => view,
                    Err(error) if error.code == store::StoreErrorCode::ConflictHeads => {
                        graph.replay(Some(&graph.shared_head()?))?
                    }
                    Err(error) => return Err(error.into()),
                };
                report["selected_head"] = json!(view.head().as_str());
                report["messages"] = json!(view.messages().len());
                report["title"] = json!(view.metadata().title);
                report["custom_title"] = json!(view.metadata().custom_title);
                report["saved"] = json!(view.metadata().saved);
                report["working_dir"] = json!(view.metadata().working_dir);
                report["archived"] = json!(view.archive().is_some_and(|state| state.archived));
                report["compaction"] = json!(view.compaction().is_some());
                report["auxiliary"] = json!(view.auxiliary().len());
                report["blocked"] = json!(view.blocked().len());
            }
        }
        store::reader::SourceLayout::Legacy => {
            match store::legacy::LegacySource::new(home, id.clone()).and_then(|source| source.read()) {
                Ok(read) => {
                    report["messages"] = json!(read.messages.len());
                    report["title"] = json!(read.metadata.title);
                    report["custom_title"] = json!(read.metadata.custom_title);
                    report["working_dir"] = json!(read.metadata.working_dir);
                    report["saved"] = json!(read.metadata.saved);
                    report["diagnostics"] = json!(read.diagnostics);
                    report["migration_required"] = json!(false);
                }
                Err(error) if error.code == store::StoreErrorCode::MigrationRequired => {
                    report["migration_required"] = json!(true);
                    report["message"] = json!(error.message);
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    if json_output {
        return emit(true, &report);
    }
    println!("session: {}", report["session_id"].as_str().unwrap_or(""));
    println!("layout:  {}", report["layout"].as_str().unwrap_or(""));
    for key in [
        "log",
        "events",
        "heads",
        "selected_head",
        "messages",
        "title",
        "archived",
        "compaction",
        "torn_tail_bytes",
        "damage",
        "migration_required",
    ] {
        if let Some(value) = report.get(key).filter(|value| !value.is_null()) {
            println!("{key}: {value}");
        }
    }
    Ok(())
}

fn doctor(home: &Path, session: Option<&str>, json_output: bool) -> Result<()> {
    let paths = store_paths(home);
    let ids = match session {
        Some(raw) => vec![parse_id(raw)?],
        None => store::reader::sources(&paths)?
            .into_iter()
            .map(|source| source.session_id)
            .collect(),
    };
    if ids.is_empty() {
        bail!("no session originals under {}", paths.root().display());
    }
    let mut rows = Vec::new();
    let mut damaged = 0usize;
    let mut migration_required = 0usize;
    for id in ids {
        let Some(layout) = store::reader::layout(&paths, &id)? else {
            continue;
        };
        match layout {
            store::reader::SourceLayout::Canonical => {
                let scan = store::full_scan(&paths, &id)?;
                if scan.damage.is_some() {
                    damaged += 1;
                }
                rows.push(json!({
                    "session_id": id.as_str(),
                    "layout": "canonical",
                    "events": scan.events.len(),
                    "complete_offset": scan.complete_offset,
                    "torn_tail_bytes": scan.torn_tail.len(),
                    "damage": match &scan.damage {
                        Some(error) => json!({"code": code_name(error.code), "message": error.message}),
                        None => Value::Null,
                    },
                }));
            }
            store::reader::SourceLayout::Legacy => {
                match store::legacy::LegacySource::new(home, id.clone()).and_then(|source| source.read()) {
                    Ok(read) => rows.push(json!({
                        "session_id": id.as_str(),
                        "layout": "legacy",
                        "messages": read.messages.len(),
                        "diagnostics": read.diagnostics,
                        "damage": Value::Null,
                    })),
                    Err(error) if error.code == store::StoreErrorCode::MigrationRequired => {
                        migration_required += 1;
                        rows.push(json!({
                            "session_id": id.as_str(),
                            "layout": "legacy",
                            "damage": Value::Null,
                            "migration_required": true,
                            "message": error.message,
                        }));
                    }
                    Err(error) => {
                        damaged += 1;
                        rows.push(json!({
                            "session_id": id.as_str(),
                            "layout": "legacy",
                            "damage": {"code": code_name(error.code), "message": error.message},
                        }));
                    }
                }
            }
        }
    }
    let report = json!({
        "sessions": rows,
        "damaged": damaged,
        "migration_required": migration_required,
    });
    if json_output {
        emit(true, &report)?;
    } else {
        for row in report["sessions"].as_array().into_iter().flatten() {
            println!(
                "{} [{}] events={} damage={}",
                row["session_id"].as_str().unwrap_or(""),
                row["layout"].as_str().unwrap_or(""),
                row["events"].as_u64().unwrap_or(0),
                row["damage"]
            );
        }
        println!(
            "checked {} session(s): {damaged} damaged, {migration_required} need an offline index migration",
            report["sessions"].as_array().map(Vec::len).unwrap_or(0)
        );
    }
    if damaged > 0 {
        bail!("doctor found damage in {damaged} session original(s)");
    }
    Ok(())
}

struct MigrateOptions {
    sessions: Vec<String>,
    all: bool,
    from: Option<String>,
    apply: bool,
    legacy_index: bool,
}

fn legacy_index_present(home: &Path) -> bool {
    ["", "-wal", "-shm"]
        .iter()
        .any(|suffix| home.join(format!("session-metadata-v1.sqlite3{suffix}")).exists())
}

fn migrate(home: &Path, options: MigrateOptions, json_output: bool) -> Result<()> {
    let from_home = match options.from.as_deref() {
        Some(raw) => {
            let path = resolve_path(raw)?;
            if !path.is_dir() {
                bail!("legacy home {} does not exist", path.display());
            }
            path.canonicalize()?
        }
        None => home.to_path_buf(),
    };
    let source_paths = store_paths(&from_home);
    let destination = store_paths(home);
    let ids = if options.sessions.is_empty() {
        if !options.all {
            bail!("pass one or more session ids, or --all to migrate every legacy original");
        }
        store::reader::sources(&source_paths)?
            .into_iter()
            .filter(|source| source.layout == store::reader::SourceLayout::Legacy)
            .map(|source| source.session_id)
            .collect::<Vec<_>>()
    } else {
        options
            .sessions
            .iter()
            .map(|raw| parse_id(raw))
            .collect::<Result<Vec<_>>>()?
    };
    if ids.is_empty() {
        bail!(
            "no legacy session originals found under {}",
            source_paths.root().display()
        );
    }
    let index_present = legacy_index_present(&from_home);
    let mut reports = Vec::new();
    let mut failures = Vec::new();
    for id in ids {
        match migrate_one(
            &from_home,
            &destination,
            &id,
            options.apply,
            options.legacy_index,
            index_present,
        ) {
            Ok(report) => reports.push(report),
            Err(error) => failures.push(json!({
                "session_id": id.as_str(),
                "error": error.to_string(),
            })),
        }
    }
    let report = json!({
        "mode": if options.apply { "apply" } else { "dry-run" },
        "from": from_home.display().to_string(),
        "destination": destination.root().display().to_string(),
        "feature_enabled": cfg!(feature = "legacy-session-index-import"),
        "sessions": reports,
        "failures": failures,
    });
    if json_output {
        emit(true, &report)?;
    } else {
        println!(
            "migrate ({}) from {} into {}",
            report["mode"].as_str().unwrap_or(""),
            from_home.display(),
            destination.root().display()
        );
        for row in report["sessions"].as_array().into_iter().flatten() {
            println!(
                "  {} status={} already_migrated={} index={}",
                row["session_id"].as_str().unwrap_or(""),
                row["status"].as_str().unwrap_or(""),
                row["already_migrated"].as_bool().unwrap_or(false),
                row["index_present"].as_bool().unwrap_or(false)
            );
        }
        for row in report["failures"].as_array().into_iter().flatten() {
            println!(
                "  {} FAILED: {}",
                row["session_id"].as_str().unwrap_or(""),
                row["error"].as_str().unwrap_or("")
            );
        }
    }
    if !report["failures"].as_array().is_none_or(Vec::is_empty) {
        bail!(
            "migration failed for {} session(s); originals were not modified",
            report["failures"].as_array().map(Vec::len).unwrap_or(0)
        );
    }
    Ok(())
}

fn migrate_one(
    from_home: &Path,
    destination: &store::SessionPaths,
    id: &store::SessionId,
    apply: bool,
    legacy_index: bool,
    index_present: bool,
) -> Result<Value> {
    let source = store::legacy::LegacySource::new(from_home, id.clone())?;
    let already_migrated = store::legacy::read_receipt(destination, id).is_ok();
    if !apply {
        let mut report = json!({
            "session_id": id.as_str(),
            "mode": "dry-run",
            "destination": destination.session_dir(id)?.display().to_string(),
            "already_migrated": already_migrated,
            "index_present": index_present,
        });
        match source.read() {
            Ok(read) => {
                report["status"] = json!("ready");
                report["messages"] = json!(read.messages.len());
                report["title"] = json!(read.metadata.title);
                report["diagnostics"] = json!(read.diagnostics);
            }
            Err(error) if error.code == store::StoreErrorCode::MigrationRequired => {
                report["status"] = json!("index-import-required");
                report["message"] = json!(error.message);
            }
            Err(error) => return Err(error.into()),
        }
        return Ok(report);
    }
    if index_present && !legacy_index {
        bail!(
            "session {} has a legacy SQLite index; re-run with --legacy-index to import its display metadata offline",
            id.as_str()
        );
    }
    if index_present && !cfg!(feature = "legacy-session-index-import") {
        bail!(
            "MigrationRequired: this build cannot read session-metadata-v1.sqlite3. Rebuild with `--features legacy-session-index-import` and run `horon-agent sessions migrate --apply --legacy-index`. The database was not opened."
        );
    }
    let report = source.migrate(destination)?;
    let outcome = match report.outcome {
        store::legacy::MigrationOutcome::Published => "published",
        store::legacy::MigrationOutcome::AlreadyPublished => "already-published",
    };
    Ok(json!({
        "session_id": id.as_str(),
        "mode": "apply",
        "status": outcome,
        "already_migrated": outcome == "already-published",
        "index_present": index_present,
        "events": report.receipt.events.len(),
        "index_imported": report.receipt.index_imported,
        "diagnostics": report.receipt.diagnostics,
    }))
}

fn export(home: &Path, session: &str, output: &str, json_output: bool) -> Result<()> {
    let id = parse_id(session)?;
    let paths = store_paths(home);
    let Some(layout) = store::reader::layout(&paths, &id)? else {
        bail!("session {} has no original to export", id.as_str());
    };
    if layout != store::reader::SourceLayout::Canonical {
        bail!(
            "session {} still uses the legacy layout; run `sessions migrate --apply` first",
            id.as_str()
        );
    }
    let destination = resolve_path(output)?;
    if destination.symlink_metadata().is_ok() {
        bail!(
            "output path already exists; refusing to overwrite {}",
            destination.display()
        );
    }
    create_private_dir(&destination)?;
    let pages_dir = destination.join("pages");
    let blobs_dir = destination.join("blobs");
    let records_dir = destination.join("records");
    create_private_dir(&pages_dir)?;
    create_private_dir(&blobs_dir)?;
    create_private_dir(&records_dir)?;

    let mut heads: Vec<store::EventId> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages: Vec<store::replication::SessionEventPage> = Vec::new();
    let mut blobs: BTreeMap<String, store::PayloadRef> = BTreeMap::new();
    let mut records: BTreeMap<String, store::record_exchange::CanonicalRecordRef> = BTreeMap::new();
    loop {
        let request = store::replication::ExportRequest {
            protocol_version: store::replication::REPLICATION_PROTOCOL_VERSION,
            session_id: id.clone(),
            heads: heads.clone(),
            known_ids: Vec::new(),
            cursor: cursor.clone(),
        };
        let page = store::replication::export_events(&paths, &request)?;
        heads = page.heads.clone();
        for reference in &page.blob_refs {
            blobs.insert(reference.sha256.clone(), reference.clone());
        }
        for reference in &page.record_refs {
            records.insert(reference.sha256.clone(), reference.clone());
        }
        cursor = page.next_cursor.clone();
        pages.push(page);
        if cursor.is_none() {
            break;
        }
    }
    for (index, page) in pages.iter().enumerate() {
        let path = pages_dir.join(format!("{index:05}.json"));
        write_file(&path, &serde_json::to_vec(page)?)?;
    }
    for (sha256, reference) in &blobs {
        let bytes = store::blob_exchange::read_blob(&paths, &id, reference)?;
        write_file(&blobs_dir.join(format!("{sha256}.json")), &bytes)?;
    }
    for (sha256, reference) in &records {
        let bytes = store::record_exchange::read_record_blob(&paths, &id, reference)?;
        write_file(&records_dir.join(format!("{sha256}.json")), &bytes)?;
    }
    let manifest = json!({
        "kind": BUNDLE_KIND,
        "version": BUNDLE_VERSION,
        "session_id": id.as_str(),
        "protocol_version": store::replication::REPLICATION_PROTOCOL_VERSION,
        "heads": heads.iter().map(|head| head.as_str()).collect::<Vec<_>>(),
        "pages": pages.len(),
        "blobs": blobs.values().collect::<Vec<_>>(),
        "records": records.values().collect::<Vec<_>>(),
    });
    write_file(
        &destination.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    let report = json!({
        "session_id": id.as_str(),
        "output": destination.display().to_string(),
        "pages": pages.len(),
        "events": pages.iter().map(|page| page.events.len()).sum::<usize>(),
        "record_refs": records.len(),
        "blobs": blobs.len(),
        "heads": manifest["heads"],
    });
    if json_output {
        emit(true, &report)?;
    } else {
        println!(
            "exported {} ({} pages, {} events, {} blobs) to {}",
            id.as_str(),
            report["pages"],
            report["events"],
            report["blobs"],
            destination.display()
        );
    }
    Ok(())
}

fn import(home: &Path, input: &str, json_output: bool) -> Result<()> {
    let bundle = resolve_path(input)?;
    if !bundle.is_dir() {
        bail!("bundle directory {} does not exist", bundle.display());
    }
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(bundle.join("manifest.json"))
            .with_context(|| format!("bundle {} has no manifest.json", bundle.display()))?,
    )?;
    if manifest["kind"] != json!(BUNDLE_KIND) {
        bail!("bundle manifest is not a {BUNDLE_KIND}");
    }
    if manifest["version"].as_u64() != Some(u64::from(BUNDLE_VERSION)) {
        bail!("unsupported bundle version {:?}", manifest["version"].as_u64());
    }
    let session_id = manifest["session_id"]
        .as_str()
        .context("bundle manifest has no session_id")?;
    let id = parse_id(session_id)?;
    let paths = store_paths(home);

    let blobs: Vec<store::PayloadRef> = serde_json::from_value(manifest["blobs"].clone())?;
    for reference in &blobs {
        let path = bundle.join("blobs").join(format!("{}.json", reference.sha256));
        let bytes = std::fs::read(&path).with_context(|| format!("bundle is missing blob {}", reference.sha256))?;
        store::blob_exchange::write_blob(&paths, &id, reference, &bytes)?;
    }
    let records: Vec<store::record_exchange::CanonicalRecordRef> = serde_json::from_value(manifest["records"].clone())?;
    for reference in &records {
        let path = bundle.join("records").join(format!("{}.json", reference.sha256));
        let bytes = std::fs::read(&path).with_context(|| format!("bundle is missing record {}", reference.sha256))?;
        store::record_exchange::write_record_blob(&paths, &id, reference, &bytes)?;
    }

    let page_count = manifest["pages"]
        .as_u64()
        .context("bundle manifest has no page count")?;
    let mut accepted = Vec::new();
    let mut heads = Vec::new();
    let mut conflicts = Vec::new();
    let mut pages = 0u64;
    for index in 0..page_count {
        let path = bundle.join("pages").join(format!("{index:05}.json"));
        let page: store::replication::SessionEventPage = serde_json::from_slice(&std::fs::read(&path)?)?;
        if page.session_id != id {
            bail!(
                "bundle page {index} belongs to {}, not {}",
                page.session_id.as_str(),
                id.as_str()
            );
        }
        let result = store::replication::import_events(&paths, &page)?;
        accepted.extend(result.accepted_ids);
        heads = result.heads;
        conflicts = result.conflict_heads;
        pages += 1;
    }
    let report = json!({
        "session_id": id.as_str(),
        "input": bundle.display().to_string(),
        "pages": pages,
        "accepted": accepted.len(),
        "heads": heads.iter().map(|head| head.as_str()).collect::<Vec<_>>(),
        "conflict_heads": conflicts.iter().map(|head| head.as_str()).collect::<Vec<_>>(),
        "blobs": blobs.len(),
        "records": records.len(),
    });
    if json_output {
        emit(true, &report)?;
    } else {
        println!(
            "imported {} from {}: {} pages, {} events accepted, {} blobs",
            id.as_str(),
            bundle.display(),
            pages,
            report["accepted"],
            report["blobs"]
        );
    }
    Ok(())
}

fn export_legacy(home: &Path, session: &str, output: &str, head: Option<&str>, json_output: bool) -> Result<()> {
    let id = parse_id(session)?;
    let paths = store_paths(home);
    let Some(layout) = store::reader::layout(&paths, &id)? else {
        bail!("session {} has no original to export", id.as_str());
    };
    if layout != store::reader::SourceLayout::Canonical {
        bail!(
            "session {} is already in the legacy layout; nothing to export",
            id.as_str()
        );
    }
    let destination = canonical_output_path(resolve_path(output)?)?;
    if destination == home {
        bail!(
            "refusing to write the legacy export over the current HOME {}",
            home.display()
        );
    }
    let graph = store::EventGraph::open(&paths, &id)?;
    let selected = match head {
        Some(raw) => {
            let candidate = store::EventId::parse(raw)?;
            if graph.event(&candidate).is_none() {
                bail!("head {raw} is not part of session {}", id.as_str());
            }
            candidate
        }
        None => graph.selected_head(None).map_err(|error| {
            anyhow::anyhow!(
                "{}; choose one branch with --head (heads: {})",
                error.message,
                graph
                    .heads()
                    .iter()
                    .map(|head| head.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?,
    };
    let view = graph.replay(Some(&selected))?;
    let metadata = serde_json::to_value(view.metadata())?;
    let mut snapshot = metadata
        .as_object()
        .cloned()
        .context("session metadata is not an object")?;
    snapshot.insert("id".into(), json!(id.as_str()));
    snapshot.insert(
        "messages".into(),
        serde_json::to_value(
            view.messages()
                .iter()
                .map(|message| message.message.clone())
                .collect::<Vec<_>>(),
        )?,
    );
    if let Some((_, compaction)) = view.compaction() {
        snapshot.insert("compaction".into(), serde_json::to_value(compaction)?);
    }
    let sessions = destination.join("sessions");
    let target = sessions.join(format!("{}.json", id.as_str()));
    if target.symlink_metadata().is_ok() {
        bail!(
            "refusing to overwrite the existing legacy original {}",
            target.display()
        );
    }
    create_private_dir(&sessions)?;
    write_file(&target, &serde_json::to_vec_pretty(&Value::Object(snapshot))?)?;
    let archived = view.archive().is_some_and(|state| state.archived);
    if archived {
        let archive = destination.join("sdk-archive.json");
        if archive.symlink_metadata().is_err() {
            write_file(
                &archive,
                &serde_json::to_vec_pretty(&json!({
                    "sessions": {id.as_str(): chrono::Utc::now().timestamp_millis()},
                    "restored": [],
                    "archive_after_days": Value::Null,
                }))?,
            )?;
        }
    }
    let verified = store::legacy::LegacySource::new(&destination, id.clone())
        .and_then(|source| source.read())
        .with_context(|| format!("the legacy export at {} could not be read back", target.display()))?;
    if verified.messages.len() != view.messages().len() {
        bail!(
            "legacy export verification failed: wrote {} messages, read back {}",
            view.messages().len(),
            verified.messages.len()
        );
    }
    let report = json!({
        "session_id": id.as_str(),
        "output": destination.display().to_string(),
        "snapshot": target.display().to_string(),
        "head": selected.as_str(),
        "messages": verified.messages.len(),
        "title": verified.metadata.title,
        "archived": archived,
        "verified": true,
    });
    if json_output {
        emit(true, &report)?;
    } else {
        println!(
            "exported {} to the legacy layout at {} ({} messages, verified)",
            id.as_str(),
            target.display(),
            report["messages"]
        );
    }
    Ok(())
}

fn recover_to(home: &Path, session: &str, output: &str, json_output: bool) -> Result<()> {
    let id = parse_id(session)?;
    let paths = store_paths(home);
    let Some(layout) = store::reader::layout(&paths, &id)? else {
        bail!("session {} has no original to recover", id.as_str());
    };
    if layout != store::reader::SourceLayout::Canonical {
        bail!(
            "session {} uses the legacy layout; migrate it before salvaging",
            id.as_str()
        );
    }
    let destination = canonical_output_path(resolve_path(output)?)?;
    if destination.symlink_metadata().is_ok() {
        bail!(
            "recovery requires a new destination root; {} already exists",
            destination.display()
        );
    }
    let source_root = paths.root();
    if destination.starts_with(source_root) || source_root.starts_with(&destination) {
        bail!(
            "refusing to recover into {}: it would overwrite the originals under {}",
            destination.display(),
            source_root.display()
        );
    }
    let report = store::recover_to(&paths, &id, &store::SessionPaths::new(&destination))?;
    let output = json!({
        "session_id": id.as_str(),
        "output": destination.display().to_string(),
        "events": report.events.len(),
        "complete_offset": report.complete_offset,
        "torn_tail_bytes": report.torn_tail.len(),
        "damage": match &report.damage {
            Some(error) => json!({"code": code_name(error.code), "message": error.message}),
            None => Value::Null,
        },
    });
    if json_output {
        emit(true, &output)?;
    } else {
        println!(
            "recovered {} events of {} into {} (source untouched)",
            output["events"],
            id.as_str(),
            destination.display()
        );
    }
    Ok(())
}

fn archive(home: &Path, session: &str, archived: bool, json_output: bool) -> Result<()> {
    let id = parse_id(session)?;
    let paths = store_paths(home);
    let Some(layout) = store::reader::layout(&paths, &id)? else {
        bail!("session {} has no original to update", id.as_str());
    };
    if layout != store::reader::SourceLayout::Canonical {
        bail!(
            "session {} still uses the legacy layout; run `sessions migrate --apply` first",
            id.as_str()
        );
    }
    let outcome = store::archive::record(
        &paths,
        &id,
        archived,
        if archived { "cli archive" } else { "cli restore" },
    )?;
    let (status, event_id, warning) = match outcome {
        store::archive::ArchiveOutcome::Recorded {
            event_id,
            cache_warning,
        } => ("recorded", Some(event_id), cache_warning),
        store::archive::ArchiveOutcome::Unchanged { head } => ("unchanged", Some(head), None),
    };
    let report = json!({
        "session_id": id.as_str(),
        "archived": archived,
        "status": status,
        "event_id": event_id.as_ref().map(|event| event.as_str()),
        "cache_warning": warning,
    });
    if json_output {
        emit(true, &report)?;
    } else {
        println!(
            "{} {} ({status})",
            if archived { "archived" } else { "restored" },
            id.as_str()
        );
        if let Some(warning) = report["cache_warning"].as_str() {
            eprintln!("derived cache needs repair: {warning}");
        }
    }
    Ok(())
}
