//! Arguments for `horon-agent api-credentials`.

use clap::{Args, Subcommand};

/// Manage the bearer tokens a harness API exposed over TCP accepts.
#[derive(Args, Debug)]
pub(crate) struct ApiCredentialsArgs {
    #[command(subcommand)]
    pub(crate) action: ApiCredentialsAction,
}

#[derive(Subcommand, Debug)]
pub(crate) enum ApiCredentialsAction {
    /// Issue one credential and print its token once
    Issue {
        /// Human-readable name stored with the credential
        #[arg(long, value_name = "NAME")]
        label: Option<String>,
    },

    /// Replace the token of an existing credential (by id or label)
    Rotate {
        /// Credential id or label
        #[arg(value_name = "ID")]
        credential: String,
    },

    /// Delete a credential (by id or label)
    Revoke {
        /// Credential id or label
        #[arg(value_name = "ID")]
        credential: String,
    },

    /// List credentials without ever printing a token
    List,
}
