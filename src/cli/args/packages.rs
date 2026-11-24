//! `horon package` command line: one subcommand per catalog operation.
//!
//! Installing is the only way a package enters the catalog. The project scope
//! exists only when `--project` names a root; running the command inside a
//! project never picks that project up on its own.

use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};

use crate::packages::PackageKind;

#[derive(Subcommand, Debug)]
pub(crate) enum PackageCommand {
    /// Install one package directory into the shared catalog
    Install {
        /// Directory containing extension.toml or plugin.toml
        #[arg(value_name = "DIR")]
        directory: PathBuf,

        /// Scope the package to this canonical project root instead of the user
        #[arg(long, value_name = "DIR")]
        project: Option<PathBuf>,

        /// Replace the catalog record that already holds this package's ID
        #[arg(long)]
        replace: bool,
    },

    /// List catalog records with their kind, state, scope, version and source
    List {
        /// Only list packages of this kind
        #[arg(long, value_enum, value_name = "KIND")]
        kind: Option<PackageKindArg>,
    },

    /// Check every published copy against its recorded hashes
    Verify,

    /// Stop loading a package without removing its copy
    Disable {
        /// Package ID shown by `horon package list`
        id: String,
    },

    /// Remove a package; its ID stays reserved until an explicit replace
    Remove {
        /// Package ID shown by `horon package list`
        id: String,
    },
}

/// The package kinds a user may filter by.
#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum PackageKindArg {
    /// Native executable with its own declarations
    Extension,
    /// Extension bundled with resources such as skills, rules or commands
    Plugin,
}

impl From<PackageKindArg> for PackageKind {
    fn from(value: PackageKindArg) -> Self {
        match value {
            PackageKindArg::Extension => Self::Extension,
            PackageKindArg::Plugin => Self::Plugin,
        }
    }
}
