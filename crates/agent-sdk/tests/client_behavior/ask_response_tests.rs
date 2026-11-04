use super::*;
use horon_agent_sdk::AskSelection;

#[test]
fn respond_to_ask_sends_the_ask_response_for_the_echoed_id() {
    let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = std::sync::Arc::clone(&requests);
    let client = fake_harness(move |frame, writer| {
        seen.lock().expect("request log").push(frame.request.clone());
        reply(frame, ApiEvent::Ok, writer);
    });

    let answers = vec![
        AskSelection {
            question_id: "q1".to_string(),
            selected: vec!["Fast".to_string()],
        },
        AskSelection {
            question_id: "q2".to_string(),
            selected: vec!["Safe".to_string(), "Cheap".to_string()],
        },
    ];
    client
        .respond_to_ask("s1", "ask-7", answers.clone())
        .expect("ask response");

    let requests = requests.lock().expect("request log");
    assert_eq!(requests.len(), 1);
    assert!(matches!(
        &requests[0],
        ApiRequest::AskResponse {
            session_id,
            ask_id,
            answers: sent,
            answer_id,
        } if session_id == "s1" && ask_id == "ask-7" && sent == &answers && answer_id.is_none()
    ));
}
