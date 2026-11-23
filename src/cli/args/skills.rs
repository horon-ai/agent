use clap::Subcommand;

#[derive(Subcommand, Debug)]
pub(crate) enum SkillsCommand {
    /// List skills with the source and commit of each installed one
    List {
        /// Emit JSON instead of plain text
        #[arg(long)]
        json: bool,
    },

    /// Install a skill into the managed skills directory
    ///
    /// The source is an https, ssh or file URL, a user@host:path remote, or a
    /// local directory. Any other source is refused before anything is written.
    /// No install script runs. Needs `[tools] managed_skills = true`.
    Install {
        /// Where the skill comes from
        source: String,

        /// Branch, tag or commit to install
        #[arg(long = "ref", value_name = "REF")]
        git_ref: Option<String>,

        /// Skill directory to take when the source holds more than one
        #[arg(long)]
        name: Option<String>,

        /// Confirm that you reviewed the source and want it installed
        #[arg(long)]
        approve: bool,
    },
}
