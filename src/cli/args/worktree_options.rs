//! Options of the `sessions` worktree commands.

#[derive(clap::Args, Debug)]
pub(crate) struct WorktreesArgs {
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(clap::Args, Debug)]
pub(crate) struct MoveToWorktreeArgs {
    pub(crate) session: String,

    /// New branch for the worktree
    #[arg(long)]
    pub(crate) branch: Option<String>,

    /// Move the session back to its previous working directory
    #[arg(long, conflicts_with = "branch")]
    pub(crate) undo: bool,

    /// Do it. Without it the command only previews.
    #[arg(long)]
    pub(crate) apply: bool,

    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(clap::Args, Debug)]
pub(crate) struct RemoveWorktreeArgs {
    pub(crate) path: String,

    /// Do it. Without it the command only previews.
    #[arg(long)]
    pub(crate) apply: bool,

    #[arg(long)]
    pub(crate) json: bool,
}
