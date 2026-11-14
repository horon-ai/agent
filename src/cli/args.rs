use clap::{Parser, Subcommand, ValueEnum};

use super::provider_init::ProviderChoice;

mod mcp;
#[allow(unused_imports)]
pub(crate) use mcp::McpCommand;

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ProviderAuthArg {
    /// Send the API key as Authorization: Bearer <key> (OpenAI-compatible default)
    Bearer,
    /// Send the API key in an API-key header (defaults to api-key)
    ApiKey,
    /// Do not send authentication, useful for localhost model servers
    None,
}

#[derive(Parser, Debug)]
#[command(name = "horon-agent")]
#[command(version = horon_agent_build_meta::version())]
#[command(about = "horon-agent: A headless coding agent for OpenAI-compatible model endpoints")]
pub(crate) struct Args {
    /// Provider profile to use: `auto` (follow [provider] default_provider or --provider-profile), `openai-compatible`, or a [providers.<name>] profile id from config.toml.
    #[arg(short, long, default_value = "auto", global = true, value_parser = super::provider_init::parse_provider_choice)]
    pub(crate) provider: ProviderChoice,

    /// Working directory for the local client process
    #[arg(short = 'C', long, global = true)]
    pub(crate) cwd: Option<String>,

    /// Working directory to send to a remote server when using --socket
    #[arg(long, global = true)]
    pub(crate) remote_working_dir: Option<String>,

    /// Remote SSH attach; unavailable in this headless build, so it fails with an explanation
    #[arg(long, global = true, conflicts_with = "socket", value_name = "HOST")]
    pub(crate) ssh: Option<String>,

    /// Remote HoronAgent executable name or literal path (requires --ssh)
    #[arg(long, global = true, requires = "ssh", value_name = "PATH")]
    pub(crate) ssh_binary: Option<String>,

    /// Remote daemon socket override, for isolated servers (requires --ssh)
    #[arg(long, global = true, requires = "ssh", value_name = "PATH")]
    pub(crate) ssh_server_socket: Option<String>,

    /// Skip the automatic update check
    #[arg(long, global = true)]
    pub(crate) no_update: bool,

    /// Auto-update when new version is available (default: true for release builds)
    #[arg(long, global = true, default_value = "true")]
    pub(crate) auto_update: bool,

    /// Log tool inputs/outputs and token usage to stderr
    #[arg(long, global = true)]
    pub(crate) trace: bool,

    /// Suppress non-error CLI/status output for scripting and wrappers
    #[arg(long, global = true)]
    pub(crate) quiet: bool,

    /// Resume a session by ID, or list sessions if no ID provided
    #[arg(long, global = true, num_args = 0..=1, default_missing_value = "")]
    pub(crate) resume: Option<String>,

    /// Deprecated: accepted so older clients keep working, and ignored.
    #[arg(long, global = true, hide = true)]
    #[allow(dead_code)]
    pub(crate) no_selfdev: bool,

    /// Custom socket path for server/client communication.
    ///
    /// Clients dial this path instead of the shared daemon socket. A headless
    /// `run` hosts its agent in-process, so nothing binds the path until a
    /// tool needs a server (today: `swarm`); the first such tool starts a
    /// temporary server here that exits with the run, leaving no daemon
    /// behind on a one-off socket.
    #[arg(long, global = true)]
    pub(crate) socket: Option<String>,

    /// Model to use (e.g., claude-opus-4-6, gpt-5.5)
    #[arg(short, long, global = true)]
    pub(crate) model: Option<String>,

    /// Named provider profile from [providers.<name>] in config.toml.
    /// Implies --provider openai-compatible for OpenAI-compatible profiles.
    #[arg(long, global = true)]
    pub(crate) provider_profile: Option<String>,

    /// Tool profile to expose to the model: core (headless default), full,
    /// minimal/lite, or none.
    #[arg(long, global = true)]
    pub(crate) tool_profile: Option<String>,

    /// Run mode for a bare launch. `rpc` speaks the stdio JSONL protocol on
    /// stdin and stdout; every other value runs the normal client.
    #[arg(long, global = true, value_name = "MODE")]
    pub(crate) mode: Option<String>,

    /// Comma-separated explicit allow-list of tools to expose, e.g. bash,read,write,apply_patch. Use '*' or 'all' for the unrestricted full toolset.
    #[arg(long, global = true)]
    pub(crate) tools: Option<String>,

    /// Comma-separated list of tools to hide after applying the selected profile.
    #[arg(long, global = true)]
    pub(crate) disabled_tools: Option<String>,

    /// Hide all built-in tools unless --tools or [tools].enabled opts tools back in.
    #[arg(long, global = true)]
    pub(crate) disable_base_tools: bool,

    /// Trust this workspace's project-local MCP config for this launch and record the
    /// decision. Without it, an untrusted workspace's project-local config is skipped.
    #[arg(long, global = true)]
    pub(crate) trust_workspace: bool,

    /// MCP tool exposure mode: auto, eager, or deferred.
    #[arg(long, global = true, value_parser = ["auto", "eager", "deferred"])]
    pub(crate) mcp_tools: Option<String>,

    /// Token estimate at which --mcp-tools=auto switches to deferred exposure.
    #[arg(long, global = true, value_name = "TOKENS")]
    pub(crate) mcp_tools_token_threshold: Option<usize>,

    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

impl Args {
    /// Turn `--mode rpc` into the `rpc` subcommand.
    ///
    /// A host that launches the binary directly passes the mode rather than a
    /// subcommand, so the flag form has to reach the same code. An explicit
    /// subcommand wins when both are present.
    pub(crate) fn promote_rpc_mode(&mut self) {
        if self.command.is_none() && self.mode.as_deref() == Some("rpc") {
            self.command = Some(Command::Rpc { session_dir: None });
        }
    }
}

#[derive(Subcommand, Debug)]
pub(crate) enum Command {
    /// List configured MCP servers or manage remote-server OAuth credentials
    Mcp {
        #[command(subcommand)]
        action: McpCommand,
    },

    /// Start the agent server (background daemon)
    Serve {
        /// Internal: mark this server as temporary so it can self-clean when its owner exits.
        #[arg(long, hide = true)]
        temporary_server: bool,

        /// Internal: owning process pid for a temporary server.
        #[arg(long, hide = true)]
        owner_pid: Option<u32>,

        /// Internal: idle shutdown timeout in seconds for a temporary server.
        #[arg(long, hide = true)]
        temp_idle_timeout_secs: Option<u64>,

        /// Stable display name for this server in connected clients and session pickers.
        ///
        /// Useful for long-lived remote runtimes, e.g. `fabian`, `john`, or
        /// `mount-cloud-fabian`. Unsafe characters are normalized before use.
        #[arg(long)]
        server_name: Option<String>,
    },

    /// Run as an Agent Client Protocol (ACP) adapter backed by the HoronAgent daemon
    Acp,

    /// Run as a stdio JSONL RPC adapter backed by the HoronAgent daemon
    Rpc {
        /// Directory the host expects session files in. Accepted for
        /// compatibility and ignored: sessions live under the Horon home.
        #[arg(long, value_name = "DIR")]
        session_dir: Option<String>,
    },

    /// Manage the background server daemon (e.g. `horon-agent server stop`).
    Server {
        #[command(subcommand)]
        action: ServerCommand,
    },

    /// Run a single message and exit
    Run {
        /// Emit a machine-readable JSON result instead of streaming text
        #[arg(long, conflicts_with = "ndjson")]
        json: bool,

        /// Emit newline-delimited JSON events while the response streams
        #[arg(long, conflicts_with = "json")]
        ndjson: bool,

        /// The message to send
        message: String,
    },
    /// Run in simple REPL mode (no TUI)
    Repl,

    /// Update horon-agent to the latest version
    Update,

    /// Verify and install a complete downloaded release from a local assets directory
    ///
    /// Installers download the release assets themselves, then hand this command
    /// the directory so verification, staging, and channel activation happen
    /// inside the release executable's own update path.
    InstallRelease {
        /// Directory holding the release assets: the agent archive and SHA256SUMS
        #[arg(long, value_name = "DIR")]
        assets: std::path::PathBuf,

        /// Agent release version the assets belong to, e.g. 0.1.0
        #[arg(long, value_name = "VERSION")]
        agent_version: String,

        /// Install this locally compiled executable instead of the published agent asset
        #[arg(long, value_name = "PATH")]
        source_binary: Option<std::path::PathBuf>,
    },

    /// Show build/version information in human or JSON form
    Version {
        /// Emit JSON instead of plain text
        #[arg(long)]
        json: bool,
    },

    /// Show usage limits for connected providers
    Usage {
        /// Emit JSON instead of plain text
        #[arg(long)]
        json: bool,
    },

    /// Inspect and settle durable pending inputs
    RuntimeState(RuntimeStateCommand),

    /// Debug socket CLI - interact with running horon-agent server
    Debug {
        /// Debug command to run (list, start, sessions, create_session, message, tool, state, history, etc.)
        #[arg(default_value = "help")]
        command: String,

        /// Optional argument for the command
        #[arg(default_value = "")]
        arg: String,

        /// Target a specific session by ID
        #[arg(short = 'S', long)]
        session: Option<String>,

        /// Connect to specific server socket path
        #[arg(short = 's', long)]
        socket: Option<String>,

        /// Wait for response to complete (for message command)
        #[arg(short, long)]
        wait: bool,
    },

    /// Provider discovery and selection helpers
    #[command(subcommand)]
    Provider(ProviderCommand),

    /// Memory management commands
    #[command(subcommand)]
    Memory(MemoryCommand),

    /// Session management commands
    #[command(subcommand, alias = "sessions")]
    Session(SessionCommand),

    /// Ambient mode management
    #[command(subcommand)]
    Ambient(AmbientCommand),

    /// SSH session migration commands
    #[command(subcommand)]
    Cloud(CloudCommand),

    /// Generate a pairing code for iOS/web client
    Pair {
        /// List paired devices instead of generating a code
        #[arg(long)]
        list: bool,

        /// Revoke a paired device by name or ID
        #[arg(long)]
        revoke: Option<String>,
    },

    /// Review and respond to pending ambient permission requests
    Permissions,

    /// Record a human verdict for an unattended run that stopped waiting on a person
    Approvals {
        #[command(subcommand)]
        action: ApprovalsCommand,
    },

    /// Model management commands
    #[command(subcommand)]
    Model(ModelCommand),
    /// Export a session transcript as Markdown
    ///
    /// Prints the transcript to stdout, or writes it to a file with `--out`.
    /// The text matches the `export_session_markdown` harness API reply:
    /// secrets are redacted and tool results over 4 KiB are truncated.
    Export {
        /// Session ID to export
        session: String,

        /// Write the transcript to this file instead of stdout
        #[arg(long)]
        out: Option<std::path::PathBuf>,

        /// Emit JSON instead of the raw transcript
        #[arg(long)]
        json: bool,
    },

    /// Serve the stable harness API on a Unix socket, for SDK clients.
    ///
    /// This is the endpoint the TypeScript SDK (`@horon-ai/agent-sdk`) connects to. It
    /// ships in the released binary on purpose: the API is only "generally
    /// available" if reaching it does not require a Rust toolchain and a
    /// source checkout.
    #[cfg(unix)]
    #[command(name = "api-bridge", alias = "api")]
    ApiBridge {
        /// Path of the API socket to listen on (default: $XDG_RUNTIME_DIR/horon-agent-api.sock)
        ///
        /// Named `--api-socket` rather than `--socket` because the global
        /// `--socket` already selects the *internal daemon* socket, and clap
        /// binds the global first: a subcommand `--socket` silently pointed
        /// both ends of the bridge at the same path.
        #[arg(long = "api-socket")]
        api_socket: Option<String>,

        /// Serve one API connection on stdin/stdout (for SSH SDK clients).
        /// Starts the shared daemon if needed; does not create an API socket.
        #[arg(long, conflicts_with = "api_socket")]
        stdio: bool,
    },
}

#[derive(clap::Args, Debug)]
pub(crate) struct RuntimeStateCommand {
    #[command(subcommand)]
    pub(crate) action: RuntimeStateAction,
}

#[derive(Subcommand, Debug)]
pub(crate) enum RuntimeStateAction {
    /// List the live inputs of a session and which ones are undetermined
    Show {
        /// Session id whose journal to read
        #[arg(long)]
        session: String,

        /// Emit JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Apply an explicit decision to an input whose outcome is unknown
    ResolveInput {
        /// Session id whose journal to settle
        #[arg(long)]
        session: String,

        /// The input id shown by `runtime-state show`
        #[arg(long)]
        input: String,

        /// What to do: retry runs the message again, cancel drops it
        #[arg(long, value_name = "ACTION")]
        action: String,
    },

    /// Hand the pending queue back to an older build, if the session is quiescent
    ExportLegacy {
        /// Session id to export
        #[arg(long)]
        session: String,

        /// Emit JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum ServerCommand {
    /// Internal native client protocol bridge over stdin/stdout (for SSH attach)
    #[cfg(unix)]
    #[command(hide = true)]
    Stdio,

    /// Start the background server if it is not already running.
    Start {
        /// Emit JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Internal: hold a lightweight connection open until stdin closes.
    #[command(hide = true)]
    Keepalive,

    /// Pin the shared server channel to an installed version.
    ///
    /// Defaults to the active `current` version. This only selects the daemon's
    /// binary; run `horon-agent server reload` separately to apply it.
    Promote {
        /// Installed version to promote (defaults to the current channel)
        version: Option<String>,

        /// Emit JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Gracefully reload the running background server onto the newest binary.
    ///
    /// This is the preferred way to pick up an upgrade: the daemon hands its
    /// live sessions off to a freshly exec'd server (the same path `/reload`
    /// uses), so headless/swarm work is preserved instead of being killed. If
    /// no server is running, this is a no-op. Use `server stop --force` only
    /// when you need to hard-retire a wedged daemon.
    Reload {
        /// Reload even if the running server is already on the newest binary.
        #[arg(long)]
        force: bool,

        /// Emit JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },

    /// Stop the running background server and clear its socket.
    ///
    /// Prefer `server reload` after an upgrade; it preserves live sessions.
    /// `stop` terminates the daemon (SIGTERM, escalating to SIGKILL), which
    /// drops any in-flight headless/swarm sessions, so it requires `--force`
    /// as a deliberate acknowledgement.
    Stop {
        /// Confirm that terminating the daemon (and dropping live sessions) is intended.
        #[arg(long)]
        force: bool,

        /// Emit JSON instead of human-readable text
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum ApprovalsCommand {
    /// Record what should happen to one tool call that a run stopped on
    Resolve {
        /// Permission request id this verdict answers
        #[arg(long, value_name = "ID")]
        request_id: String,

        /// Tool call the verdict applies to. Required: a verdict names one call.
        #[arg(long, value_name = "ID")]
        tool_call_id: String,

        /// What to do with the named call
        #[arg(long, value_enum)]
        verdict: ApprovalVerdictArg,

        /// Free-text note recorded with the verdict
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub(crate) enum ApprovalVerdictArg {
    /// Run the named call again
    Retry,
    /// Skip the named call and continue without it
    Skip,
}

#[derive(Subcommand, Debug)]
pub(crate) enum CloudCommand {
    /// Move a live session (transcript, repo state, env notes) to a cloud host
    /// and keep working there. Your local files stay editable. Git reconciles
    /// both sides when the session comes back with `horon-agent cloud return`.
    Move {
        /// Session ID or name. Defaults to the session this command runs in.
        #[arg(long)]
        session: Option<String>,
        #[command(flatten)]
        target: CloudMoveTarget,
        /// Allow moving while the session is mid-turn (the agent itself runs /cloud).
        #[arg(long)]
        allow_active: bool,
        /// Prepare and verify only. Do not hand ownership to the cloud host.
        #[arg(long)]
        dry_run: bool,
        /// After a successful move, attach this terminal to the cloud session.
        #[arg(long)]
        attach: bool,
        #[arg(long)]
        json: bool,
    },

    /// Bring a moved session back: pull the cloud transcript and merge its git
    /// work into your local checkout with a normal 3-way merge.
    Return {
        #[arg(long)]
        session: Option<String>,
        #[command(flatten)]
        target: CloudMoveTarget,
        /// Bring the work back as refs only (refs/horon-agent-cloud/<session>/*) without touching the working tree.
        #[arg(long)]
        refs_only: bool,
        /// After returning, resume the session in this terminal.
        #[arg(long)]
        attach: bool,
        #[arg(long)]
        json: bool,
    },

    /// Show where moved sessions live and how their repos have diverged.
    Where {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        json: bool,
    },

    /// Attach this terminal to a session that lives on a cloud host.
    Attach {
        #[arg(long)]
        session: Option<String>,
    },

    /// Internal: remote side of `cloud move` (reads a bundle tarball on stdin).
    #[command(hide = true)]
    Receive {
        #[arg(long)]
        json: bool,
    },

    /// Internal: remote side of `cloud move` commit phase.
    #[command(hide = true)]
    Activate {
        #[arg(long)]
        session: String,
        #[arg(long)]
        epoch: u64,
    },

    /// Internal: remote side of `cloud return` (writes a bundle tarball to stdout).
    #[command(hide = true)]
    Export {
        #[arg(long)]
        session: String,
        #[arg(long)]
        epoch: u64,
    },
}

#[derive(Parser, Debug, Clone, Default)]
pub(crate) struct CloudMoveTarget {
    /// SSH host alias for the cloud machine. Defaults to [cloud] host in config
    /// or $HORON_AGENT_CLOUD_HOST.
    #[arg(long)]
    pub(crate) host: Option<String>,
    /// Remote horon-agent binary (name or path). Defaults to `horon-agent`.
    #[arg(long)]
    pub(crate) remote_binary: Option<String>,
    /// Local command used in place of `ssh <host>` (for tests and custom
    /// transports). Receives the remote shell command as its final argument.
    /// Also read from $HORON_AGENT_CLOUD_TRANSPORT.
    #[arg(long, hide = true)]
    pub(crate) transport: Option<String>,
}

#[derive(Subcommand, Debug)]
pub(crate) enum ModelCommand {
    /// List model names you can pass to -m/--model
    List {
        /// Emit JSON instead of plain text
        #[arg(long)]
        json: bool,

        /// Show provider/selection summary before the list
        #[arg(long)]
        verbose: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum SessionCommand {
    /// Rename a saved session's human-readable name/title
    Rename {
        /// Session ID or memorable short name, e.g. fox
        session: String,

        /// New session name/title
        #[arg(required_unless_present = "clear")]
        name: Option<String>,

        /// Clear the custom session name/title
        #[arg(long, conflicts_with = "name")]
        clear: bool,

        /// Emit JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },

    /// Show one session's layout, heads, archive state and damage, read-only
    Inspect {
        /// Session ID
        session: String,

        #[arg(long)]
        json: bool,
    },

    /// Validate session originals without repairing them
    Doctor {
        /// Check this session only. Defaults to every stored session.
        session: Option<String>,

        #[arg(long)]
        json: bool,
    },

    /// Migrate legacy originals into the event store (dry run unless --apply)
    Migrate {
        /// Sessions to migrate. Defaults to every legacy original in the source HOME.
        session: Vec<String>,

        /// Migrate every legacy original in the source HOME
        #[arg(long)]
        all: bool,

        /// Legacy HOME to read. Defaults to this HOME. Originals are never modified.
        #[arg(long)]
        from: Option<String>,

        /// Perform the migration. Without this flag the command only reports the plan.
        #[arg(long, conflicts_with = "dry_run")]
        apply: bool,

        /// Report the plan only. This is the default; the flag exists so scripts can say it out loud.
        #[arg(long)]
        dry_run: bool,

        /// Also import display metadata that exists only in the old SQLite index
        #[arg(long, hide = !crate::session::event_store_api::index_import::AVAILABLE)]
        legacy_index: bool,

        #[arg(long)]
        json: bool,
    },

    /// Export one session's original events and blobs to a directory
    Export {
        session: String,

        /// New directory to write the bundle into. Must not exist.
        #[arg(long)]
        output: String,

        #[arg(long)]
        json: bool,
    },

    /// Import an exported bundle into this store
    Import {
        /// Bundle directory written by `sessions export`
        input: String,

        #[arg(long)]
        json: bool,
    },

    /// Write the current projection back to the legacy layout in another HOME
    ExportLegacy {
        session: String,

        /// HOME to write `sessions/<id>.json` into. Must differ from this HOME.
        #[arg(long)]
        output: String,

        /// Branch head to export when the session has diverged heads
        #[arg(long)]
        head: Option<String>,

        #[arg(long)]
        json: bool,
    },

    /// Salvage the verified prefix of a session into a new store root
    RecoverTo {
        session: String,

        /// New store root. Must not exist and must not overlap the source.
        #[arg(long)]
        output: String,

        #[arg(long)]
        json: bool,
    },

    /// Hide a session by recording an ArchiveChanged event
    Archive {
        session: String,

        #[arg(long)]
        json: bool,
    },

    /// Un-hide a session by recording an ArchiveChanged event
    Restore {
        session: String,

        #[arg(long)]
        json: bool,
    },

    /// Permanently delete a session and everything stored for it
    ///
    /// Unlike `archive`, nothing can be restored afterwards. The transcript,
    /// todos, the goal attachment, artifacts and the runtime journal all go.
    /// A session that is open in a running daemon is refused.
    Delete {
        /// Session ID
        session: String,

        /// Delete without asking for confirmation
        #[arg(long)]
        yes: bool,

        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum ProviderCommand {
    /// List provider IDs you can pass to -p/--provider
    List {
        /// Emit JSON instead of plain text
        #[arg(long)]
        json: bool,
    },

    /// Show the currently requested and resolved provider selection
    Current {
        /// Emit JSON instead of plain text
        #[arg(long)]
        json: bool,
    },

    /// Add a named OpenAI-compatible API provider profile
    Add {
        /// Profile name used with --provider-profile and config defaults, e.g. my-gateway
        name: String,

        /// OpenAI-compatible API base URL, e.g. https://llm.example.com/v1
        #[arg(long, alias = "api-base")]
        base_url: String,

        /// Default model id for this provider profile
        #[arg(short, long)]
        model: String,

        /// Optional model context window in tokens
        #[arg(long)]
        context_window: Option<usize>,

        /// Environment variable name that contains the API key
        #[arg(long, conflicts_with = "no_api_key")]
        api_key_env: Option<String>,

        /// API key value to store in horon-agent's private provider env file. Prefer --api-key-stdin for shell history safety.
        #[arg(long, conflicts_with_all = ["api_key_stdin", "no_api_key"])]
        api_key: Option<String>,

        /// Read the API key from stdin and store it in horon-agent's private provider env file
        #[arg(long, conflicts_with = "no_api_key")]
        api_key_stdin: bool,

        /// Configure the provider with no API key/authentication
        #[arg(long, conflicts_with_all = ["api_key", "api_key_stdin", "api_key_env"])]
        no_api_key: bool,

        /// Authentication style for the API key
        #[arg(long, value_enum)]
        auth: Option<ProviderAuthArg>,

        /// Header name when --auth api-key is used (default: api-key)
        #[arg(long)]
        auth_header: Option<String>,

        /// Private env file name under horon-agent's app config directory for stored API keys
        #[arg(long)]
        env_file: Option<String>,

        /// Make this profile the startup default provider/model
        #[arg(long, alias = "default")]
        set_default: bool,

        /// Replace an existing profile with the same name
        #[arg(long)]
        overwrite: bool,

        /// Allow provider-routing features for OpenRouter-style gateways
        #[arg(long)]
        provider_routing: bool,

        /// Fetch/list models from the provider's /models endpoint
        #[arg(long)]
        model_catalog: bool,

        /// Emit JSON instead of human-readable setup output
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug)]
pub(crate) enum AmbientCommand {
    /// Show ambient mode status
    Status,
    /// Show recent ambient activity log
    Log,
    /// Manually trigger an ambient cycle
    Trigger,
    /// Stop ambient mode
    Stop,
}

#[derive(Subcommand, Debug)]
pub(crate) enum MemoryCommand {
    /// List all stored memories
    List {
        /// Filter by scope (project, global, all)
        #[arg(short, long, default_value = "all")]
        scope: String,

        /// Filter by tag
        #[arg(short, long)]
        tag: Option<String>,
    },

    /// Search memories by query
    Search {
        /// Search query
        query: String,

        /// Use Jev relevance decisions instead of local keyword search (requires Jev access)
        #[arg(short, long)]
        semantic: bool,
    },

    /// Export memories to a JSON file
    Export {
        /// Output file path
        output: String,

        /// Export scope (project, global, all)
        #[arg(short, long, default_value = "all")]
        scope: String,
    },

    /// Import memories from a JSON file
    Import {
        /// Input file path
        input: String,

        /// Import scope (project, global)
        #[arg(short, long, default_value = "project")]
        scope: String,

        /// Overwrite existing memories with same ID
        #[arg(long)]
        overwrite: bool,
    },

    /// Show memory statistics
    Stats,

    /// Clear test memory storage (used by debug sessions)
    ClearTest,
}

#[cfg(test)]
mod tests;
