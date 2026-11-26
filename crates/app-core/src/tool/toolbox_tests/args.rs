//! How `toolbox run` reads arguments a model did not put in `args`.

use super::*;

/// A weaker model drops `args` or spreads the target's fields beside `action`;
/// either way the call must run or tell it exactly what to send, because a
/// bare refusal is repeated until the loop guard ends the turn.
#[tokio::test]
async fn run_reads_spread_or_stringified_args_and_explains_a_missing_one() {
    let _guard = crate::storage::lock_test_env();
    let _policy = PolicyGuard::install(tiers(&[]));
    let registry = registry().await;

    for input in [
        json!({"action": "run", "tool": "readonly_probe", "intent": "x", "text": "spread"}),
        json!({"action": "run", "tool": "readonly_probe", "args": "{\"text\":\"spread\"}"}),
    ] {
        let output = call(&registry, input.clone()).await.expect("the arguments were found");
        assert_eq!(output.output, "echo:spread", "{input}");
    }

    let refusal = call(
        &registry,
        json!({"action": "run", "tool": "readonly_probe", "intent": "x"}),
    )
    .await
    .expect_err("a required field has no value")
    .to_string();
    for part in [
        "needs `args`",
        "Required: text",
        r#"{"action":"run","args":{"text":"..."},"tool":"readonly_probe"}"#,
        "From the next request `readonly_probe` is also a direct tool",
    ] {
        assert!(refusal.contains(part), "missing {part}: {refusal}");
    }
    // The refused tool is handed to the session's agent to send directly, once.
    assert_eq!(
        crate::tool::take_direct_requests(SESSION),
        vec!["readonly_probe".to_string()]
    );
    assert!(crate::tool::take_direct_requests(SESSION).is_empty());
}
