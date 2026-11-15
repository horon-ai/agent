//! Server-initiated JSON-RPC requests: every one of them gets an answer.
//!
//! A server that asks the client something waits for a reply. Before this file
//! existed the client dropped the frame, so the server waited out its own
//! timeout, indistinguishable from a hang. The fake server below sends one
//! request of each kind, plus a notification, and records exactly what comes
//! back.

use super::McpClient;
use crate::mcp::protocol::{METHOD_NOT_FOUND_CODE, McpRoot, McpServerConfig};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The probe server. On `initialize` it answers, then immediately asks the
/// client three things and sends one notification; every later inbound frame
/// that carries `result` or `error` is a client reply and is appended to the
/// capture file.
const PROBE_SERVER: &str = r#"
out=$1
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' "$line" > "${out}.initialize"
      printf '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"probe","version":"1"}}}\n'
      printf '{"jsonrpc":"2.0","id":10,"method":"ping"}\n'
      printf '{"jsonrpc":"2.0","id":11,"method":"sampling/createMessage","params":{"maxTokens":8}}\n'
      printf '{"jsonrpc":"2.0","id":12,"method":"roots/list"}\n'
      printf '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}\n'
      ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}\n'
      ;;
    *'"result"'*|*'"error"'*)
      printf '%s\n' "$line" >> "$out"
      ;;
  esac
done
"#;

/// Isolated agent home plus the capture files the probe server writes.
struct Probe {
    _guard: std::sync::MutexGuard<'static, ()>,
    home: tempfile::TempDir,
    root: PathBuf,
    previous_home: Option<std::ffi::OsString>,
}

impl Probe {
    fn new(expose_roots: bool) -> Self {
        let guard = crate::storage::lock_test_env();
        let home = tempfile::tempdir().expect("agent home");
        let previous_home = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", home.path());
        if expose_roots {
            std::fs::write(home.path().join("config.toml"), "[mcp]\nexpose_roots = true\n").expect("config");
        }
        crate::config::Config::invalidate_cache();
        let root = home.path().join("session-root");
        std::fs::create_dir_all(&root).expect("root dir");
        Self {
            _guard: guard,
            home,
            root,
            previous_home,
        }
    }

    fn capture(&self) -> PathBuf {
        self.home.path().join("replies.jsonl")
    }

    fn config(&self) -> McpServerConfig {
        McpServerConfig {
            command: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                PROBE_SERVER.to_string(),
                "probe".to_string(),
                self.capture().display().to_string(),
            ],
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

    fn initialize_frame(&self) -> Value {
        let text = std::fs::read_to_string(format!("{}.initialize", self.capture().display()))
            .expect("the probe must record the initialize frame");
        serde_json::from_str(&text).expect("the initialize frame is JSON")
    }

    /// Wait until the probe has recorded `count` replies. Returns them parsed,
    /// so a wrong frame fails on the comparison rather than on a timeout.
    async fn replies(&self, count: usize) -> Vec<Value> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let text = std::fs::read_to_string(self.capture()).unwrap_or_default();
            let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
            if lines.len() >= count {
                return lines
                    .into_iter()
                    .map(|line| serde_json::from_str(line).expect("a client reply is JSON"))
                    .collect();
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the client answered {} of {count} server requests: {text:?}",
                lines.len()
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        match self.previous_home.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
        crate::config::Config::invalidate_cache();
    }
}

fn sh_unavailable(test: &str) -> bool {
    let available = Path::new("/bin/sh").exists();
    if !available {
        eprintln!("SKIP {test}: /bin/sh is unavailable");
    }
    !available
}

#[tokio::test]
async fn server_requests_are_answered_and_a_notification_is_not() {
    if sh_unavailable("server_requests_are_answered_and_a_notification_is_not") {
        return;
    }
    let probe = Probe::new(false);
    let client = McpClient::connect_in_dir("probe".to_string(), &probe.config(), Some(&probe.root))
        .await
        .expect("the probe server connects");
    let handle = client.handle();
    assert!(!handle.is_closed());

    let replies = probe.replies(3).await;

    // ping: an empty result, so the server's ping resolves instead of timing out.
    assert_eq!(replies[0], json!({"jsonrpc": "2.0", "id": 10, "result": {}}));
    // Everything else: a named "method not found" error.
    assert_eq!(
        replies[1],
        json!({
            "jsonrpc": "2.0",
            "id": 11,
            "error": {"code": METHOD_NOT_FOUND_CODE, "message": "Method not found: sampling/createMessage"},
        })
    );
    // roots/list without the opt-in is refused, and `roots` was never declared.
    assert_eq!(
        replies[2],
        json!({
            "jsonrpc": "2.0",
            "id": 12,
            "error": {"code": METHOD_NOT_FOUND_CODE, "message": "Method not found: roots/list"},
        })
    );
    assert!(
        !probe.initialize_frame()["params"]["capabilities"]
            .as_object()
            .expect("capabilities is an object")
            .contains_key("roots"),
        "a client that refuses roots/list must not declare the capability"
    );

    // The notification (no `id`) earns no reply: the probe would have recorded a
    // fourth frame if one had been written.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let after = std::fs::read_to_string(probe.capture()).unwrap_or_default();
    assert_eq!(
        after.lines().filter(|line| !line.trim().is_empty()).count(),
        3,
        "a notification must not be answered: {after}"
    );
}

#[tokio::test]
async fn roots_are_served_when_the_opt_in_is_on() {
    if sh_unavailable("roots_are_served_when_the_opt_in_is_on") {
        return;
    }
    let probe = Probe::new(true);
    let client = McpClient::connect_in_dir("probe".to_string(), &probe.config(), Some(&probe.root))
        .await
        .expect("the probe server connects");
    assert!(!client.handle().is_closed());

    let replies = probe.replies(3).await;
    assert_eq!(replies[0], json!({"jsonrpc": "2.0", "id": 10, "result": {}}));
    assert_eq!(
        replies[1],
        json!({
            "jsonrpc": "2.0",
            "id": 11,
            "error": {"code": METHOD_NOT_FOUND_CODE, "message": "Method not found: sampling/createMessage"},
        })
    );

    // The session working directory, percent-encoded as one file:// root. The
    // directory name is plain ASCII, so the expectation needs no encoder.
    let expected_uri = format!("file://{}/", probe.root.display());
    assert_eq!(
        replies[2],
        json!({"jsonrpc": "2.0", "id": 12, "result": {"roots": [{"uri": expected_uri}]}})
    );
    // A percent-encoded encoding of the same path must agree with the encoder,
    // not with a hand-written string.
    assert_eq!(
        McpRoot::directory(&probe.root).expect("root").uri,
        expected_uri,
        "the root URI is a plain file:// URL for a plain path"
    );

    // The declared capability matches what the connection serves.
    assert_eq!(
        probe.initialize_frame()["params"]["capabilities"],
        json!({"roots": {"listChanged": false}})
    );
}
