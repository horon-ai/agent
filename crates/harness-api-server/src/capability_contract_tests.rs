use super::*;

/// The session event store's contract (plan D12): protocol minor 12
/// advertises exactly these capabilities, and a client may check any of
/// them before sending a request.
#[test]
fn bridge_advertises_the_session_event_capabilities() {
    for capability in [
        "session_event_store_v1",
        "session_replication_v1",
        "session_history_cursor_v1",
        "structured_compaction_v1",
    ] {
        assert!(
            BRIDGE_CAPABILITIES.contains(&capability),
            "the bridge must advertise {capability}; a client that checks it \
             before sending would otherwise be refused by its own SDK"
        );
    }
}

/// Every capability an SDK method gates on must be advertised here.
///
/// This reads the reference clients' own source, so a method added on one
/// side without the capability on the other fails here instead of at a
/// user's first call.
#[test]
fn every_sdk_capability_gate_is_advertised() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut checked = 0;
    for (path, marker) in [
        (root.join("crates/agent-sdk/src/client.rs"), "require_capability(\""),
        (root.join("sdk/typescript/src/client.ts"), "requireCapability(\""),
    ] {
        let source = std::fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "cannot read {}: {error}. A missing SDK would make this gate pass by \
                 checking nothing, which is the failure it exists to catch",
                path.display()
            )
        });
        let mut gates_here = 0;
        for (index, _) in source.match_indices(marker) {
            let rest = &source[index + marker.len()..];
            let Some(end) = rest.find('"') else { continue };
            let capability = &rest[..end];
            checked += 1;
            gates_here += 1;
            assert!(
                BRIDGE_CAPABILITIES.contains(&capability),
                "{} gates on {capability}, which the bridge never advertises",
                path.display()
            );
        }
        assert!(
            gates_here > 0,
            "{} contains no `{marker}` gate; the scan would silently check nothing",
            path.display()
        );
    }
    assert!(checked > 0, "the capability gate scan found no gates to check");
}
