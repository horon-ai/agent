//! Context notes ride in the dynamic system prompt, never the cached prefix,
//! and disappear with the conversation they describe.

use super::*;

#[tokio::test]
async fn context_notes_reach_the_next_request_and_clear_with_the_branch() {
    let sandbox = crate::auth::test_sandbox::AuthTestSandbox::new().unwrap();
    std::fs::write(sandbox.root().join("config.toml"), "[tools]\ncontext_notes = true\n").unwrap();
    crate::config::Config::invalidate_cache();

    let (provider, prompts) = shared_round_provider(vec![vec![
        StreamEvent::TextDelta("done".into()),
        StreamEvent::MessageEnd {
            stop_reason: Some("end_turn".into()),
        },
    ]]);
    let registry = Registry::new(provider.clone()).await;
    let mut agent = Agent::new(provider, registry);

    crate::context_notes::save(&agent.session.id, "- next: parse_x\n- done: read a.rs").unwrap();
    agent.add_message(
        crate::message::Role::User,
        vec![crate::message::ContentBlock::Text {
            text: "continue".into(),
            cache_control: None,
        }],
    );
    agent.run_turn(false).await.unwrap();

    {
        let prompts = prompts.lock().unwrap();
        let prompt = prompts.first().expect("the turn made one request");
        assert!(prompt.contains("# Context Notes"), "{prompt}");
        assert!(prompt.contains("Your working notes for this session"), "{prompt}");
        assert!(prompt.contains("- next: parse_x"), "{prompt}");
    }

    let path = crate::context_notes::notes_path(&agent.session.id).unwrap();
    assert!(path.exists(), "the notes stay on disk between requests");
    agent.clear();
    assert!(
        !path.exists(),
        "clearing the conversation must drop the notes of the cleared branch"
    );
}
