//! Arguments of `approvals grants`.

#[derive(clap::Args, Debug)]
pub(crate) struct GrantsArgs {
    #[command(subcommand)]
    pub(crate) action: GrantsAction,
}

#[derive(clap::Subcommand, Debug)]
pub(crate) enum GrantsAction {
    /// List stored scoped approval grants, read-only. Expired ones are marked
    List {
        /// Emit JSON instead of plain text
        #[arg(long)]
        json: bool,
    },

    /// Revoke grants so the next matching action asks again
    Revoke {
        /// Grant ids from `approvals grants list`
        #[arg(value_name = "ID", required_unless_present = "all", conflicts_with = "all")]
        ids: Vec<String>,

        /// Revoke every stored grant. Prints what it covers first and needs --yes
        #[arg(long)]
        all: bool,

        /// Confirm a bulk revocation
        #[arg(long, requires = "all")]
        yes: bool,
    },
}
