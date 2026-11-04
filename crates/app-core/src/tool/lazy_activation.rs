use super::*;
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) struct RecordingTool {
    name: &'static str,
    runs: Arc<AtomicUsize>,
}

impl RecordingTool {
    pub(super) fn new(name: &'static str) -> (Self, Arc<AtomicUsize>) {
        let runs = Arc::new(AtomicUsize::new(0));
        (
            Self {
                name,
                runs: Arc::clone(&runs),
            },
            runs,
        )
    }
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "Test tool that counts its executions."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({"type": "object", "properties": {}})
    }

    async fn execute(&self, _input: Value, _ctx: ToolContext) -> Result<ToolOutput> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::new(format!("{} ran", self.name)))
    }
}

pub(super) fn context() -> ToolContext {
    ToolContext {
        session_id: "lazy-activation".to_string(),
        message_id: "message".to_string(),
        tool_call_id: "call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

pub(super) fn counting_activator(calls: Arc<AtomicUsize>, allow: bool) -> LazyToolActivator {
    Arc::new(move |_name: String| -> BoxFuture<'static, bool> {
        let calls = Arc::clone(&calls);
        Box::pin(async move {
            calls.fetch_add(1, Ordering::SeqCst);
            allow
        })
    })
}

pub(super) async fn registry_with_inactive_tool(name: &'static str) -> (Registry, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let registry = Registry::empty();
    let calls = Arc::new(AtomicUsize::new(0));
    registry.register_lazy_tool_activator(counting_activator(Arc::clone(&calls), true));
    let (tool, runs) = RecordingTool::new(name);
    registry.register_inactive(name.to_string(), Arc::new(tool)).await;
    (registry, calls, runs)
}

#[tokio::test]
async fn lazy_activation_hides_inactive_from_definitions() {
    let registry = Registry::empty();
    let (active, _) = RecordingTool::new("always_on");
    registry.register("always_on".to_string(), Arc::new(active)).await;
    let before: Vec<String> = registry
        .definitions(None)
        .await
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    assert_eq!(before, vec!["always_on".to_string()]);

    for name in ["lazy_one", "lazy_two", "lazy_three"] {
        let (tool, _) = RecordingTool::new(name);
        registry.register_inactive(name.to_string(), Arc::new(tool)).await;
    }

    let after: Vec<String> = registry
        .definitions(None)
        .await
        .into_iter()
        .map(|definition| definition.name)
        .collect();
    assert_eq!(
        after, before,
        "registering three inactive tools must not change the definition list"
    );
}

#[tokio::test]
async fn lazy_activation_calls_activator_once() {
    let (registry, calls, runs) = registry_with_inactive_tool("lazy_echo").await;
    assert!(
        !registry.tool_names().await.iter().any(|name| name == "lazy_echo"),
        "an inactive tool must not be advertised before activation"
    );

    let output = registry
        .execute("lazy_echo", serde_json::json!({}), context())
        .await
        .expect("an activated tool must run");

    assert_eq!(output.output, "lazy_echo ran");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the activator runs exactly once for one call"
    );
    assert_eq!(runs.load(Ordering::SeqCst), 1, "the tool itself runs once");
    assert!(
        registry.tool_names().await.iter().any(|name| name == "lazy_echo"),
        "activation makes the tool callable"
    );
}

#[tokio::test]
async fn lazy_activation_refusal_keeps_error_path() {
    let registry = Registry::empty();
    let calls = Arc::new(AtomicUsize::new(0));
    registry.register_lazy_tool_activator(counting_activator(Arc::clone(&calls), false));
    let (tool, runs) = RecordingTool::new("lazy_refused");
    registry
        .register_inactive("lazy_refused".to_string(), Arc::new(tool))
        .await;

    let error = registry
        .execute("lazy_refused", serde_json::json!({}), context())
        .await
        .expect_err("a refused tool must not run");

    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(runs.load(Ordering::SeqCst), 0, "a refused tool never executes");
    assert!(
        error.to_string().starts_with("Unknown tool: lazy_refused."),
        "a refusal must keep the ordinary error path, got: {error}"
    );
    assert!(
        !registry.tool_names().await.iter().any(|name| name == "lazy_refused"),
        "a refused tool stays hidden"
    );
}

#[tokio::test]
async fn lazy_activation_hides_from_tool_names_and_suggestions() {
    let registry = Registry::empty();
    registry.register_lazy_tool_activator(counting_activator(Arc::new(AtomicUsize::new(0)), true));
    let (active, _) = RecordingTool::new("always_on");
    registry.register("always_on".to_string(), Arc::new(active)).await;
    let (lazy, _) = RecordingTool::new("lazy_grep");
    registry
        .register_inactive("lazy_grep".to_string(), Arc::new(lazy))
        .await;

    assert_eq!(
        registry.tool_names().await,
        vec!["always_on".to_string()],
        "tool_names must omit the inactive tool"
    );

    let error = registry
        .execute("lazy_gre", serde_json::json!({}), context())
        .await
        .expect_err("a name near an inactive tool is still unknown");
    let message = error.to_string();
    assert!(
        message.contains("Available tools: always_on."),
        "the available list must name only callable tools, got: {message}"
    );
    assert!(
        !message.contains("lazy_grep"),
        "an inactive name must not leak through suggestions, got: {message}"
    );
}
