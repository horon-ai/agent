//! Options of `sessions anonymize`.

#[derive(clap::Args, Debug)]
pub(crate) struct AnonymizeArgs {
    pub(crate) session: String,

    /// New directory to write anonymized.json into. Without it the command only previews.
    #[arg(long)]
    pub(crate) output: Option<String>,

    /// Also export every descendant session, keeping the parent and child links
    #[arg(long)]
    pub(crate) with_children: bool,

    /// Keep message text, with recognizable secrets and known paths masked
    #[arg(long)]
    pub(crate) keep_text: bool,

    /// Keep tool names instead of replacing them
    #[arg(long)]
    pub(crate) keep_tool_names: bool,

    /// Keep message and session timestamps
    #[arg(long)]
    pub(crate) keep_timestamps: bool,

    #[arg(long)]
    pub(crate) json: bool,
}
