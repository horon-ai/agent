//! `horon://` reads: the documentation bundled into this build.
//!
//! `horon://` lists the bundled documents and `horon://<path>` returns one,
//! with the same selectors as a file read (`horon://docs/runtime/TOOLS.md:40-80`).
//! The path is the one `horon` search and list print; `docs/`, `.md` and
//! letter case may be left out (`horon://runtime/tools`).

use super::{ReadInput, page_text};
use crate::tool::ToolOutput;
use crate::tool::horon_agent_docs;
use crate::tool::read_selector::Selector;
use anyhow::Result;

pub(super) fn read_horon_url(params: &ReadInput) -> Result<ToolOutput> {
    let (uri, selector) = crate::tool::read_selector::split_uri_and_selector(&params.file_path)?;
    let reference = uri.get(horon_agent_docs::SCHEME.len()..).unwrap_or("");
    let Some((path, body)) = horon_agent_docs::document(reference)? else {
        return Ok(ToolOutput::new(horon_agent_docs::list_docs()).with_title(params.file_path.clone()));
    };
    // A document is read to be understood, not edited: whole and without line
    // numbers unless a selector or range asks for a part.
    let explicit_range =
        params.start_line.is_some() || params.end_line.is_some() || params.offset.is_some() || params.limit.is_some();
    let selector = if matches!(selector, Selector::None) && !explicit_range {
        Selector::Raw
    } else {
        selector
    };
    let page = page_text(params, &selector, body)?;
    Ok(ToolOutput::new(format!(
        "Source: horon://{path} (bundled with this Horon build)\n\n{page}"
    ))
    .with_title(params.file_path.clone()))
}
