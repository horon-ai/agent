#[derive(clap::Args, Debug)]
pub(crate) struct ModelListFilter {
    /// Filter model IDs by a case-insensitive substring
    #[arg(long, value_name = "TEXT")]
    pub(crate) search: Option<String>,
    /// Filter results by provider profile name
    #[arg(long, value_name = "PROFILE")]
    pub(crate) profile: Option<String>,
    /// Fetch current model IDs for matching OpenAI-compatible profiles
    #[arg(long)]
    pub(crate) refresh: bool,
}
