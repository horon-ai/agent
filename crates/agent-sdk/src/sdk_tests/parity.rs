//! Parity between the Rust, TypeScript, and Python SDKs.
//!
//! Second-order dogfooding only works if the SDKs stay the same shape. If the
//! Rust one drifts into "whatever external client happened to need", external client
//! stops telling us anything about the shipped SDKs and we are back to
//! validating them with examples written to make them look good.
//!
//! So the capability list below is the contract, and all three SDKs are checked
//! against it: a method added to one and not the others fails here. Naming is
//! the only translation (`snake_case` in Rust and Python, `camelCase` in TS);
//! semantics and arity are expected to match.
//!
//! What is deliberately *not* checked: error handling, streaming style, and
//! launch strategy. `Result` versus `throw`, channels versus `EventEmitter`,
//! and "attach to the user's horon-agent" versus "provision a private instance" are
//! places where forcing symmetry would produce an un-idiomatic SDK in one
//! language to flatter the other.

/// One capability, named in both SDKs' conventions.
struct Capability {
    /// Rust method on `HoronAgentClient`.
    rust: &'static str,
    /// TypeScript method on `HoronAgentClient`.
    ts: &'static str,
}

/// The shared surface. Adding a capability means adding it here first.
const CAPABILITIES: &[Capability] = &[
    cap("connect", "connect"),
    cap("launch", "launch"),
    cap("list_sessions", "listSessions"),
    cap("list_sessions_limited", "listSessionsLimited"),
    cap("archive_session", "archiveSession"),
    cap("restore_session", "restoreSession"),
    cap("set_retention_policy", "setRetentionPolicy"),
    cap("create_session", "createSession"),
    cap("create_session_with_options", "createSession"),
    cap("attach_session", "attachSession"),
    cap("fork_session", "forkSession"),
    cap("transfer_session", "transferSession"),
    cap("detach_session", "detachSession"),
    cap("send_message", "sendMessage"),
    cap("send_message_with_reminder", "sendMessageWithReminder"),
    cap("send_system_reminder", "sendSystemReminder"),
    cap("send_context_message", "sendContextMessage"),
    cap("list_session_heads", "listSessionHeads"),
    cap("select_session_head", "selectSessionHead"),
    cap("get_history_page", "getHistoryPage"),
    cap("export_session_events", "exportSessionEvents"),
    cap("import_session_events", "importSessionEvents"),
    cap("export_session_blob", "exportSessionBlob"),
    cap("import_session_blob", "importSessionBlob"),
    cap("session_blob_status", "sessionBlobStatus"),
    cap("export_session_record_blob", "exportSessionRecordBlob"),
    cap("import_session_record_blob", "importSessionRecordBlob"),
    cap("session_record_blob_status", "sessionRecordBlobStatus"),
    cap("cancel", "cancel"),
    cap("soft_interrupt", "softInterrupt"),
    cap("soft_interrupt_with_images", "softInterruptWithImages"),
    cap("get_history", "getHistory"),
    cap("get_history_with_images", "getHistoryWithImages"),
    cap("peek_session", "peekSession"),
    cap("clear", "clear"),
    cap("rewind", "rewind"),
    cap("rewind_undo", "rewindUndo"),
    cap("respond_to_permission", "respondToPermission"),
    cap("respond_to_ask", "respondToAsk"),
    cap("respond_to_ask_with_id", "respondToAskWithId"),
    cap("get_runtime_state", "getRuntimeState"),
    cap("apply_control", "applyControl"),
    cap("get_control_receipt", "getControlReceipt"),
    cap("get_snapshot", "getSnapshot"),
    cap("list_models", "listModels"),
    cap("get_runtime_info", "getRuntimeInfo"),
    cap("runtime_command", "runtimeCommand"),
    cap("set_api_key", "setApiKey"),
    cap("clear_api_key", "clearApiKey"),
    cap("notify_auth_changed", "notifyAuthChanged"),
    cap("invalidate_usage", "invalidateUsage"),
    cap("read_file", "readFile"),
    cap("find_files", "findFiles"),
    cap("search_text", "searchText"),
    cap("file_status", "fileStatus"),
    cap("todo_state", "todoState"),
    cap("ack_todo_follow_up", "ackTodoFollowUp"),
    cap("set_model", "setModel"),
    cap("configure_tools", "configureTools"),
    cap("list_tools", "listTools"),
    cap("submit_tool_result", "submitToolResult"),
    cap("set_reasoning_effort", "setReasoningEffort"),
    cap("compact", "compact"),
    cap("rename_session", "renameSession"),
    cap("set_session_saved", "setSessionSaved"),
    cap("cancel_soft_interrupts", "cancelSoftInterrupts"),
    cap("cancel_soft_interrupts_by_id", "cancelSoftInterruptsById"),
    cap("cancel_soft_interrupt", "cancelSoftInterrupt"),
    cap("soft_interrupt_with_id", "softInterruptWithId"),
    cap("background_tool", "backgroundTool"),
    cap("background_tool_result", "backgroundToolResult"),
    cap("ping", "ping"),
    cap("run", "run"),
    cap("run_structured", "runStructured"),
    cap("events", "events"),
    cap("global_events", "globalEvents"),
    cap("request", "request"),
    cap("notify", "notify"),
    cap("supports", "supports"),
];

const fn cap(rust: &'static str, ts: &'static str) -> Capability {
    Capability { rust, ts }
}

/// Every shared capability exists in the Rust SDK.
#[test]
fn the_rust_sdk_implements_every_shared_capability() {
    let methods = rust_public_methods();
    let missing: Vec<&str> = CAPABILITIES
        .iter()
        .map(|c| c.rust)
        .filter(|name| !methods.iter().any(|method| method == name))
        .collect();
    assert!(
        missing.is_empty(),
        "the shared SDK surface names capabilities the Rust SDK does not have: \
         {missing:?}. Implement them in client.rs, or remove them from \
         CAPABILITIES if the capability is being dropped from both SDKs."
    );
}

/// Every shared capability exists in the TypeScript SDK.
#[test]
fn the_typescript_sdk_implements_every_shared_capability() {
    let Some(methods) = ts_public_methods() else {
        // Vendored builds without the sdk/ tree: nothing to compare against.
        return;
    };
    let missing: Vec<&str> = CAPABILITIES
        .iter()
        .map(|c| c.ts)
        .filter(|name| !methods.iter().any(|method| method == name))
        .collect();
    assert!(
        missing.is_empty(),
        "the shared SDK surface names capabilities the TypeScript SDK does not \
         have: {missing:?}. A capability that exists only in Rust means \
         external client is exercising a design the shipped SDK does not have, which \
         is the drift this test exists to prevent."
    );
}

/// Every shared capability exists in the Python SDK.
#[test]
fn the_python_sdk_implements_every_shared_capability() {
    let Some(methods) = python_public_methods() else {
        // Vendored builds without the sdk/ tree: nothing to compare against.
        return;
    };
    let missing: Vec<&str> = CAPABILITIES
        .iter()
        .map(|c| c.rust)
        .filter(|name| !methods.iter().any(|method| method == name))
        .collect();
    assert!(
        missing.is_empty(),
        "the shared SDK surface names capabilities the Python SDK does not \
         have: {missing:?}. Implement them in sdk/python/src/horon_agent_sdk/client.py, \
         or remove them from CAPABILITIES if the capability is being dropped from every SDK."
    );
}

/// No SDK has a public capability that is missing from the shared list.
///
/// The direction that actually rots: someone adds a method to the Rust SDK for
/// external client, never touches the TS SDK, and the lists silently diverge. Failing
/// here forces the decision to be made rather than deferred.
#[test]
fn neither_sdk_has_an_untriaged_public_capability() {
    let rust = rust_public_methods();
    let known: std::collections::BTreeSet<&str> = CAPABILITIES.iter().map(|c| c.rust).collect();
    let untriaged: Vec<&String> = rust
        .iter()
        .filter(|name| !known.contains(name.as_str()))
        .filter(|name| !RUST_ONLY.contains(&name.as_str()))
        .collect();
    assert!(
        untriaged.is_empty(),
        "these public HoronAgentClient methods are in the Rust SDK but not in the \
         shared capability list: {untriaged:?}. Add each to CAPABILITIES with \
         its TypeScript counterpart, or to RUST_ONLY with a comment saying why \
         it is Rust-specific."
    );

    let Some(ts) = ts_public_methods() else {
        return;
    };
    let known: std::collections::BTreeSet<&str> = CAPABILITIES.iter().map(|c| c.ts).collect();
    let untriaged: Vec<&String> = ts
        .iter()
        .filter(|name| !known.contains(name.as_str()))
        .filter(|name| !TS_ONLY.iter().any(|(allowed, _)| allowed == &name.as_str()))
        .collect();
    assert!(
        untriaged.is_empty(),
        "these public HoronAgentClient methods are in the TypeScript SDK but not in the \
         shared capability list: {untriaged:?}. Add each to CAPABILITIES and the \
         Rust SDK, or to TS_ONLY with a reason when it is genuinely language-specific."
    );

    if let Some(python) = python_public_methods() {
        let known: std::collections::BTreeSet<&str> = CAPABILITIES.iter().map(|c| c.rust).collect();
        let untriaged: Vec<&String> = python
            .iter()
            .filter(|name| !known.contains(name.as_str()))
            .filter(|name| !PY_ONLY.iter().any(|(allowed, _)| allowed == &name.as_str()))
            .collect();
        assert!(
            untriaged.is_empty(),
            "these public HoronAgentClient methods are in the Python SDK but not in the \
             shared capability list: {untriaged:?}. Add each to CAPABILITIES and the other \
             SDKs, or to PY_ONLY with a reason when it is genuinely language-specific."
        );
        if !PY_ONLY.is_empty() {
            eprintln!(
                "warning: SDK parity has {} explicitly triaged Python-only methods: {}",
                PY_ONLY.len(),
                PY_ONLY.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")
            );
        }
    }

    if !TS_ONLY.is_empty() {
        eprintln!(
            "warning: SDK parity has {} explicitly triaged TypeScript-only methods: {}",
            TS_ONLY.len(),
            TS_ONLY.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(", ")
        );
    }
}

/// Rust-specific members, with the reason each one is not mirrored.
const RUST_ONLY: &[&str] = &[
    // Rust's native process transport/launch strategy. TypeScript accepts a
    // caller-supplied transport; a built-in SSH launcher is not yet mirrored.
    "connect_ssh",
    // Rust-only shared OpenSSH ownership lease for independent reconnects.
    "shared_ssh_transport",
    // `connect_with` is the explicit transport seam Rust tests use; TypeScript
    // accepts its transport through the options passed to `connect`.
    "connect_with",
    // `Drop` closes the connection in Rust, so there is no `close()` to mirror.
    "is_closed",
    // TypeScript passes the same budgets as an optional second argument to
    // `events()`; Rust has no optional struct argument without a new exported
    // type, so the budgets are a second constructor.
    "events_with_limits",
    // Rust streams are an explicit subscription list, so a leaked stream is
    // observable there. TypeScript uses an `EventEmitter`, where the same check
    // is `client.listenerCount("event")` and needs no method.
    "subscriber_count",
    // TypeScript's `next()` rejects its promise with the overflow error, so a
    // caller cannot miss it. Rust's `next()` returns `Option` and reports the
    // overflow as `None`, which is indistinguishable from a clean end, so this
    // non-taking accessor is what makes the loss visible to an iterator caller.
    "overflow_error",
    // TS reads `client.socketPath` as a field; Rust exposes it as an accessor.
    "socket_path",
];

/// Public TypeScript methods not yet represented by an equivalent Rust method.
///
/// This is intentionally noisy technical debt, not a second capability list.
/// New TS methods fail the parity test unless they are implemented in Rust or
/// added here with a reviewable reason. Removing entries is the parity roadmap.
const TS_ONLY: &[(&str, &str)] = &[
    ("close", "Rust closes through Drop"),
    (
        "snapshotWithBufferedEvents",
        "TypeScript subscribes through an EventEmitter, so an event that races the snapshot reply \
         is lost unless it is buffered before the request is awaited. Rust's events() hands the \
         caller an owned EventStream it keeps across the get_snapshot call, so the same window \
         cannot open and no helper is needed",
    ),
];

/// Public Python methods not represented by an equivalent Rust method.
///
/// Same rule as `TS_ONLY`: noisy technical debt, not a second capability list.
/// A new Python method fails the parity test unless it also exists in Rust, or
/// is added here with a reviewable reason.
const PY_ONLY: &[(&str, &str)] = &[
    (
        "close",
        "Rust closes through Drop. Python has no deterministic destructor, and a launched instance \
         must be stopped explicitly, so close() is an awaited method",
    ),
    (
        "instance_home",
        "A property exposing the launched instance's state directory. TypeScript exposes the same \
         value as a getter, which the TypeScript parser does not count as a method",
    ),
    (
        "snapshot_with_buffered_events",
        "Same helper as TypeScript's snapshotWithBufferedEvents: an event that races the snapshot \
         reply is lost unless it is buffered before the request resolves. Rust's events() hands the \
         caller an owned stream it keeps across get_snapshot, so the window does not open there",
    ),
];

fn rust_client_source() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    ["client.rs", "structured.rs"]
        .into_iter()
        .map(|file| {
            std::fs::read_to_string(root.join(file))
                .unwrap_or_else(|error| panic!("the Rust SDK source {file} must be readable: {error}"))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn ts_client_source() -> Option<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/typescript/src/client.ts");
    std::fs::read_to_string(path).ok()
}

fn python_client_source() -> Option<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sdk/python/src/horon_agent_sdk/client.py");
    std::fs::read_to_string(path).ok()
}

/// Public method names in `impl HoronAgentClient`.
fn rust_public_methods() -> Vec<String> {
    let source = rust_client_source();
    let mut methods = std::collections::BTreeSet::new();
    let mut remaining = source.as_str();
    while let Some(start) = remaining.find("impl HoronAgentClient {") {
        let body = &remaining[start..];
        let end = body.find("\n}").unwrap_or(body.len());
        methods.extend(body[..end].lines().filter_map(|line| {
            let rest = line.trim().strip_prefix("pub fn ")?;
            Some(
                rest.chars()
                    .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
                    .collect::<String>(),
            )
        }));
        remaining = &body[end.min(body.len())..];
    }
    methods.into_iter().collect()
}

/// Public method names in the TypeScript `HoronAgentClient` class.
///
/// Methods are two-space-indented in this file. Private helpers are explicitly
/// excluded and overloads are deduplicated. Keeping this small parser here makes
/// the guard run under ordinary `cargo test`, without requiring Node or a TS AST.
fn ts_public_methods() -> Option<Vec<String>> {
    let source = ts_client_source()?;
    let start = source.find("export class HoronAgentClient")?;
    let mut methods = std::collections::BTreeSet::new();
    for line in source[start..].lines() {
        let Some(mut declaration) = line.strip_prefix("  ") else {
            continue;
        };
        if declaration.starts_with(' ') || declaration.starts_with("private ") {
            continue;
        }
        declaration = declaration.strip_prefix("static ").unwrap_or(declaration);
        declaration = declaration.strip_prefix("async ").unwrap_or(declaration);
        let Some(open) = declaration.find('(') else {
            continue;
        };
        let name = declaration[..open].split('<').next().unwrap_or_default();
        if !name.is_empty()
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
        {
            methods.insert(name.to_string());
        }
    }
    Some(methods.into_iter().collect())
}

/// Public method names in the Python `HoronAgentClient` class.
///
/// Methods are four-space-indented `def`/`async def` declarations. Nested
/// helpers live deeper and are skipped, as are `_`-prefixed names (Python's
/// privacy convention), so the comparison sees the same curated surface the
/// Rust and TypeScript parsers do. Keeping the parser here makes the guard run
/// under ordinary `cargo test`, without requiring Python.
fn python_public_methods() -> Option<Vec<String>> {
    let source = python_client_source()?;
    let start = source.find("class HoronAgentClient:")?;
    let mut methods = std::collections::BTreeSet::new();
    for line in source[start..].lines().skip(1) {
        if !line.starts_with(' ') && !line.trim().is_empty() {
            // The next top-level item ends the class body.
            break;
        }
        let Some(body) = line.strip_prefix("    ") else {
            continue;
        };
        if body.starts_with(' ') || body.trim().is_empty() {
            continue;
        }
        let declaration = body.strip_prefix("async ").unwrap_or(body);
        let Some(declaration) = declaration.strip_prefix("def ") else {
            continue;
        };
        let name: String = declaration
            .chars()
            .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
            .collect();
        if !name.is_empty() && !name.starts_with('_') {
            methods.insert(name);
        }
    }
    Some(methods.into_iter().collect())
}
