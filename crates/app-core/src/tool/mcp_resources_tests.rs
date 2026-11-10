//! The `mcp` tool's resource and prompt actions, end to end over stdio.

use super::*;
use crate::tool::Tool;

/// The fixture servers are POSIX shell scripts, so a host without `/bin/sh`
/// skips these tests instead of failing them.
fn sh_unavailable(test: &str) -> bool {
    let available = std::path::Path::new("/bin/sh").exists();
    if !available {
        eprintln!("SKIP {test}: /bin/sh is unavailable");
    }
    !available
}

fn fixture_config(script: &str) -> McpServerConfig {
    McpServerConfig {
        command: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        env: Default::default(),
        shared: false,
        transport: None,
        url: None,
        headers: std::collections::HashMap::new(),
        enabled: None,
        disabled: None,
        timeout_secs: None,
    }
}

fn tool_with_server(name: &str, script: &str) -> McpManagementTool {
    let mut config = crate::mcp::McpConfig::default();
    config.servers.insert(name.to_string(), fixture_config(script));
    McpManagementTool::new(Arc::new(RwLock::new(McpManager::with_config(config))))
}

fn test_context() -> ToolContext {
    ToolContext {
        session_id: "test-session".to_string(),
        message_id: "test-message".to_string(),
        tool_call_id: "test-tool-call".to_string(),
        working_dir: None,
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::Direct,
    }
}

/// One resource, one prompt, and a resource whose body is far over the token
/// budget.
const NOTES_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{},"resources":{"listChanged":true},"prompts":{"listChanged":true}},"serverInfo":{"name":"notes","version":"1"}}}'
      ;;
    *'"tools/list"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"tools":[]}}'
      ;;
    *'"resources/list"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"resources":[{"uri":"mem://notes/a","name":"note a","description":"first note","mimeType":"text/plain"},{"uri":"mem://big","name":"big","mimeType":"text/plain"}]}}'
      ;;
    *'"resources/read"'*)
      case "$line" in
        *'mem://big'*)
          big=$(head -c 200000 /dev/zero | tr '\0' 'x')
          echo '{"jsonrpc":"2.0","id":'$id',"result":{"contents":[{"uri":"mem://big","mimeType":"text/plain","text":"'$big'"}]}}'
          ;;
        *)
          echo '{"jsonrpc":"2.0","id":'$id',"result":{"contents":[{"uri":"mem://notes/a","mimeType":"text/plain","text":"alpha body"}]}}'
          ;;
      esac
      ;;
    *'"prompts/list"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"prompts":[{"name":"summarize","description":"Summarize a note","arguments":[{"name":"topic","required":true},{"name":"tone"}]}]}}'
      ;;
    *'"prompts/get"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"description":"Summarize a note","messages":[{"role":"user","content":{"type":"text","text":"Summarize mem://notes/a"}}]}}'
      ;;
  esac
done
"#;

/// A server that publishes tools only.
const TOOLS_ONLY_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"plain","version":"1"}}}'
      ;;
    *'"tools/list"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"tools":[]}}'
      ;;
  esac
done
"#;

#[tokio::test]
async fn mcp_resources_action_lists_and_reads_a_servers_resources() {
    if sh_unavailable("mcp_resources_action_lists_and_reads_a_servers_resources") {
        return;
    }
    let tool = tool_with_server("notes", NOTES_SERVER);

    let listed = tool
        .execute(json!({"action": "resources", "server": "notes"}), test_context())
        .await
        .expect("resources must list");
    assert!(listed.output.contains("mem://notes/a"), "{}", listed.output);
    assert!(listed.output.contains("note a"), "{}", listed.output);
    assert!(listed.output.contains("text/plain"), "{}", listed.output);
    assert!(listed.output.contains("first note"), "{}", listed.output);

    let read = tool
        .execute(
            json!({"action": "read_resource", "server": "notes", "uri": "mem://notes/a"}),
            test_context(),
        )
        .await
        .expect("read_resource must return the body");
    assert_eq!(read.output, "alpha body");

    let prompts = tool
        .execute(json!({"action": "prompts", "server": "notes"}), test_context())
        .await
        .expect("prompts must list");
    assert!(prompts.output.contains("summarize(topic*, tone)"), "{}", prompts.output);
    assert!(prompts.output.contains("Summarize a note"), "{}", prompts.output);

    let prompt = tool
        .execute(
            json!({"action": "get_prompt", "server": "notes", "name": "summarize", "arguments": {"topic": "notes"}}),
            test_context(),
        )
        .await
        .expect("get_prompt must expand the prompt");
    assert!(
        prompt.output.contains("user: Summarize mem://notes/a"),
        "{}",
        prompt.output
    );
}

#[tokio::test]
async fn mcp_resources_actions_report_a_missing_capability_as_a_result() {
    if sh_unavailable("mcp_resources_actions_report_a_missing_capability_as_a_result") {
        return;
    }
    let tool = tool_with_server("plain", TOOLS_ONLY_SERVER);

    let listed = tool
        .execute(json!({"action": "resources", "server": "plain"}), test_context())
        .await
        .expect("a server without the capability is not an error");
    assert!(listed.output.contains("not supported by 'plain'"), "{}", listed.output);

    let read = tool
        .execute(
            json!({"action": "read_resource", "server": "plain", "uri": "mem://notes/a"}),
            test_context(),
        )
        .await
        .expect("read_resource on a server without resources is a plain result");
    assert!(read.output.contains("not supported by 'plain'"), "{}", read.output);

    let prompt = tool
        .execute(
            json!({"action": "get_prompt", "server": "plain", "name": "summarize"}),
            test_context(),
        )
        .await
        .expect("get_prompt on a server without prompts is a plain result");
    assert!(prompt.output.contains("not supported by 'plain'"), "{}", prompt.output);
}

#[tokio::test]
async fn mcp_resources_read_body_honours_the_result_budget() {
    if sh_unavailable("mcp_resources_read_body_honours_the_result_budget") {
        return;
    }
    let tool = tool_with_server("notes", NOTES_SERVER);

    let trimmed = tool
        .execute(
            json!({"action": "read_resource", "server": "notes", "uri": "mem://big"}),
            test_context(),
        )
        .await
        .expect("read_resource must return the body");
    assert!(
        trimmed.output.contains("[MCP result trimmed from "),
        "an oversized resource body must be trimmed like a tool result"
    );
    assert!(trimmed.output.len() < 100_000, "trimmed body stayed large");

    let full = tool
        .execute(
            json!({
                "action": "read_resource",
                "server": "notes",
                "uri": "mem://big",
                "accept_large_output": true
            }),
            test_context(),
        )
        .await
        .expect("read_resource must return the body");
    assert!(
        !full.output.contains("[MCP result trimmed from "),
        "accept_large_output must return the untrimmed body"
    );
    assert!(full.output.len() > 100_000, "full body must not be trimmed");
}
