//! Options of `sessions import-transcript`.

#[derive(clap::Args, Debug)]
pub(crate) struct ImportTranscriptArgs {
    /// Transcript file (JSON lines) written by Claude Code or Codex
    pub(crate) file: String,

    /// Force the format instead of detecting it
    #[arg(long, value_parser = ["claude", "codex"])]
    pub(crate) format: Option<String>,

    /// Import the transcript. Without it the command only previews.
    #[arg(long)]
    pub(crate) apply: bool,

    #[arg(long)]
    pub(crate) json: bool,
}
