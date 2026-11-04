//! Manual QA round trip for `browser_mirror`, through the same entry point the
//! socket loop uses (`BridgeState::api_request_to_legacy`).
//!
//! Run with `cargo test -p horon-agent-harness-api-server --test browser_mirror_qa -- --nocapture`.
//! The frames it prints are the evidence file's content, so they are produced
//! by real serialization rather than written by hand.
//!
//! The browser is a stub CLI plus a stub session daemon: this machine has no
//! Browser Agent Bridge extension, so nothing here claims to drive a real
//! browser. What it does prove is the wire shape, the daemon naming the mirror
//! shares with the agent's `browser` tool, and the `browser_unavailable` path.

use horon_agent_harness_api::ApiEvent;
use horon_agent_harness_api_server::translate::{BridgeState, Outbound};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// A stub bridge: a session daemon that reports one bound tab, and a direct
/// mode that lists two windows.
const STUB_CLI: &str = r#"#!/usr/bin/env python3
import json, os, socket, sys
runtime = os.environ.get('XDG_RUNTIME_DIR', '/tmp')
home = os.environ['HORON_AGENT_HOME']
if sys.argv[1:4] == ['session', 'start', '--help']:
    print('--bind-window')
elif sys.argv[1:3] == ['session', 'start']:
    name = sys.argv[3]
    path = runtime + '/browser-session-' + name
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(path + '.sock')
    listener.listen()
    with open(path + '.pid', 'w') as handle:
        handle.write(str(os.getpid()))
    while True:
        conn, _ = listener.accept()
        with conn:
            raw = conn.recv(65536)
            if not raw:
                continue
            request = json.loads(raw)
            if request['action'] == 'getActiveTab':
                reply = {'tabId': 7, 'url': 'https://app.example.com/inbox', 'title': 'Inbox', 'windowId': 900}
            else:
                reply = {'ok': True}
            conn.sendall((json.dumps(reply) + '\n').encode())
else:
    action = sys.argv[1]
    if action == 'listTabs':
        print(json.dumps({'windows': [{'windowId': 900, 'tabs': [
            {'tabId': 7, 'url': 'https://app.example.com/inbox', 'title': 'Inbox'},
            {'tabId': 8, 'url': 'https://docs.example.com/', 'title': 'Docs'}]}]}))
    elif action == 'getActiveTab':
        print(json.dumps({'tabId': 7, 'url': 'https://app.example.com/inbox', 'title': 'Inbox', 'windowId': 900}))
    else:
        print(json.dumps({'ok': True}))
"#;

struct Home {
    temp: PathBuf,
    previous: Option<std::ffi::OsString>,
    previous_browser: Option<std::ffi::OsString>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

/// Serializes the tests in this file that redirect `HORON_AGENT_HOME`. The
/// crate's own lock is `cfg(test)` inside the library, so an integration test
/// cannot reach it.
fn home_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Home {
    fn new(name: &str, with_browser: bool) -> Self {
        let guard = home_lock();
        let temp = std::env::temp_dir().join(format!("horon-agent-mirror-qa-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&temp).unwrap();
        if with_browser {
            install_stub(&temp);
        }
        let previous = std::env::var_os("HORON_AGENT_HOME");
        let previous_browser = std::env::var_os("HORON_AGENT_BROWSER");
        // SAFETY: the home lock serializes every test that mutates these.
        unsafe {
            std::env::set_var("HORON_AGENT_HOME", &temp);
            std::env::set_var("HORON_AGENT_BROWSER", "chrome");
        }
        Self {
            temp,
            previous,
            previous_browser,
            _guard: guard,
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let pid_file = runtime.join(format!(
            "browser-session-{}",
            horon_agent_base::browser::session_name_for(
                &format!("session_mirror_qa-{}", std::process::id()),
                Some("chrome")
            )
        ));
        if let Ok(pid) = std::fs::read_to_string(pid_file.with_extension("pid"))
            && let Ok(pid) = pid.trim().parse::<i32>()
        {
            // SAFETY: signal the pid this stub recorded.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
        for extension in ["pid", "sock"] {
            let _ = std::fs::remove_file(pid_file.with_extension(extension));
        }
        // SAFETY: the home lock is still held.
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var("HORON_AGENT_HOME", value),
                None => std::env::remove_var("HORON_AGENT_HOME"),
            }
            match &self.previous_browser {
                Some(value) => std::env::set_var("HORON_AGENT_BROWSER", value),
                None => std::env::remove_var("HORON_AGENT_BROWSER"),
            }
        }
        let _ = std::fs::remove_dir_all(&self.temp);
    }
}

fn install_stub(home: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let dir = home.join("browser");
    std::fs::create_dir_all(&dir).unwrap();
    let bin = dir.join("browser");
    std::fs::write(&bin, STUB_CLI).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn round_trip(request: Value) -> Value {
    let mut state = BridgeState::default();
    let outbound = state.api_request_to_legacy(&request);
    assert_eq!(outbound.len(), 1, "one request, one frame");
    match outbound.into_iter().next().unwrap() {
        Outbound::Reply(frame) | Outbound::Serialized { frame, .. } => serde_json::to_value(frame).unwrap(),
        Outbound::Legacy(value) => panic!("browser_mirror must answer locally, not forward: {value}"),
    }
}

#[test]
fn state_and_unavailable_round_trips_print_real_frames() {
    let session = format!("session_mirror_qa-{}", std::process::id());
    {
        let _home = Home::new("browser", true);
        let state = round_trip(json!({
            "v": 1, "id": 1, "req": "browser_mirror",
            "session_id": session, "action": "state",
        }));
        println!("STATE {}", serde_json::to_string(&state).unwrap());
        assert_eq!(state["ev"], "browser_mirror_result");
        assert_eq!(state["state"]["tabs"].as_array().unwrap().len(), 2);

        let unsupported = round_trip(json!({
            "v": 1, "id": 2, "req": "browser_mirror",
            "session_id": session, "action": "teleport",
        }));
        println!("UNSUPPORTED {}", serde_json::to_string(&unsupported).unwrap());
        assert_eq!(unsupported["error"]["code"], "unsupported_action");
    }
    {
        let _home = Home::new("missing", false);
        let unavailable = round_trip(json!({
            "v": 1, "id": 3, "req": "browser_mirror",
            "session_id": session, "action": "frame",
        }));
        println!("UNAVAILABLE {}", serde_json::to_string(&unavailable).unwrap());
        assert_eq!(unavailable["ev"], "error");
        assert_eq!(unavailable["code"], "browser_unavailable");
        let message = unavailable["message"].as_str().unwrap();
        assert!(message.contains("horon-agent browser setup"), "{message}");
    }
}

#[test]
fn the_mirror_reports_the_daemon_the_browser_tool_started() {
    let session = format!("session_mirror_qa-{}", std::process::id());
    let _home = Home::new("shared", true);
    // The agent's `browser` tool starts the session daemon on its first action.
    let started = horon_agent_base::browser::ensure_browser_session_for(&session, Some("chrome"));
    let expected = horon_agent_base::browser::session_name_for(&session, Some("chrome"));
    assert_eq!(
        started.as_deref(),
        Some(expected.as_str()),
        "the stub daemon must start"
    );

    let frame = round_trip(json!({
        "v": 1, "id": 1, "req": "browser_mirror",
        "session_id": session, "action": "takeover",
    }));
    println!("TAKEOVER {}", serde_json::to_string(&frame).unwrap());
    assert_eq!(frame["state"]["connected"], true);
    assert_eq!(frame["state"]["mode"], "user");
    assert_eq!(frame["state"]["active_tab_id"], 7);
    assert_eq!(frame["state"]["tabs"][0]["active"], true);
    assert_eq!(frame["state"]["url"], "https://app.example.com/inbox");
}

#[test]
fn the_event_enum_round_trips_through_the_typed_surface() {
    let encoded = round_trip(json!({
        "v": 1, "id": 1, "req": "browser_mirror",
        "session_id": "session_mirror_typed", "action": "teleport",
    }));
    let decoded: horon_agent_harness_api::ServerFrame = serde_json::from_value(encoded).unwrap();
    match decoded.event {
        ApiEvent::BrowserMirrorResult { action, error, .. } => {
            assert_eq!(action, "teleport");
            assert_eq!(error.unwrap().code, "unsupported_action");
        }
        other => panic!("expected a mirror result, got {other:?}"),
    }
}
