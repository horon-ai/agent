use std::path::Path;

use anyhow::Result;

use super::{Args, output, resolve_resume_arg};
use crate::session::event_store_api as store;

pub(super) fn resolve(args: &mut Args) -> Result<()> {
    if args.continue_latest {
        let cwd = std::env::current_dir()?.canonicalize()?;
        let home = crate::storage::horon_agent_dir()?;
        let home = if home.try_exists()? { home.canonicalize()? } else { home };
        let session_id = latest_workspace_session(&home, &cwd)?;
        output::stderr_info(format!("Continuing session {session_id}"));
        args.resume = Some(session_id);
        return Ok(());
    }
    // The standalone picker is the entire command. Its absence must be
    // reported before a daemon starts or an empty resume ID is resolved.
    if args.resume.as_deref() == Some("") {
        anyhow::bail!(
            "the interactive session picker is unavailable in this headless build; pass an explicit session id to --resume"
        );
    }
    resolve_resume_arg(args)
}

pub(super) fn latest_workspace_session(home: &Path, cwd: &Path) -> Result<String> {
    let paths = store::SessionPaths::new(home.join("sessions"));
    let no_session = || anyhow::anyhow!("No eligible saved session in workspace {}", cwd.display());
    if !paths.root().try_exists()? {
        return Err(no_session());
    }
    let probe = store::ReadProbe::default();
    let rows = match store::catalog::list(&paths, probe.clone()) {
        Ok(rows) => rows,
        Err(error) if error.code == store::StoreErrorCode::Busy => {
            let mut rows = Vec::new();
            for source in store::reader::sources(&paths)? {
                if source.layout != store::reader::SourceLayout::Canonical {
                    continue;
                }
                match store::catalog::row(&paths, &source.session_id, probe.clone()) {
                    Ok(row) => rows.push(row),
                    Err(error) if error.code == store::StoreErrorCode::Busy => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            rows
        }
        Err(error) => return Err(error.into()),
    };
    rows.into_iter()
        .filter(|row| !row.archived)
        .filter(|row| {
            row.working_dir.as_deref().map(Path::new).is_some_and(|path| {
                path == cwd || (path.is_absolute() && path.canonicalize().is_ok_and(|path| path == cwd))
            })
        })
        .max_by(|a, b| {
            a.last_active_at_ms
                .unwrap_or_else(|| a.updated_at.timestamp_millis())
                .cmp(&b.last_active_at_ms.unwrap_or_else(|| b.updated_at.timestamp_millis()))
                .then_with(|| b.session_id.cmp(&a.session_id))
        })
        .map(|row| row.session_id.as_str().to_owned())
        .ok_or_else(no_session)
}
