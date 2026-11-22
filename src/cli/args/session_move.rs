/// `sessions move`: the session, the new root and whether to apply it.
#[derive(clap::Args, Debug)]
pub(crate) struct SessionMoveArgs {
    /// Session ID or memorable short name
    pub(crate) session: String,

    /// The new working root; it must be an existing directory
    pub(crate) to: std::path::PathBuf,

    /// Change the stored working root instead of only previewing it
    #[arg(long)]
    pub(crate) apply: bool,

    #[arg(long)]
    pub(crate) json: bool,
}
