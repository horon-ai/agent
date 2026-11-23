//! The todo edit request and reply on the harness API wire.
//!
//! Kept out of the general schema snapshot: the request, the revision-bearing
//! state, and the edit result are one flow, and the snapshot file is already at
//! its test-size ceiling.

use crate::*;

/// A status edit carries the revision it was made against, and the reply
/// carries the current list whether or not the write landed. A stale edit is a
/// refusal, not an error frame, so a client can refresh from the same answer.
#[test]
fn todo_status_edit_and_revision_round_trip() {
    let request = ClientFrame::new(
        21,
        ApiRequest::SetTodoStatus {
            session_id: "s1".into(),
            expected_revision: "rev-a".into(),
            id: "t1".into(),
            status: "completed".into(),
        },
    );
    let wire = serde_json::to_string(&request).unwrap();
    assert_eq!(
        wire,
        r#"{"v":1,"id":21,"req":"set_todo_status","session_id":"s1","expected_revision":"rev-a","todo_id":"t1","status":"completed"}"#
    );
    assert_eq!(serde_json::from_str::<ClientFrame>(&wire).unwrap(), request);

    let state = ApiEvent::TodoState {
        session_id: "s1".into(),
        state: horon_agent_todo_policy::TodoSnapshot::default(),
        revision: Some("rev-a".into()),
    };
    let wire = serde_json::to_string(&ServerFrame::reply(1, state.clone())).unwrap();
    assert!(wire.contains(r#""revision":"rev-a""#), "{wire}");
    let legacy: ServerFrame = serde_json::from_str(
        r#"{"v":1,"id":1,"ev":"todo_state","session_id":"s1","state":{"todos":[],"plan":{},"goals":[],"gate_observations":[],"long_session_review_due":false}}"#,
    )
    .unwrap();
    assert_eq!(
        legacy.event,
        ApiEvent::TodoState {
            session_id: "s1".into(),
            state: horon_agent_todo_policy::TodoSnapshot::default(),
            revision: None,
        },
        "a server without revisions still decodes as a list with no revision"
    );

    let stale = ApiEvent::TodoEditResult {
        session_id: "s1".into(),
        applied: false,
        refusal: Some(horon_agent_todo_policy::TodoEditRefusal::StaleRevision),
        state: horon_agent_todo_policy::TodoSnapshot::default(),
        revision: "rev-b".into(),
    };
    let wire = serde_json::to_string(&ServerFrame::reply(2, stale.clone())).unwrap();
    assert!(wire.contains(r#""refusal":"stale_revision""#), "{wire}");
    assert_eq!(
        serde_json::from_str::<ServerFrame>(&wire).unwrap(),
        ServerFrame::reply(2, stale)
    );
}
