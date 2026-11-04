use super::{AskTool, Tool, ToolContext};
use crate::config::AskPolicy;
use crate::tool::ToolExecutionMode;
use serde_json::{Value, json};

fn context() -> ToolContext {
    ToolContext {
        session_id: "ask-tests".to_string(),
        message_id: "message".to_string(),
        tool_call_id: "call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn two_questions() -> Value {
    json!({
        "questions": [
            {
                "id": "scope",
                "question": "Which surface should change?",
                "header": "Scope",
                "options": [
                    {"label": "daemon only", "description": "Keep clients untouched"},
                    {"label": "daemon and desktop"},
                    {"label": "every client"}
                ],
                "recommended": 1
            },
            {
                "id": "style",
                "question": "How strict should validation be?",
                "options": [
                    {"label": "lenient"},
                    {"label": "strict"}
                ]
            }
        ]
    })
}

async fn run(policy: AskPolicy, input: Value) -> anyhow::Result<super::ToolOutput> {
    AskTool::with_policy(policy).execute(input, context()).await
}

#[tokio::test]
async fn ask_recommended_resolves_every_question() {
    let output = run(AskPolicy::Recommended, two_questions())
        .await
        .expect("recommended policy resolves");

    let metadata = output.metadata.expect("answers travel in metadata");
    assert_eq!(metadata["policy"], json!("recommended"));
    let answers = metadata["answers"].as_array().expect("answers array");
    assert_eq!(answers.len(), 2, "both questions must be answered");

    assert_eq!(answers[0]["question_id"], json!("scope"));
    assert_eq!(answers[0]["selected"], json!(["daemon and desktop"]));
    assert_eq!(answers[0]["auto_resolved"], json!(true));

    assert_eq!(answers[1]["question_id"], json!("style"));
    assert_eq!(
        answers[1]["selected"],
        json!(["lenient"]),
        "a question without a recommendation resolves to its first option"
    );
    assert_eq!(answers[1]["auto_resolved"], json!(true));

    assert!(
        output.output.contains("automatically"),
        "the tool text must say the choice was automatic, got {:?}",
        output.output
    );
    assert!(output.output.contains("daemon and desktop"));
    assert!(output.output.contains("lenient"));
}

#[tokio::test]
async fn ask_error_policy_fails_closed() {
    let error = run(AskPolicy::Error, two_questions())
        .await
        .expect_err("error policy refuses");
    let message = error.to_string();
    assert!(
        message.contains("cannot answer questions"),
        "the error must explain why nobody answers, got {message:?}"
    );
    assert!(message.contains("ask_policy"));
}

#[tokio::test]
async fn ask_rejects_option_counts_outside_the_bounds() {
    let one_option = json!({"questions": [{"question": "q", "options": [{"label": "only"}]}]});
    let error = run(AskPolicy::Recommended, one_option)
        .await
        .expect_err("one option is too few");
    assert!(error.to_string().contains("between 2 and 5 options"), "{error}");

    let six_options = json!({"questions": [{
        "question": "q",
        "options": [
            {"label": "a"}, {"label": "b"}, {"label": "c"},
            {"label": "d"}, {"label": "e"}, {"label": "f"}
        ]
    }]});
    let error = run(AskPolicy::Recommended, six_options)
        .await
        .expect_err("six options are too many");
    assert!(error.to_string().contains("between 2 and 5 options"), "{error}");
}

#[tokio::test]
async fn ask_rejects_more_than_four_questions() {
    let questions: Vec<Value> = (0..5)
        .map(|i| json!({"id": format!("q{i}"), "question": "q", "options": [{"label": "a"}, {"label": "b"}]}))
        .collect();
    let error = run(AskPolicy::Recommended, json!({"questions": questions}))
        .await
        .expect_err("five questions exceed the cap");
    assert!(error.to_string().contains("at most 4 questions"), "{error}");
}

#[tokio::test]
async fn ask_rejects_an_out_of_range_recommendation() {
    let input = json!({"questions": [{
        "id": "pick",
        "question": "q",
        "options": [{"label": "a"}, {"label": "b"}],
        "recommended": 2
    }]});
    let error = run(AskPolicy::Recommended, input)
        .await
        .expect_err("recommended must index an option");
    let message = error.to_string();
    assert!(message.contains("recommends option 2"), "{message}");
    assert!(message.contains("2 options"), "{message}");
}

#[tokio::test]
async fn ask_rejects_duplicate_question_ids() {
    let input = json!({"questions": [
        {"id": "same", "question": "first", "options": [{"label": "a"}, {"label": "b"}]},
        {"id": "same", "question": "second", "options": [{"label": "c"}, {"label": "d"}]}
    ]});
    let error = run(AskPolicy::Recommended, input)
        .await
        .expect_err("ids must be unique");
    assert!(error.to_string().contains("must be unique"), "{error}");
}

#[tokio::test]
async fn ask_rejects_an_empty_question_list() {
    let error = run(AskPolicy::Recommended, json!({"questions": []}))
        .await
        .expect_err("an empty request is meaningless");
    assert!(error.to_string().contains("at least 1 question"), "{error}");
}

#[test]
fn ask_policy_parses_and_defaults_to_recommended() {
    assert_eq!(crate::config::ToolConfig::default().ask_policy, AskPolicy::Recommended);
    assert_eq!(AskPolicy::parse(" RECOMMENDED "), Some(AskPolicy::Recommended));
    assert_eq!(AskPolicy::parse("error"), Some(AskPolicy::Error));
    assert_eq!(AskPolicy::parse("block"), None);
    assert_eq!(AskPolicy::Recommended.as_str(), "recommended");
    assert_eq!(AskPolicy::Error.as_str(), "error");
}
