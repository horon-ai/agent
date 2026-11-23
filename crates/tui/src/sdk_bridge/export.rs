//! `/export`: write the attached session's transcript as a standalone HTML
//! report.
//!
//! The document comes from the harness (`export_session_html`), so the report
//! is the redacted projection the Markdown export renders rather than the rows
//! this client drew. The destination is the path the reader typed, kept as
//! typed so a relative one resolves against the directory the client runs in.
//!
//! A file that already exists is refused. A report is written once and the
//! reader named the file by hand, so replacing one without being asked would
//! discard the only copy they may have kept.

use std::path::Path;

use horon_agent_sdk::{Error, ErrorKind, HoronAgentClient};

/// Render the session and write the report at `path`.
pub(super) fn run(client: &HoronAgentClient, session_id: &str, path: &str) -> Result<String, Error> {
    let html = client.export_session_html(session_id)?;
    write(&html, path)?;
    Ok(format!("exported the transcript to {path} (HTML, opens offline)"))
}

/// Write one already-rendered report, refusing a destination that exists.
pub(super) fn write(html: &str, path: &str) -> Result<(), Error> {
    let destination = Path::new(path);
    if destination.symlink_metadata().is_ok() {
        return Err(Error::new(
            ErrorKind::InvalidOption,
            format!("{path} already exists; name a file that does not"),
        ));
    }
    if let Some(parent) = destination.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|error| {
            Error::new(
                ErrorKind::Transport,
                format!("could not create {}: {error}", parent.display()),
            )
        })?;
    }
    std::fs::write(destination, html.as_bytes())
        .map_err(|error| Error::new(ErrorKind::Transport, format!("could not write {path}: {error}")))
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod tests;
