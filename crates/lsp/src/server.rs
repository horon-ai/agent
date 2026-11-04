//! The built-in language server table and workspace-root discovery.
//!
//! Four servers cover the languages this agent's users work in. Adding one is a
//! new entry in [`SERVERS`]; there is deliberately no user-facing config file,
//! because a server table that drifts from what is installed is worse than a
//! fixed one.

use std::path::{Path, PathBuf};

/// One language server this build knows how to start.
#[derive(Debug)]
pub struct ServerSpec {
    /// Short name shown in `lsp status` and error messages.
    pub name: &'static str,
    /// Executable looked up on `PATH`.
    pub program: &'static str,
    /// Arguments that make the executable speak LSP over stdio.
    pub args: &'static [&'static str],
    /// File extensions (without the dot) this server owns.
    pub extensions: &'static [&'static str],
    /// Files whose presence marks the workspace root.
    pub root_markers: &'static [&'static str],
}

/// The built-in server table, in match order.
pub static SERVERS: &[ServerSpec] = &[
    ServerSpec {
        name: "rust-analyzer",
        program: "rust-analyzer",
        args: &[],
        extensions: &["rs"],
        root_markers: &["Cargo.toml"],
    },
    ServerSpec {
        name: "typescript-language-server",
        program: "typescript-language-server",
        args: &["--stdio"],
        extensions: &["ts", "tsx", "js", "jsx", "mjs", "cjs"],
        root_markers: &["package.json", "tsconfig.json", "jsconfig.json"],
    },
    ServerSpec {
        name: "gopls",
        program: "gopls",
        args: &["serve"],
        extensions: &["go"],
        root_markers: &["go.mod", "go.work"],
    },
    ServerSpec {
        name: "pyright",
        program: "pyright-langserver",
        args: &["--stdio"],
        extensions: &["py"],
        root_markers: &["pyproject.toml", "setup.py", "pyrightconfig.json"],
    },
];

/// The server that handles `path`'s extension, if any.
pub fn server_for_path(path: &Path) -> Option<&'static ServerSpec> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    SERVERS
        .iter()
        .find(|spec| spec.extensions.contains(&extension.as_str()))
}

/// The server with this short name.
pub fn server_for_name(name: &str) -> Option<&'static ServerSpec> {
    SERVERS.iter().find(|spec| spec.name == name)
}

/// The nearest ancestor of `file` that holds one of the server's root markers,
/// falling back to the session's working directory.
///
/// A language server indexes the whole workspace it is started in, so rooting
/// it at the file's own directory would lose every cross-file answer.
pub fn workspace_root(file: &Path, spec: &ServerSpec, fallback: &Path) -> PathBuf {
    let start = file.parent().unwrap_or(file);
    for ancestor in start.ancestors() {
        if spec.root_markers.iter().any(|marker| ancestor.join(marker).exists()) {
            return ancestor.to_path_buf();
        }
    }
    fallback.to_path_buf()
}

/// Whether the server's executable can be found on `PATH`.
pub fn is_installed(spec: &ServerSpec) -> bool {
    if spec.program.contains('/') || spec.program.contains('\\') {
        return is_executable(Path::new(spec.program));
    }
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| is_executable(&dir.join(spec.program)))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(metadata) => metadata.is_file() && metadata.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
