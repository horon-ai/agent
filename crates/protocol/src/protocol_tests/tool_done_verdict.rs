// Wire shape of `tool_done` once it carries the stored verdict (protocol
// 1.19). Additive: a call no layer judged writes the same bytes it did before
// the field existed, and a client that predates the field still decodes.

#[test]
fn tool_done_without_a_verdict_writes_the_pre_verdict_frame() -> Result<()> {
    let event = ServerEvent::tool_done("call-1", "bash", "ok", None);
    assert_eq!(
        encode_event(&event),
        "{\"type\":\"tool_done\",\"id\":\"call-1\",\"name\":\"bash\",\"output\":\"ok\"}\n",
        "an unjudged call must stay byte-identical to the frame that predates the field"
    );
    Ok(())
}

#[test]
fn tool_done_with_a_verdict_round_trips_the_wire_name() -> Result<()> {
    let event = ServerEvent::ToolDone {
        id: "call-1".to_string(),
        name: "bash".to_string(),
        output: "denied".to_string(),
        error: Some("denied".to_string()),
        verdict: Some(horon_agent_tool_types::ToolVerdict::Rejected),
    };
    assert_eq!(
        encode_event(&event),
        "{\"type\":\"tool_done\",\"id\":\"call-1\",\"name\":\"bash\",\"output\":\"denied\",\
         \"error\":\"denied\",\"verdict\":\"rejected\"}\n"
    );
    let ServerEvent::ToolDone { verdict, .. } = parse_event_json(encode_event(&event).trim())? else {
        return Err(anyhow!("expected ToolDone"));
    };
    assert_eq!(verdict, Some(horon_agent_tool_types::ToolVerdict::Rejected));
    Ok(())
}

#[test]
fn tool_done_decodes_a_frame_that_predates_the_verdict() -> Result<()> {
    let older = r#"{"type":"tool_done","id":"call-1","name":"bash","output":"ok"}"#;
    let ServerEvent::ToolDone { id, verdict, .. } = parse_event_json(older)? else {
        return Err(anyhow!("expected ToolDone"));
    };
    assert_eq!(id, "call-1");
    assert_eq!(verdict, None, "an absent verdict is not a judgment");
    Ok(())
}
