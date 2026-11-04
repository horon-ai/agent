use super::*;
use crate::tool::Registry;
use std::collections::HashSet;

struct JsonTool;

#[async_trait::async_trait]
impl Tool for JsonTool {
    fn name(&self) -> &str {
        "lookup"
    }

    fn description(&self) -> &str {
        "Return structured data for an id\nMore detail."
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object", "properties": {"id": {"type": "number"}}})
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        let id = input["id"].as_i64().unwrap_or_default();
        Ok(ToolOutput::new(json!({ "id": id, "square": id * id }).to_string()))
    }
}

struct TextTool;

#[async_trait::async_trait]
impl Tool for TextTool {
    fn name(&self) -> &str {
        "shout"
    }

    fn description(&self) -> &str {
        "Uppercase text"
    }

    fn parameters_schema(&self) -> Value {
        json!({"type": "object"})
    }

    async fn execute(&self, input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        Ok(ToolOutput::new(
            input["text"].as_str().unwrap_or_default().to_uppercase(),
        ))
    }
}

fn ctx(session: &str) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: "message".to_string(),
        tool_call_id: "codemode-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: super::super::ToolExecutionMode::Direct,
    }
}

async fn registry() -> Registry {
    let registry = Registry::empty();
    let mut tools = registry.tools.write().await;
    tools.insert("lookup".to_string(), Arc::new(JsonTool));
    tools.insert("shout".to_string(), Arc::new(TextTool));
    tools.insert(
        "codemode".to_string(),
        Arc::new(CodemodeTool::new(registry.downgrade())),
    );
    drop(tools);
    registry
}

#[tokio::test(flavor = "multi_thread")]
async fn script_composes_registry_tools() {
    let registry = registry().await;
    let out = registry
        .execute(
            "codemode",
            json!({
                "code": r#"
                    const rows = await Promise.all([1, 2, 3].map((id) => tools.lookup({ id })));
                    const loud = await tools.shout({ text: "done" });
                    console.log("rows", rows.length);
                    return { total: rows.reduce((s, r) => s + r.square, 0), loud };
                "#
            }),
            ctx("codemode-compose-test"),
        )
        .await
        .expect("codemode runs");
    assert!(out.output.contains("\"total\": 14"), "{}", out.output);
    assert!(out.output.contains("\"loud\": \"DONE\""), "{}", out.output);
    assert!(out.output.contains("console:\nrows 3"), "{}", out.output);
    assert!(out.output.contains("[4 tool calls"), "{}", out.output);
}

#[tokio::test(flavor = "multi_thread")]
async fn script_cannot_call_itself_or_batch() {
    let registry = registry().await;
    let out = registry
        .execute(
            "codemode",
            json!({ "code": "return listTools().map((t) => t.name);" }),
            ctx("codemode-exclusion-test"),
        )
        .await
        .expect("codemode runs");
    assert!(out.output.contains("lookup"), "{}", out.output);
    assert!(!out.output.contains("\"codemode\""), "{}", out.output);

    let err = registry
        .execute(
            "codemode",
            json!({ "code": "return await tools.codemode({ code: 'return 1' });" }),
            ctx("codemode-exclusion-test"),
        )
        .await;
    let text = match err {
        Ok(out) => out.output,
        Err(err) => err.to_string(),
    };
    assert!(text.contains("unknown tool 'codemode'"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn session_policy_applies_inside_scripts() {
    let session = "codemode-policy-test";
    crate::tool::set_session_tool_policy(session, None, HashSet::from(["shout".to_string()]));
    let registry = registry().await;
    let out = registry
        .execute(
            "codemode",
            json!({ "code": "return [listTools().map((t) => t.name), 'shout' in tools];" }),
            ctx(session),
        )
        .await;
    crate::tool::clear_session_tool_policy(session);
    let out = out.expect("codemode runs");
    assert!(!out.output.contains("\"shout\""), "{}", out.output);
    assert!(out.output.contains("false"), "{}", out.output);
}

#[tokio::test(flavor = "multi_thread")]
async fn uncaught_errors_without_side_effects_are_tool_errors() {
    let registry = registry().await;
    let err = registry
        .execute(
            "codemode",
            json!({ "code": "throw new Error('broken script')" }),
            ctx("codemode-error-test"),
        )
        .await
        .expect_err("pure failure is a tool error");
    assert!(err.to_string().contains("broken script"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn script_tool_calls_measure_the_session_budget_not_the_default_one() {
    const SESSION_BUDGET: usize = 20_000;

    struct HugeTool;

    #[async_trait::async_trait]
    impl Tool for HugeTool {
        fn name(&self) -> &str {
            "huge"
        }
        fn description(&self) -> &str {
            "Return an oversized result"
        }
        fn parameters_schema(&self) -> Value {
            json!({"type": "object"})
        }
        async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
            // Condensed to the 48 KiB generic budget (~8k tokens) before the
            // guard sees it: above the 6k single-output ceiling of a 20k session
            // budget, below the 50k ceiling of the 200k default budget that a
            // plain clone installs.
            Ok(ToolOutput::new("x".repeat(150_000)))
        }
    }

    let registry = Registry::empty();
    {
        let mut tools = registry.tools.write().await;
        tools.insert("huge".to_string(), Arc::new(HugeTool));
        tools.insert(
            "codemode".to_string(),
            Arc::new(CodemodeTool::new(registry.downgrade())),
        );
    }
    *registry.compaction().try_write().expect("fresh lock is uncontended") =
        crate::compaction::CompactionManager::new().with_budget(SESSION_BUDGET);

    let out = registry
        .execute(
            "codemode",
            json!({"code": "const text = await tools.huge({}); return text.slice(0, 120);"}),
            ctx("codemode-budget-test"),
        )
        .await
        .expect("codemode runs");

    let prefix: String = out.output.chars().take(200).collect();
    println!("QA codemode report prefix={prefix}");
    assert!(
        out.output.contains("OUTPUT WITHHELD"),
        "a script's tool call must be refused against the session's budget, got: {prefix}"
    );
    assert!(
        out.output.contains("20k"),
        "the refusal must name the session budget, not the default one, got: {prefix}"
    );
}

#[test]
fn report_renders_strings_raw_and_lists_store_keys() {
    let mut store = Map::new();
    store.insert("k".into(), json!(1));
    let out = horon_agent_codemode::RunOutput {
        result: Some(json!("plain text")),
        tool_calls: 1,
        store,
        ..Default::default()
    };
    let report = render_report(&out);
    assert!(report.starts_with("result:\nplain text\n"), "{report}");
    assert!(report.contains("[1 tool call, "), "{report}");
    assert!(report.contains("stored: k]"), "{report}");
}
