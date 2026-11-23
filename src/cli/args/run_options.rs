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

    /// Ask before file-changing calls (`write`) or every non-read call (`all`) for this run; never weakens the configured mode
    #[arg(long, value_name = "write|all", value_parser = ["write", "all"])]
    pub(crate) approval: Option<String>,

    /// Replace the system prompt for this run (text, or @file); the tool definitions still apply
    #[arg(long, value_name = "TEXT|@FILE")]
    pub(crate) system_prompt: Option<String>,

    /// Add text after the composed system prompt for this run (text, or @file; at most 16 KiB)
    #[arg(long, value_name = "TEXT|@FILE")]
    pub(crate) append_system_prompt: Option<String>,

    /// Layer a TOML file over config.toml for this run only; repeatable, later files win per key
    #[arg(long = "config", value_name = "FILE")]
    pub(crate) config_overlay: Vec<std::path::PathBuf>,
    /// With `--plan`, approve the plan the model submits instead of waiting for an answer
    #[arg(long, requires = "plan")]
    pub(crate) approve_plan: bool,
}
