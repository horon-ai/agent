//! Arguments of `session cache`.

#[derive(clap::Args, Debug)]
pub(crate) struct CacheArgs {
    /// Remove the listed files. Without this option nothing is written
    #[arg(long)]
    pub(crate) apply: bool,

    /// Emit JSON instead of plain text
    #[arg(long)]
    pub(crate) json: bool,
}
