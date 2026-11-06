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
#[command(about = "horon-agent: A coding agent using Claude Max or ChatGPT Pro subscriptions")]
pub(crate) struct Args {
    /// Initial provider to use (horon-agent, claude, openai, openai-api, openrouter, azure, opencode, opencode-go, zai, 302ai, baseten, conifer, cortecs, comtegra, deepseek, fpt, firmware, huggingface, moonshotai, nebius, scaleway, stackit, groq, mistral, perplexity, togetherai, deepinfra, xai, grok-build, nvidia-nim, lmstudio, ollama, chutes, cerebras, alibaba-coding-plan, openai-compatible, cursor, copilot, gemini, antigravity, or auto-detect). Interactive sessions can switch providers with /model.
    #[arg(short, long, default_value = "auto", global = true)]
    pub(crate) provider: ProviderChoice,

    /// Working directory for the local client process
    #[arg(short = 'C', long, global = true)]
    pub(crate) cwd: Option<String>,

    /// Working directory to send to a remote server when using --socket
    #[arg(long, global = true)]
    pub(crate) remote_working_dir: Option<String>,

    /// Run the UI locally and attach to the persistent HoronAgent server on this SSH host
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

    /// Login to a provider via OAuth, API key, or local credentials
    Login {
        /// Provider to log in to. Equivalent to --provider for this command, e.g. `horon-agent login openai`.
        // Distinct clap id: the global `--provider` flag also has id "provider";
        // sharing the id makes clap drop the flag inside `login` (so
        // `horon-agent login --provider x` errors) and propagate the global default
        // into this positional.
        #[arg(value_enum, id = "login_provider", value_name = "PROVIDER")]
        provider: Option<ProviderChoice>,

        /// Account label for multi-account support (stored labels are auto-numbered)
        #[arg(long, short = 'a')]
        account: Option<String>,

        /// Do not open a local browser. Show a login QR for another device (useful over SSH).
        #[arg(long, alias = "headless")]
        no_browser: bool,

        /// Print a script-friendly auth URL and persist temporary login state for later completion.
        #[arg(long, conflicts_with_all = ["callback_url", "auth_code"])]
        print_auth_url: bool,

        /// Complete a previously printed auth flow using a full callback URL or query string.
        #[arg(long, conflicts_with = "auth_code")]
        callback_url: Option<String>,

        /// Complete a previously printed auth flow using a provider-issued authorization code.
        #[arg(long, conflicts_with = "callback_url")]
        auth_code: Option<String>,

        /// Emit machine-readable JSON for script-friendly login flows.
        #[arg(long)]
        json: bool,

        /// Resume a pending scriptable login flow that does not require callback/code input.
        #[arg(long, conflicts_with_all = ["print_auth_url", "callback_url", "auth_code"])]
        complete: bool,

        /// Isolate temporary login state using 1-64 ASCII letters, digits, underscores or hyphens.
        #[arg(long, value_parser = super::login::parse_login_flow_id)]
        flow_id: Option<String>,

        /// Cancel only this provider's pending flow. Does not remove saved credentials.
        #[arg(long, requires = "flow_id", conflicts_with_all = ["print_auth_url", "callback_url", "auth_code", "complete", "account", "api_base", "api_key", "api_key_env"])]
        cancel: bool,

        /// Save credentials without running the post-login live provider validation.
        /// Useful for offline setup, CI, or when entering credentials before network access is available.
        #[arg(long)]
        no_validate: bool,

        /// OpenAI-compatible API base URL. Used with --provider openai-compatible/custom profiles.
        #[arg(long)]
        api_base: Option<String>,

        /// OpenAI-compatible API key. If omitted, horon-agent prompts securely when needed.
        #[arg(long)]
        api_key: Option<String>,

        /// Environment variable name to store/use for an OpenAI-compatible API key.
        #[arg(long)]
        api_key_env: Option<String>,
    },

    /// Log in to and manage your HoronAgent account
    Account {
        #[command(subcommand)]
        action: AccountCommand,
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

        /// Agent release version the assets belong to, e.g. 0.98.1
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

    /// Authentication status and validation helpers
    #[command(subcommand)]
    Auth(AuthCommand),

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

    /// Show live verification coverage. With no provider/model, prints the full coverage summary.
    #[command(name = "provider-test-coverage", alias = "model-status")]
    ProviderTestCoverage {
        /// Provider to look up. Omit provider and model to print the full coverage summary.
        #[arg(value_name = "PROVIDER")]
        provider_query: Option<String>,

        /// Model to look up. Defaults to the global --model value only when PROVIDER is supplied.
        #[arg(value_name = "MODEL")]
        model_query: Option<String>,

        /// Read coverage from this JSON file instead of the default live-test coverage ledger
        #[arg(long)]
        coverage_file: Option<String>,

        /// Maximum provider/model pairs to list in the full summary (0 = show all)
        #[arg(long, default_value_t = 0)]
        coverage_limit: usize,
    },

    /// Diagnose why a provider/model or the model picker is broken by walking the
    /// strict end-to-end checkpoints (catalog, picker, model-switch, chat, streaming, tools).
    #[command(name = "provider-doctor", alias = "provider-strict-e2e")]
    ProviderDoctor {
        /// OpenAI-compatible provider id to diagnose (e.g. cerebras, fpt, nvidia-nim)
        #[arg(id = "doctor_provider", value_name = "PROVIDER")]
        provider: String,

        /// How much to exercise: offline (no key/no spend), catalog (key, ~no spend),
        /// or full (key, spends balance: chat + streaming + tools).
        #[arg(long, value_name = "TIER", default_value = "catalog")]
        tier: String,

        /// Emit the report as JSON for scripting
        #[arg(long)]
        json: bool,
    },

    /// Test authentication end-to-end: login (optional), credential probe, refresh, and provider smoke
    AuthTest {
        /// Run the provider login flow before validation (interactive/browser-based)
        #[arg(long)]
        login: bool,

        /// Test all currently configured supported auth providers instead of just --provider
        #[arg(long)]
        all_configured: bool,

        /// Skip the provider runtime smoke prompt
        #[arg(long)]
        no_smoke: bool,

        /// Skip the tool-enabled runtime smoke prompt (the same request path used during normal chat)
        #[arg(long)]
        no_tool_smoke: bool,

        /// Custom smoke prompt (default asks for AUTH_TEST_OK)
        #[arg(long)]
        prompt: Option<String>,

        /// Emit JSON report instead of human-readable output
        #[arg(long)]
        json: bool,

        /// Write the full auth-test report JSON to a file
        #[arg(long)]
        output: Option<String>,

        /// Show strict live provider/model E2E coverage instead of running auth tests
        #[arg(long, conflicts_with_all = ["login", "all_configured", "no_smoke", "no_tool_smoke", "prompt"])]
        coverage: bool,

        /// Fetch live model catalogs and verify context-window resolution for each model with metadata
        #[arg(long, conflicts_with_all = ["login", "no_smoke", "no_tool_smoke", "prompt", "coverage"])]
        context_audit: bool,

        /// Read coverage from this JSON file instead of the default live-test coverage ledger
        #[arg(long, requires = "coverage")]
        coverage_file: Option<String>,

        /// Maximum uncovered provider/model gaps to show in the text coverage report
        #[arg(long, requires = "coverage", default_value_t = 50)]
        coverage_limit: usize,
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
pub(crate) enum AccountCommand {
    /// Open browser-based device authorization and wait for plan activation
    Login {
        /// Do not open a browser automatically; print the public approval URL instead
        #[arg(long, alias = "headless")]
        no_browser: bool,
    },
    /// Show canonical account, plan, and usage status from /v1/me
    Status {
        /// Emit JSON instead of human-readable output
        #[arg(long)]
        json: bool,
    },
    /// Open the public HoronAgent account management page
    Manage,
    /// Revoke the current key when reachable, then securely clear local state
    Logout,
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

        /// Also import display metadata that exists only in the legacy SQLite index
        #[arg(long)]
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
pub(crate) enum AuthCommand {
    /// Import one selected OAuth login from a trusted client (never overwrites a store)
    Import {
        /// Read the private credential envelope from stdin, never from command arguments
        #[arg(long, required = true)]
        stdin: bool,

        /// Emit a secret-free JSON acknowledgement
        #[arg(long)]
        json: bool,
    },
    /// Show configured authentication status for model/tool providers
    Status {
        /// Emit JSON instead of plain text
        #[arg(long)]
        json: bool,
    },
    /// Diagnose provider auth issues and suggest next steps
    Doctor {
        /// Optional provider id or alias to focus diagnosis on one provider
        #[arg(id = "auth_provider", value_name = "PROVIDER")]
        provider: Option<String>,

        /// Run live post-login validation for configured providers during diagnosis
        #[arg(long)]
        validate: bool,

        /// Emit JSON instead of plain text
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
