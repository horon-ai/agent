//! Event payloads and serde-side details shared by the daemon wire events.

use super::*;

/// One entry of the agent's plan, as a client renders it in a plan update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanEntryWire {
    pub content: String,
    /// `"high"` | `"medium"` | `"low"`.
    pub priority: String,
    /// `"pending"` | `"in_progress"` | `"completed"`.
    pub status: String,
}

/// One file change a tool call made, as a client renders it in a tool call
/// update.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiffWire {
    pub path: String,
    #[serde(default)]
    pub old_text: Option<String>,
    pub new_text: String,
    /// The tool removed this file rather than writing it.
    #[serde(default, skip_serializing_if = "is_false")]
    pub deleted: bool,
    /// Previous path when the tool moved or renamed the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    /// The tool created this file: it did not exist before the call, even when
    /// the tool read its prior text as empty.
    #[serde(default, skip_serializing_if = "is_false")]
    pub created: bool,
}

#[cfg(test)]
mod approval_failover_wire_tests {
    use super::*;

    fn encode(event: &ServerEvent) -> serde_json::Value {
        serde_json::to_value(event).expect("server event serializes")
    }

    fn assert_roundtrip(event: ServerEvent, wire_type: &str) -> serde_json::Value {
        let wire = encode(&event);
        assert_eq!(wire["type"], wire_type, "wire tag must be stable: {wire}");
        let decoded: ServerEvent = serde_json::from_value(wire.clone()).expect("server event deserializes");
        assert_eq!(encode(&decoded), wire, "round trip must preserve the payload");
        wire
    }

    #[test]
    fn approval_and_failover_events_roundtrip_with_their_wire_tags() {
        let pending = assert_roundtrip(
            ServerEvent::ApprovalPending {
                request_id: "req-1".into(),
                action: "bash".into(),
                expires_at: Some("2026-10-17T12:00:00Z".into()),
            },
            "approval_pending",
        );
        assert_eq!(pending["expires_at"], "2026-10-17T12:00:00Z");

        let resolved = assert_roundtrip(
            ServerEvent::ApprovalResolved {
                request_id: "req-1".into(),
                approved: true,
                via: "file".into(),
                resolved_by: "runner".into(),
            },
            "approval_resolved",
        );
        assert_eq!(resolved["approved"], true);
        assert_eq!(resolved["resolved_by"], "runner");

        let switched = assert_roundtrip(
            ServerEvent::FailoverSwitched {
                chain_id: "chain-1".into(),
                from: "openai".into(),
                to: "anthropic".into(),
                reason: "rate_limit".into(),
                attempt: 2,
                max: 3,
            },
            "failover_switched",
        );
        assert_eq!(switched["attempt"], 2);
        assert_eq!(switched["max"], 3);

        let needs_human = assert_roundtrip(
            ServerEvent::RunNeedsHumanResume {
                request_id: Some("req-1".into()),
                session_id: "sess-1".into(),
                reason: "unverifiable".into(),
                attempts: 2,
            },
            "run_needs_human_resume",
        );
        assert_eq!(needs_human["session_id"], "sess-1");
        assert_eq!(needs_human["attempts"], 2);

        // A stop with no permission request behind it (an exhausted failover
        // budget) must say so on the wire instead of naming a chain id that
        // resolves to nothing.
        let needs_human_without_request = assert_roundtrip(
            ServerEvent::RunNeedsHumanResume {
                request_id: None,
                session_id: "sess-1".into(),
                reason: "failover_budget_exhausted".into(),
                attempts: 3,
            },
            "run_needs_human_resume",
        );
        assert_eq!(needs_human_without_request["request_id"], serde_json::Value::Null);
    }

    #[test]
    fn approval_and_failover_acks_roundtrip_with_their_wire_tags() {
        assert_roundtrip(
            ServerEvent::ApprovalAck {
                request_id: "req-1".into(),
            },
            "approval_ack",
        );
        let ack = assert_roundtrip(
            ServerEvent::FailoverAck {
                chain_id: "chain-1".into(),
                from: "openai".into(),
                to: "anthropic".into(),
                attempt: 1,
            },
            "failover_ack",
        );
        assert_eq!(ack["chain_id"], "chain-1");
        assert_eq!(ack["attempt"], 1);
    }

    #[test]
    fn approval_pending_omits_absent_expiry_and_accepts_unknown_fields() {
        let event = ServerEvent::ApprovalPending {
            request_id: "req-1".into(),
            action: "bash".into(),
            expires_at: None,
        };
        let wire = encode(&event);
        assert!(
            wire.get("expires_at").is_none(),
            "absent expiry stays off the wire: {wire}"
        );

        // Forward compatibility: a newer peer may add fields, and an explicit
        // null expiry must read as "unknown" rather than fail the parse.
        let decoded: ServerEvent = serde_json::from_str(
            r#"{"type":"approval_pending","request_id":"req-2","action":"write","expires_at":null,"future_field":{"nested":1}}"#,
        )
        .expect("unknown fields must not break parsing");
        let ServerEvent::ApprovalPending {
            request_id,
            action,
            expires_at,
        } = decoded
        else {
            panic!("wrong event type");
        };
        assert_eq!(request_id, "req-2");
        assert_eq!(action, "write");
        assert_eq!(expires_at, None);
    }
}
