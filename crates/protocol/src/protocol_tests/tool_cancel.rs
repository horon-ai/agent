// Requests that cancel one tool call or one named turn (plan row 5), and the
// two background-work requests the same wave introduces.

#[test]
fn cancel_turn_round_trips_and_reports_id() -> Result<()> {
    let request = Request::CancelTurn {
        id: 7,
        run_id: "run-abc".to_string(),
    };
    let json = serde_json::to_string(&request)?;
    assert_eq!(json, r#"{"type":"cancel_turn","id":7,"run_id":"run-abc"}"#);
    assert_eq!(request.id(), 7);

    let decoded = parse_request_json(&json)?;
    assert_eq!(decoded.id(), 7);
    let Request::CancelTurn { id, run_id } = decoded else {
        return Err(anyhow!("expected CancelTurn"));
    };
    assert_eq!(id, 7);
    assert_eq!(run_id, "run-abc");
    Ok(())
}

#[test]
fn cancel_tool_call_round_trips_and_reports_id() -> Result<()> {
    let request = Request::CancelToolCall {
        id: 8,
        call_id: "call-1".to_string(),
    };
    let json = serde_json::to_string(&request)?;
    assert_eq!(json, r#"{"type":"cancel_tool_call","id":8,"call_id":"call-1"}"#);
    assert_eq!(request.id(), 8);

    let decoded = parse_request_json(&json)?;
    let Request::CancelToolCall { id, call_id } = decoded else {
        return Err(anyhow!("expected CancelToolCall"));
    };
    assert_eq!(id, 8);
    assert_eq!(call_id, "call-1");
    Ok(())
}

#[test]
fn cancel_background_terminal_round_trips_and_reports_id() -> Result<()> {
    let request = Request::CancelBackgroundTerminal {
        id: 9,
        call_id: "call-2".to_string(),
    };
    let json = serde_json::to_string(&request)?;
    assert_eq!(
        json,
        r#"{"type":"cancel_background_terminal","id":9,"call_id":"call-2"}"#
    );
    assert_eq!(request.id(), 9);

    let decoded = parse_request_json(&json)?;
    let Request::CancelBackgroundTerminal { id, call_id } = decoded else {
        return Err(anyhow!("expected CancelBackgroundTerminal"));
    };
    assert_eq!(id, 9);
    assert_eq!(call_id, "call-2");
    Ok(())
}

#[test]
fn list_background_work_round_trips_and_reports_id() -> Result<()> {
    let request = Request::ListBackgroundWork { id: 10 };
    let json = serde_json::to_string(&request)?;
    assert_eq!(json, r#"{"type":"list_background_work","id":10}"#);
    assert_eq!(request.id(), 10);

    let decoded = parse_request_json(&json)?;
    let Request::ListBackgroundWork { id } = decoded else {
        return Err(anyhow!("expected ListBackgroundWork"));
    };
    assert_eq!(id, 10);
    Ok(())
}

#[test]
fn existing_cancel_keeps_its_bytes_while_cancel_turn_is_new() -> Result<()> {
    // The plain cancel must not gain a field: an older client's frame and the
    // daemon's own encoder both keep this exact shape.
    let plain = serde_json::to_string(&Request::Cancel { id: 1 })?;
    assert_eq!(plain, r#"{"type":"cancel","id":1}"#);

    // The new request is a separate wire name, so a peer that does not know it
    // fails to decode rather than misreading it as a plain cancel.
    let with_target = serde_json::to_string(&Request::CancelTurn {
        id: 1,
        run_id: String::new(),
    })?;
    assert_eq!(with_target, r#"{"type":"cancel_turn","id":1,"run_id":""}"#);
    Ok(())
}
