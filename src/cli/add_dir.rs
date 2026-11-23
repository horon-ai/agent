//! The `--add-dir` launch flag: extra workspace roots for the new session.
//!
//! The value is resolved once here, before a session exists, so a path that
//! names nothing fails the launch instead of reaching the model as a dead root.
//! The session stores the canonical, de-duplicated list and lists it in its
//! context; a root is workspace context, never an approval grant.

use anyhow::{Result, bail};

use super::args::{Args, Command};

/// Resolve `args.add_dir` in place, or refuse a command it cannot apply to.
pub(crate) fn canonical_add_dirs(args: &mut Args) -> Result<()> {
    if args.add_dir.is_empty() {
        return Ok(());
    }
    if !matches!(args.command, None | Some(Command::Run { .. })) {
        bail!("--add-dir is only supported when opening the terminal client or running a single message");
    }
    args.add_dir =
        crate::workspace_dirs::canonical_roots(&args.add_dir).map_err(|error| anyhow::anyhow!("--add-dir: {error}"))?;
    Ok(())
}
