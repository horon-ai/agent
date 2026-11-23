/// `volume`: the user volume lifecycle commands.
///
/// Kept in its own module so the `args.rs` diff is one variant and one type
/// declaration. Every command here acts on one `HORON_HOME` volume and needs no
/// provider, daemon or session.
#[derive(clap::Subcommand, Debug)]
pub(crate) enum VolumeCommand {
    /// Write a tar of the memory wiki (pages, metadata lines, manifest)
    Export {
        /// Path to write the tar to
        output: std::path::PathBuf,
        /// Print the report as JSON
        #[arg(long)]
        json: bool,
    },

    /// Unpack a tar from `export` into a new, empty volume
    Import {
        /// Path of the tar to import
        input: std::path::PathBuf,
        /// Print the report as JSON
        #[arg(long)]
        json: bool,
    },

    /// Checkpoint the memory store and fence further writes before a snapshot
    Quiesce {
        /// Print the report as JSON
        #[arg(long)]
        json: bool,
    },

    /// Lift the write fence so the volume accepts writes again
    Resume {
        /// Print the report as JSON
        #[arg(long)]
        json: bool,
    },

    /// Manage the user data key record and its revocation state
    #[command(subcommand)]
    Key(VolumeKeyCommand),

    /// Run (or resume) `delete_user` for this volume
    DeleteUser {
        /// The user whose volume this is
        user_id: String,
        /// The control-plane endpoint (defaults to 127.0.0.1:8080)
        #[arg(long)]
        endpoint: Option<String>,
        /// The environment variable that holds the bearer token
        #[arg(long)]
        token_env: Option<String>,
        /// A file that holds the bearer token
        #[arg(long)]
        token_file: Option<std::path::PathBuf>,
        /// Print only the record's state without running any step
        #[arg(long)]
        status: bool,
        /// Print the report as JSON
        #[arg(long)]
        json: bool,
    },

    /// Print the persisted deletion job, if there is one
    DeletionStatus {
        /// Print the report as JSON
        #[arg(long)]
        json: bool,
    },
}

/// `volume key`: create, show or revoke the user data key record.
#[derive(clap::Subcommand, Debug)]
pub(crate) enum VolumeKeyCommand {
    /// Create the key record for `key_id` (fails when one exists)
    Create {
        /// The key's identifier in the key service
        key_id: String,
        /// The algorithm id the key service named
        #[arg(long, default_value = "aes-256-gcm-envelope")]
        algorithm: String,
    },
    /// Print the key record, if there is one
    Show,
    /// Revoke the key record and wipe any staged material
    Revoke,
}
