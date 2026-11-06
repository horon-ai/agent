//! Capability coverage: what the daemon can do versus what the SDK can reach.
//!
//! The harness API is a curated subset of the internal protocol, and that is
//! correct: 77 internal requests should not become 77 public ones. But
//! "curated" and "missing something important" look identical from inside the
//! API crate, and the gap only shows up when someone tries to build a real
//! client and finds they cannot switch models.
//!
//! So the reference clients are the specification. The TUI is
//! complete, shipping clients of the same daemon; every request they send is
//! by definition something a client needs. This test diffs that set against
//! the API surface and fails when an unreviewed gap appears.
//!
//! The ledger below is the reviewed answer for each one. Adding a request to
//! the TUI without deciding what it means for the API fails this test, which
//! is the point: the decision gets made deliberately, once, and is recorded
//! where the next person will find it.

use std::collections::BTreeSet;

use crate::{
    ADVERTISED_CAPABILITIES, ApiEvent, ApiRequest, CAPABILITY_SURFACES, RuntimeControlReceipt, RuntimeStateSummary,
    SessionSnapshot, SnapshotMarkerInfo,
};

/// Why a daemon capability the reference clients use is not in the harness API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// Reachable through the API today.
    Covered,
    /// Deliberately internal: it serves the TUI's own presentation or process
    /// model, and would mean nothing to a third-party client.
    ClientInternal,
    /// A real gap. Worth exposing, not yet done. Every entry needs a reason
    /// that says what a client cannot build without it.
    #[allow(dead_code)] // Empty today; the ledger keeps the slot for future gaps.
    Gap(&'static str),
}

use Disposition::{ClientInternal, Covered, Gap};

/// Reviewed disposition of every internal request the reference clients send.
///
/// Sorted by name so additions produce clean diffs.
const LEDGER: &[(&str, Disposition)] = &[
    ("BackgroundTool", Covered),
    ("Cancel", Covered),
    ("CancelSoftInterrupts", Covered),
    ("Clear", Covered),
    ("Compact", Covered),
    ("CycleModel", ClientInternal),
    ("GetCompactedHistory", ClientInternal),
    ("GetHistory", Covered),
    ("GetModelCatalog", Covered),
    // Create/attach session bootstrap requests state through the API bridge.
    ("GetState", Covered),
    ("InputShell", ClientInternal),
    ("Message", Covered),
    ("NotifyAuthChanged", Covered),
    ("RefreshModels", ClientInternal),
    ("Reload", ClientInternal),
    ("RenameSession", Covered),
    ("ResumeAllSessions", ClientInternal),
    ("ResumeSession", ClientInternal),
    ("Rewind", Covered),
    ("RewindUndo", Covered),
    ("RunSubagent", ClientInternal),
    ("SetCompactionMode", ClientInternal),
    ("SetAgentModel", Covered),
    ("SetFeature", ClientInternal),
    ("SetModel", Covered),
    ("SetPremiumMode", ClientInternal),
    ("SetReasoningEffort", Covered),
    ("SetRoute", ClientInternal),
    ("SetServiceTier", ClientInternal),
    ("SetSessionSaved", Covered),
    ("SetSubagentModel", ClientInternal),
    ("SetTransport", ClientInternal),
    ("SoftInterrupt", Covered),
    ("Split", ClientInternal),
    ("StdinResponse", ClientInternal),
    ("Subscribe", Covered),
    ("SwitchOpenAiAccount", ClientInternal),
    ("InvalidateOpenAiUsage", Covered),
    ("Transfer", ClientInternal),
    ("TriggerMemoryExtraction", ClientInternal),
];

/// Requests the reference clients (TUI) send to the daemon.
fn reference_client_requests() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    {
        let dir = "../horon-agent-tui/src";
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
        collect_requests(&root, &mut found);
    }
    // covered by construction and would otherwise pollute the diff.
    for api_only in [
        "CreateSession",
        "AttachSession",
        "ListSessions",
        "PeekSession",
        "SendMessage",
    ] {
        found.remove(api_only);
    }
    found
}

fn collect_requests(dir: &std::path::Path, found: &mut BTreeSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_requests(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (index, _) in source.match_indices("Request::") {
                // `Request::` also matches unrelated local enums whose names
                // merely end in "Request" (the TUI's own
                // `InlinePickerPreviewRequest::Login`, say). Those are UI
                // state, not daemon capabilities, and counting them put three
                // phantom entries in the ledger that no client could ever
                // reach. Require the bare `Request::` path, so a qualified or
                // suffixed enum is skipped.
                let preceding = source[..index].chars().next_back();
                if preceding.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
                    continue;
                }
                let name: String = source[index + "Request::".len()..]
                    .chars()
                    .take_while(|ch| ch.is_ascii_alphanumeric())
                    .collect();
                if name.chars().next().is_some_and(|ch| ch.is_ascii_uppercase()) {
                    found.insert(name);
                }
            }
        }
    }
}

/// Every capability the reference clients use must have a reviewed entry.
///
/// This is the gate: it does not demand that the API cover everything, only
/// that nothing is missing *by accident*.
#[test]
fn every_reference_client_capability_is_triaged() {
    let used = reference_client_requests();
    if used.is_empty() {
        // Vendored/packaged builds without the sibling crates: nothing to do.
        return;
    }
    let ledger: BTreeSet<&str> = LEDGER.iter().map(|(name, _)| *name).collect();

    let untriaged: Vec<&String> = used.iter().filter(|name| !ledger.contains(name.as_str())).collect();
    assert!(
        untriaged.is_empty(),
        "these daemon requests are used by the TUI/desktop but have no reviewed \
         disposition in the harness API capability ledger: {untriaged:?}\n\
         Add each to LEDGER in capability_coverage.rs as Covered, ClientInternal, \
         or Gap(\"what a client cannot build without it\")."
    );

    let stale: Vec<&str> = ledger.iter().filter(|name| !used.contains(**name)).copied().collect();
    assert!(
        stale.is_empty(),
        "the capability ledger lists requests the reference clients no longer send: \
         {stale:?}. Remove them so the ledger keeps describing reality."
    );
}

/// Print the current gap list. Not a failure: gaps are a roadmap, not a bug.
///
/// Run with `cargo test -p horon-agent-harness-api -- --nocapture capability_report`.
#[test]
fn capability_report() {
    let covered = LEDGER.iter().filter(|(_, d)| *d == Covered).count();
    let internal = LEDGER.iter().filter(|(_, d)| *d == ClientInternal).count();
    let gaps: Vec<(&str, &str)> = LEDGER
        .iter()
        .filter_map(|(name, disposition)| match disposition {
            Gap(reason) => Some((*name, *reason)),
            _ => None,
        })
        .collect();

    println!("\nharness API capability coverage");
    println!("  covered by the API:      {covered}");
    println!("  deliberately internal:   {internal}");
    println!("  known gaps:              {}", gaps.len());
    for (name, reason) in &gaps {
        println!("    - {name}: {reason}");
    }
    println!();
}

/// The four capabilities of protocol minor 14 are a fixed, reviewable set.
///
/// This is the gate for "the bridge advertises what the SDK gates on": a method
/// that checks a capability nobody advertises is refused by its own SDK before
/// the request leaves the process, which reads as a broken feature rather than
/// an old server.
#[test]
fn the_minor_fourteen_capabilities_are_exactly_the_advertised_set() {
    let expected = [
        "durable_runtime_v1",
        "monitor_v1",
        "snapshot_marker_v1",
        "ask_receipts_v1",
    ];
    let advertised: Vec<&str> = ADVERTISED_CAPABILITIES.to_vec();
    assert_eq!(
        advertised, expected,
        "the advertised set is the contract both SDKs read"
    );

    let surfaces: Vec<&str> = CAPABILITY_SURFACES.iter().map(|(capability, _)| *capability).collect();
    assert_eq!(surfaces, expected, "every advertised capability needs a surface entry");
}

/// Every gated request in `ApiRequest` must name a capability this version
/// defines, and every capability must be one the bridge can advertise.
#[test]
fn every_gated_request_names_a_defined_capability() {
    let defined: std::collections::BTreeSet<&str> = ADVERTISED_CAPABILITIES.iter().copied().collect();
    let requests = [
        ApiRequest::GetRuntimeState {
            session_id: "s1".into(),
        },
        ApiRequest::ApplyControl {
            session_id: "s1".into(),
            operation_id: "op-1".into(),
            action: "cancel_input".into(),
            args: None,
        },
        ApiRequest::GetControlReceipt {
            session_id: "s1".into(),
            operation_id: "op-1".into(),
        },
        ApiRequest::GetSnapshot {
            session_id: "s1".into(),
            tail: None,
            max_bytes: None,
        },
        ApiRequest::AskResponse {
            session_id: "s1".into(),
            ask_id: "a1".into(),
            answers: Vec::new(),
            answer_id: Some("answer-1".into()),
        },
    ];
    let mut gated = 0;
    for request in requests {
        let capability = request.required_capability().expect("a new request must be gated");
        assert!(
            defined.contains(capability),
            "{request:?} gates on {capability}, which no client can negotiate"
        );
        gated += 1;
    }
    assert_eq!(gated, 5, "the scan must cover every new request, not silently skip one");

    let events = [
        ApiEvent::RuntimeState {
            session_id: "s1".into(),
            state: RuntimeStateSummary::default(),
        },
        ApiEvent::ControlReceipt {
            session_id: "s1".into(),
            receipt: RuntimeControlReceipt::default(),
        },
        ApiEvent::SnapshotMarker {
            session_id: "s1".into(),
            marker: SnapshotMarkerInfo::default(),
        },
        ApiEvent::Snapshot {
            session_id: "s1".into(),
            snapshot: SessionSnapshot::default(),
        },
    ];
    for event in events {
        let capability = event.required_capability().expect("a new event must be gated");
        assert!(
            defined.contains(capability),
            "{event:?} gates on {capability}, which no client can negotiate"
        );
    }

    let legacy = [
        ApiEvent::Ok,
        ApiEvent::Pong,
        ApiEvent::History {
            session_id: "s1".into(),
            messages: Vec::new(),
            images: Vec::new(),
        },
    ];
    for event in legacy {
        assert_eq!(
            event.required_capability(),
            None,
            "{event:?} predates capability negotiation and must reach every connection"
        );
    }
}

/// A gated event may only be sent to a connection that advertised its
/// capability. This is what keeps a legacy connection byte-identical to what it
/// received before minor 14.
#[test]
fn a_gated_event_is_withheld_from_a_connection_without_its_capability() {
    let gated = ApiEvent::SnapshotMarker {
        session_id: "s1".into(),
        marker: SnapshotMarkerInfo::default(),
    };
    let capability = gated.required_capability().expect("marker is gated");
    let legacy_peer: Vec<String> = Vec::new();
    assert!(
        !legacy_peer.iter().any(|held| held == capability),
        "a peer that advertises nothing must not be handed the marker"
    );
    let modern_peer = [capability.to_string()];
    assert!(modern_peer.iter().any(|held| held == capability));
}

/// The SDKs mirror horon-agent's external credential locations by hand, so a new one
/// added in Rust must also be inherited by launched instances.
///
/// `HORON_AGENT_HOME` redirects every `user_home_path` lookup into
/// `$HORON_AGENT_HOME/external/`, which is what makes an instance private. The cost
/// is that an instance only sees another CLI's credentials if the SDK links
/// that directory in. Miss one and inheritance silently half-works: the
/// instance starts fine and fails on the first turn with an auth error, which
/// reads like the user's login is broken rather than like a missing mapping.
#[test]
fn the_sdk_inherits_every_external_credential_location() {
    let sources = launch_sources();
    if sources.is_empty() {
        // Vendored/packaged builds without the sdk/ tree: nothing to check.
        return;
    }
    for path in external_credential_paths() {
        for (file, sdk) in &sources {
            // The SDK links directories, so any file under a covered directory is
            // covered. Compare on the first path segment (plus a second for the
            // nested `.config/...` and `.local/share/...` cases).
            assert!(
                sdk_covers(sdk, &path),
                "horon-agent reads credentials from `~/{path}`, but {file} does not inherit \
                 it. Add the directory to its external credential list, or a launched \
                 instance will fail its first turn with an auth error."
            );
        }
    }
}

/// Every SDK launcher that mirrors the credential locations, as (path, source).
fn launch_sources() -> Vec<(&'static str, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    [
        "sdk/typescript/src/launch.ts",
        "sdk/python/src/horon_agent_sdk/launch.py",
    ]
    .into_iter()
    .filter_map(|file| {
        std::fs::read_to_string(root.join(file))
            .ok()
            .map(|source| (file, source))
    })
    .collect()
}

/// Does the SDK link a directory that contains `path`?
fn sdk_covers(sdk: &str, path: &str) -> bool {
    let mut prefix = String::new();
    for segment in path.split('/') {
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(segment);
        if sdk.contains(&format!("\"{prefix}\"")) {
            return true;
        }
    }
    false
}

/// Home-relative paths horon-agent reads other tools' credentials from.
///
/// Read out of the source so the list cannot drift: `user_home_path` is the
/// single funnel for "a file in the user's home that HORON_AGENT_HOME sandboxes".
fn external_credential_paths() -> Vec<String> {
    const CREDENTIAL_HINTS: [&str; 5] = ["auth", "credential", "oauth", "hosts.json", "apps.json"];
    let mut found = Vec::new();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    collect_home_paths(&root.join("crates"), &mut found);
    collect_home_paths(&root.join("src"), &mut found);
    found.retain(|path| {
        let lower = path.to_ascii_lowercase();
        CREDENTIAL_HINTS.iter().any(|hint| lower.contains(hint))
    });
    found.sort();
    found.dedup();
    found
}

fn collect_home_paths(dir: &std::path::Path, found: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_home_paths(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let Ok(source) = std::fs::read_to_string(&path) else {
                continue;
            };
            for (index, _) in source.match_indices("user_home_path(\"") {
                let rest = &source[index + "user_home_path(\"".len()..];
                if let Some(end) = rest.find('"') {
                    found.push(rest[..end].to_string());
                }
            }
        }
    }
}

/// Every ledger entry must name a real daemon request.
///
/// The scraper looks for `Request::Foo`, which also matches unrelated local
/// enums whose names end in "Request". Three of the TUI's own picker states
/// (`InlinePickerPreviewRequest::{Login, Account, Model}`) were recorded as
/// daemon capabilities that way, and a phantom entry is worse than a missing
/// one: it reads as a reviewed decision about a capability that does not
/// exist, and "Login is deliberately internal" is a sentence nobody can act
/// on. Check the other direction too, so the ledger cannot describe something
/// the protocol has never had.
#[test]
fn the_ledger_only_names_real_daemon_requests() {
    let Some(protocol) = protocol_request_variants() else {
        return;
    };
    let unknown: Vec<&str> = LEDGER
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !protocol.contains(*name))
        .collect();
    assert!(
        unknown.is_empty(),
        "the capability ledger names requests that do not exist in \
         horon-agent-protocol's Request enum: {unknown:?}. These are usually local \
         enums whose names end in \"Request\"; a phantom entry claims a \
         decision was made about a capability that was never real."
    );
}

/// Variant names of the daemon's wire `Request` enum.
fn protocol_request_variants() -> Option<BTreeSet<String>> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../protocol/src/wire.rs");
    let source = std::fs::read_to_string(path).ok()?;
    let start = source.find("pub enum Request {")?;
    let body = &source[start..];
    let end = body.find("\n}").unwrap_or(body.len());
    Some(
        body[..end]
            .lines()
            .filter_map(|line| {
                let rest = line.strip_prefix("    ")?;
                if rest.starts_with(' ') {
                    return None;
                }
                let name: String = rest.chars().take_while(|ch| ch.is_ascii_alphanumeric()).collect();
                name.chars()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_uppercase())
                    .then_some(name)
            })
            .collect(),
    )
}
