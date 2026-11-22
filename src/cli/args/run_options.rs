//! Option groups shared by the `run` command.

#[derive(clap::Args, Debug)]
pub(crate) struct RunModelOverrides {
    /// Reasoning effort for the selected model
    #[arg(long, value_name = "EFFORT")]
    pub(crate) thinking: Option<String>,

    /// OpenAI service tier (off, priority, fast, or ultrafast where supported)
    #[arg(long, value_name = "TIER")]
    pub(crate) service_tier: Option<String>,

    /// Turn the session advisor on or off for this run, overriding `[advisor] enabled`
    #[arg(long, value_name = "on|off", value_parser = ["on", "off"])]
    pub(crate) advisor: Option<String>,
}
