//! Resource and prompt round trips against a fixture stdio server.

use super::McpClient;
use crate::mcp::protocol::{ContentBlock, McpServerConfig};
use serde_json::json;

/// A stdio server that publishes one resource and one prompt.
///
/// The first `resources/list` answers with one resource, later ones with two,
/// so a cached result and a refetched one are distinguishable. `resources/read`
/// sends `notifications/resources/list_changed` immediately before its reply,
/// which makes the invalidation observable without a sleep: the client's reader
/// handles the frames in order, so the notification is applied before the read
/// call returns.
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

const RESOURCES_AND_PROMPTS_SERVER: &str = r#"
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  lists=${lists:-0}
  case "$line" in
    *'"initialize"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{},"resources":{"listChanged":true},"prompts":{"listChanged":true}},"serverInfo":{"name":"notes","version":"1"}}}'
      ;;
    *'"tools/list"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"tools":[]}}'
      ;;
    *'"resources/list"'*)
      lists=$((lists+1))
      if [ "$lists" -eq 1 ]; then
        echo '{"jsonrpc":"2.0","id":'$id',"result":{"resources":[{"uri":"mem://notes/a","name":"a","description":"first note","mimeType":"text/plain"}]}}'
      else
        echo '{"jsonrpc":"2.0","id":'$id',"result":{"resources":[{"uri":"mem://notes/a","name":"a","mimeType":"text/plain"},{"uri":"mem://notes/b","name":"b","mimeType":"text/plain"}]}}'
      fi
      ;;
    *'"resources/read"'*)
      echo '{"jsonrpc":"2.0","method":"notifications/resources/list_changed"}'
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"contents":[{"uri":"mem://notes/a","mimeType":"text/plain","text":"alpha body"}]}}'
      ;;
    *'"prompts/list"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"prompts":[{"name":"summarize","description":"Summarize a note","arguments":[{"name":"topic","required":true}]}]}}'
      ;;
    *'"prompts/get"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"result":{"description":"Summarize a note","messages":[{"role":"user","content":{"type":"text","text":"Summarize mem://notes/a"}}]}}'
      ;;
  esac
done
"#;

/// A server that only publishes tools: no `resources`, no `prompts`.
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
    *'"resources/list"'*|*'"prompts/list"'*)
      echo '{"jsonrpc":"2.0","id":'$id',"error":{"code":-32601,"message":"method not found"}}'
      ;;
  esac
done
"#;

#[tokio::test]
async fn mcp_resources_and_prompts_round_trip() {
    let client = McpClient::connect("notes".to_string(), &fixture_config(RESOURCES_AND_PROMPTS_SERVER))
        .await
        .expect("fixture server must connect");
    let handle = client.handle();
    assert!(handle.supports_resources(), "resources capability must be seen");
    assert!(handle.supports_prompts(), "prompts capability must be seen");

    let resources = handle.list_resources().await.expect("resources/list");
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0].uri, "mem://notes/a");
    assert_eq!(resources[0].name, "a");
    assert_eq!(resources[0].mime_type.as_deref(), Some("text/plain"));
    assert_eq!(resources[0].description.as_deref(), Some("first note"));

    let read = handle.read_resource("mem://notes/a").await.expect("resources/read");
    assert_eq!(read.contents.len(), 1);
    assert_eq!(read.contents[0].text.as_deref(), Some("alpha body"));

    let prompts = handle.list_prompts().await.expect("prompts/list");
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].name, "summarize");
    assert_eq!(prompts[0].arguments.len(), 1);
    assert_eq!(prompts[0].arguments[0].name, "topic");
    assert!(prompts[0].arguments[0].required);

    let prompt = handle
        .get_prompt("summarize", Some(json!({"topic": "notes"})))
        .await
        .expect("prompts/get");
    assert_eq!(prompt.messages.len(), 1);
    assert_eq!(prompt.messages[0].role, "user");
    match &prompt.messages[0].content {
        ContentBlock::Text { text } => assert_eq!(text, "Summarize mem://notes/a"),
        other => panic!("prompt content must be text, got {other:?}"),
    }
}

#[tokio::test]
async fn mcp_resources_server_without_capability_is_rejected_before_any_request() {
    let client = McpClient::connect("plain".to_string(), &fixture_config(TOOLS_ONLY_SERVER))
        .await
        .expect("fixture server must connect");
    let handle = client.handle();
    assert!(!handle.supports_resources());
    assert!(!handle.supports_prompts());

    let error = handle
        .list_resources()
        .await
        .expect_err("a server without the capability must not answer");
    let message = error.to_string();
    assert!(
        message.contains("not supported by 'plain'"),
        "error must name the missing capability: {message}"
    );
    assert!(
        !message.contains("MCP error"),
        "the client must reject locally, not surface a server error: {message}"
    );

    let error = handle
        .read_resource("mem://notes/a")
        .await
        .expect_err("reading a resource without the capability must fail locally");
    assert!(error.to_string().contains("not supported by 'plain'"));
    let error = handle
        .get_prompt("summarize", None)
        .await
        .expect_err("expanding a prompt without the capability must fail locally");
    assert!(error.to_string().contains("not supported by 'plain'"));
}

#[tokio::test]
async fn mcp_resources_list_changed_notification_invalidates_the_list_cache() {
    let client = McpClient::connect("notes".to_string(), &fixture_config(RESOURCES_AND_PROMPTS_SERVER))
        .await
        .expect("fixture server must connect");
    let handle = client.handle();

    let first = handle.list_resources().await.expect("first resources/list");
    assert_eq!(first.len(), 1);

    // The server announces the change on the same stream, immediately before
    // answering this read, so the reader has applied it once the call returns.
    handle.read_resource("mem://notes/a").await.expect("resources/read");

    let second = handle.list_resources().await.expect("refetch after list_changed");
    assert_eq!(second.len(), 2, "list_changed must drop the cached list");
}

#[test]
fn mcp_resources_binary_bodies_are_summarized_and_secrets_redacted() {
    use crate::mcp::protocol::ResourceContent;

    let binary = ResourceContent {
        uri: "mem://logo.png".to_string(),
        mime_type: Some("image/png".to_string()),
        text: None,
        blob: Some("aGVsbG8=".to_string()),
    };
    assert_eq!(
        crate::mcp::render_resource_body(&binary),
        "[Binary resource: mem://logo.png (image/png, ~5 bytes)]"
    );

    let secret = ResourceContent {
        uri: "mem://env".to_string(),
        mime_type: Some("text/plain".to_string()),
        text: Some("GITHUB_TOKEN=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123".to_string()),
        blob: None,
    };
    let rendered = crate::mcp::render_resource_body(&secret);
    assert!(
        rendered.contains("[REDACTED_SECRET]"),
        "resource bodies go through the tool-result redaction pass: {rendered}"
    );
}
