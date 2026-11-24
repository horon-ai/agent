//! Opt-in real-time voice session adapter: configuration, consent, stale-capture fencing, and text fallback.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, mpsc};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{Action, App};
use crate::commands::Local;
use crate::keys::ids;
use crate::sdk_bridge::AttachedSession;
use crate::settings::{SettingValue, Settings};
use crate::transcript::Item;
use crate::voice::live::{LiveAdapterEvent, LiveError, LivePhase, LiveVoiceAdapter, LiveVoiceConfig};

fn app() -> App {
    let (_sender, receiver) = mpsc::channel();
    App::new("test".to_string(), receiver)
}

fn app_with_live_settings(enabled: bool, endpoint: &str) -> App {
    let mut app = app();
    let mut settings = Settings::defaults();
    let command = if enabled {
        vec!["horon-mock-live-adapter".to_string()]
    } else {
        Vec::new()
    };
    settings.apply_overrides(&[
        ("voice.live_enabled", SettingValue::Bool(enabled)),
        ("voice.live_adapter", SettingValue::Str("mock".to_string())),
        ("voice.live_endpoint", SettingValue::Str(endpoint.to_string())),
        ("voice.live_command", SettingValue::List(command)),
    ]);
    app.refresh_settings(&settings);
    app
}

fn last_notice(app: &App) -> String {
    app.transcript
        .items()
        .iter()
        .rev()
        .find_map(|item| match item {
            Item::Notice(text) | Item::Warning(text) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[derive(Default)]
struct SharedMockState {
    calls: Vec<String>,
    events: VecDeque<LiveAdapterEvent>,
}

#[derive(Clone, Default)]
struct MockLiveAdapter {
    state: Arc<Mutex<SharedMockState>>,
}

impl MockLiveAdapter {
    fn new() -> Self {
        Self::default()
    }

    fn push_event(&self, event: LiveAdapterEvent) {
        self.state.lock().expect("lock").events.push_back(event);
    }

    fn calls(&self) -> Vec<String> {
        self.state.lock().expect("lock").calls.clone()
    }
}

impl LiveVoiceAdapter for MockLiveAdapter {
    fn start(&mut self, _config: &LiveVoiceConfig, generation: u64) -> Result<(), LiveError> {
        let mut guard = self.state.lock().expect("lock");
        guard.calls.push(format!("start:{generation}"));
        guard.events.push_back(LiveAdapterEvent::Ready { generation });
        Ok(())
    }

    fn begin_capture(&mut self, generation: u64, capture_id: u64) -> Result<(), LiveError> {
        self.state
            .lock()
            .expect("lock")
            .calls
            .push(format!("begin:{generation}:{capture_id}"));
        Ok(())
    }

    fn end_capture(&mut self, generation: u64, capture_id: u64, commit: bool) -> Result<(), LiveError> {
        self.state
            .lock()
            .expect("lock")
            .calls
            .push(format!("end:{generation}:{capture_id}:{commit}"));
        Ok(())
    }

    fn stop(&mut self, generation: u64) -> Result<(), LiveError> {
        self.state
            .lock()
            .expect("lock")
            .calls
            .push(format!("stop:{generation}"));
        Ok(())
    }

    fn reconnect(&mut self, _config: &LiveVoiceConfig, generation: u64) -> Result<(), LiveError> {
        let mut guard = self.state.lock().expect("lock");
        guard.calls.push(format!("reconnect:{generation}"));
        guard.events.push_back(LiveAdapterEvent::Ready { generation });
        Ok(())
    }

    fn poll(&mut self) -> Vec<LiveAdapterEvent> {
        self.state.lock().expect("lock").events.drain(..).collect()
    }

    fn shutdown(&mut self) {
        self.state.lock().expect("lock").calls.push("shutdown".to_string());
    }
}

#[test]
fn live_voice_requires_opt_in_and_has_no_preconfigured_endpoint() {
    let defaults = Settings::defaults();
    let config = LiveVoiceConfig::from_settings(&defaults);
    assert!(!config.enabled, "live voice must default to off");
    assert!(
        config.endpoint.is_empty(),
        "no third-party live voice endpoint may be preconfigured: {}",
        config.endpoint
    );
    assert!(config.fallback_to_text, "text fallback stays enabled by default");

    let mut app = app();
    app.run_live_voice("consent");
    app.run_live_voice("");
    assert_eq!(app.voice.live.phase(), LivePhase::Idle);
    let notice = last_notice(&app);
    assert!(notice.contains("voice.live_enabled"), "{notice}");

    // Plain keyboard text submission remains untouched.
    app.input.set_text("text fallback works");
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        Some(Action::Send("text fallback works".to_string()))
    );
}

#[test]
fn live_voice_validates_endpoint_scheme_and_requires_session_consent() {
    let mut app = app_with_live_settings(true, "ftp://unsupported.example.test");
    app.voice.live.set_adapter(Box::new(MockLiveAdapter::new()));
    app.run_live_voice("consent");
    app.run_live_voice("start");
    assert_eq!(app.voice.live.phase(), LivePhase::Idle);
    let notice = last_notice(&app);
    assert!(notice.contains("endpoint"), "{notice}");

    // Valid endpoint still requires explicit session consent before starting adapter.
    let mut app = app_with_live_settings(true, "wss://voice.internal.example/live");
    let mock = MockLiveAdapter::new();
    app.voice.live.set_adapter(Box::new(mock.clone()));
    app.run_live_voice("start");
    assert_eq!(app.voice.live.phase(), LivePhase::Idle);
    assert!(last_notice(&app).contains("consent"));
    assert!(mock.calls().is_empty(), "adapter must not start without consent");
}

#[test]
fn live_voice_mock_adapter_starts_stops_and_reconnects_without_submitting_stale_capture() {
    let mut app = app_with_live_settings(true, "wss://voice.internal.example/live");
    let mock = MockLiveAdapter::new();
    app.voice.live.set_adapter(Box::new(mock.clone()));
    app.run_live_voice("consent");

    // Start live session -> Ready (generation 1).
    app.run_live_voice("start");
    assert_eq!(app.voice.live.phase(), LivePhase::Connecting);
    assert!(app.tick_live_voice());
    assert_eq!(app.voice.live.phase(), LivePhase::Ready);
    let gen1 = app.voice.live.generation();

    // Begin capture 1, then stop without committing.
    app.run_live_voice("talk");
    assert_eq!(app.voice.live.phase(), LivePhase::Capturing);
    let cap1 = app.voice.live.active_capture_id().expect("active capture 1");
    app.run_live_voice("stop");
    assert_eq!(app.voice.live.phase(), LivePhase::Idle);

    // Late transcript from stopped capture 1 MUST be discarded, never sent.
    mock.push_event(LiveAdapterEvent::Transcript {
        generation: gen1,
        capture_id: cap1,
        text: "stale capture after stop".to_string(),
        is_final: true,
    });
    let _ = app.tick_live_voice();
    assert_eq!(app.take_queued(), None, "stale capture after stop must not submit");
    assert!(
        !app.transcript
            .items()
            .iter()
            .any(|item| matches!(item, Item::User(body) if body.contains("stale capture"))),
        "stale capture must not enter transcript"
    );

    // Start again, begin capture 2, then reconnect before completion.
    app.run_live_voice("start");
    app.tick_live_voice();
    assert_eq!(app.voice.live.phase(), LivePhase::Ready);
    let gen2 = app.voice.live.generation();
    app.run_live_voice("talk");
    let cap2 = app.voice.live.active_capture_id().expect("active capture 2");
    app.run_live_voice("reconnect");
    assert_eq!(app.voice.live.phase(), LivePhase::Reconnecting);

    // Late transcript from pre-reconnect generation/capture arrives before or after Ready.
    mock.push_event(LiveAdapterEvent::Transcript {
        generation: gen2,
        capture_id: cap2,
        text: "stale capture across reconnect".to_string(),
        is_final: true,
    });
    app.tick_live_voice();
    assert_eq!(app.voice.live.phase(), LivePhase::Ready);
    assert_eq!(
        app.take_queued(),
        None,
        "stale capture across reconnect must not submit"
    );
    assert!(app.voice.live.dropped_stale() >= 2);

    // Fresh capture on post-reconnect generation 3 commits and submits cleanly.
    let gen3 = app.voice.live.generation();
    assert!(gen3 > gen2);
    app.run_live_voice("talk");
    let cap3 = app.voice.live.active_capture_id().expect("active capture 3");
    app.run_live_voice("end");
    assert_eq!(app.voice.live.phase(), LivePhase::Ready);
    mock.push_event(LiveAdapterEvent::Transcript {
        generation: gen3,
        capture_id: cap3,
        text: "fresh utterance after reconnect".to_string(),
        is_final: true,
    });
    assert!(app.tick_live_voice());
    assert_eq!(
        app.take_queued(),
        Some(Action::Send("fresh utterance after reconnect".to_string()))
    );
}

#[test]
fn live_voice_leave_restores_text_mode_and_preserves_session_state() {
    let mut app = app_with_live_settings(true, "wss://voice.internal.example/live");
    let mock = MockLiveAdapter::new();
    app.voice.live.set_adapter(Box::new(mock.clone()));
    app.session_id = Some("sess-text-1".to_string());
    app.transcript.push_user("earlier message".to_string());
    app.input.set_text("unsent text draft");

    app.run_live_voice("consent");
    app.run_live_voice("start");
    app.tick_live_voice();
    let gen_before = app.voice.live.generation();
    app.run_live_voice("talk");
    let cap = app.voice.live.active_capture_id().expect("capture");

    // Leave live audio mode: consent is forgotten while text session, transcript history, and draft survive intact.
    app.run_live_voice("leave");
    assert!(!app.voice.live.consent_given());
    assert_eq!(app.voice.live.phase(), LivePhase::Idle);
    assert_eq!(app.session_id.as_deref(), Some("sess-text-1"));
    assert_eq!(app.input.text(), "unsent text draft");
    assert!(
        app.transcript
            .items()
            .iter()
            .any(|item| item == &Item::User("earlier message".to_string()))
    );

    // Any capture arriving after leaving is ignored.
    mock.push_event(LiveAdapterEvent::Transcript {
        generation: gen_before,
        capture_id: cap,
        text: "after leave".to_string(),
        is_final: true,
    });
    app.tick_live_voice();
    assert_eq!(app.take_queued(), None);

    // Leaving forgot consent, so starting live mode again prompts for fresh /live consent.
    app.run_live_voice("start");
    assert!(last_notice(&app).contains("Type /live consent"));

    // Typing/sending in the text session still works.
    assert_eq!(
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        Some(Action::Send("unsent text draft".to_string()))
    );
}

#[test]
fn live_voice_session_switch_and_disconnect_forget_consent_and_stop_capture() {
    let mut app = app_with_live_settings(true, "wss://voice.internal.example/live");
    let mock = MockLiveAdapter::new();
    app.voice.live.set_adapter(Box::new(mock));
    app.run_live_voice("consent");
    app.run_live_voice("start");
    app.tick_live_voice();
    app.run_live_voice("talk");
    assert_eq!(app.voice.live.phase(), LivePhase::Capturing);

    app.attach_session(
        AttachedSession {
            description: "socket · sess-next".to_string(),
            session_id: "sess-next".to_string(),
            status: "idle".to_string(),
            model: None,
            effort: None,
            history: Vec::new(),
            title: None,
        },
        false,
    );
    assert!(!app.voice.live.consent_given());
    assert_eq!(app.voice.live.phase(), LivePhase::Idle);

    // Re-consent, start capture, then simulate disconnect.
    app.run_live_voice("consent");
    app.run_live_voice("start");
    app.tick_live_voice();
    app.disconnected = true;
    app.tick_live_voice();
    assert!(!app.voice.live.consent_given());
    assert_eq!(app.voice.live.phase(), LivePhase::Idle);
}

#[test]
fn live_command_and_keybinding_coexist_with_ctrl_l_repaint() {
    let mut app = app();
    // Ctrl+L continues to trigger terminal repaint (#482), not live voice.
    assert_eq!(
        app.bindings
            .resolve_editing(&crate::keys::Key::parse("ctrl+l").expect("parse")),
        Some(ids::APP_REPAINT)
    );
    assert_eq!(
        app.bindings
            .resolve_editing(&crate::keys::Key::parse("alt+v").expect("parse")),
        Some(ids::APP_LIVE_VOICE)
    );
    assert_eq!(
        app.commands.resolve("/live status"),
        crate::commands::Invocation::Local {
            command: Local::Live,
            arguments: "status".to_string(),
        }
    );

    // Alt+V invokes live voice toggle cleanly without crashing.
    app.on_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT));
    assert!(last_notice(&app).contains("live voice"));
}

#[test]
fn live_voice_forget_preserves_already_accepted_pending_turn_and_disconnect_clears_it() {
    let mut app = app_with_live_settings(true, "wss://voice.internal.example/live");
    let mock = MockLiveAdapter::new();
    app.voice.live.set_adapter(Box::new(mock.clone()));
    app.run_live_voice("consent");
    app.run_live_voice("start");
    app.tick_live_voice();
    let gen_id = app.voice.live.generation();
    app.run_live_voice("talk");
    let cap = app.voice.live.active_capture_id().expect("capture");
    app.run_live_voice("end");

    mock.push_event(LiveAdapterEvent::Transcript {
        generation: gen_id,
        capture_id: cap,
        text: "accepted before forget".to_string(),
        is_final: true,
    });
    app.tick_live_voice();

    // Forgetting/leaving after the turn was accepted revokes consent without dropping the queued Action::Send.
    app.run_live_voice("forget");
    assert!(!app.voice.live.consent_given());
    assert_eq!(
        app.take_queued(),
        Some(Action::Send("accepted before forget".to_string()))
    );

    // Re-consent and deliver another transcript, then drop the connection before take_queued drains it.
    app.run_live_voice("consent");
    app.run_live_voice("start");
    app.tick_live_voice();
    let gen_id = app.voice.live.generation();
    app.run_live_voice("talk");
    let cap = app.voice.live.active_capture_id().expect("capture");
    app.run_live_voice("end");
    mock.push_event(LiveAdapterEvent::Transcript {
        generation: gen_id,
        capture_id: cap,
        text: "stale across disconnect".to_string(),
        is_final: true,
    });
    app.tick_live_voice();
    app.disconnected = true;
    app.tick_live_voice();
    assert!(!app.voice.live.consent_given());
    assert_eq!(app.take_queued(), None);
}

#[test]
fn live_voice_reconnect_failure_stops_active_capture_and_sanitizes_control_bytes() {
    let mut app = app_with_live_settings(true, "wss://voice.internal.example/live");
    let mock = MockLiveAdapter::new();
    app.voice.live.set_adapter(Box::new(mock.clone()));
    app.run_live_voice("consent");
    app.run_live_voice("start");
    app.tick_live_voice();
    let gen_id = app.voice.live.generation();
    app.run_live_voice("talk");
    let cap = app.voice.live.active_capture_id().expect("capture");
    app.run_live_voice("end");

    mock.push_event(LiveAdapterEvent::Transcript {
        generation: gen_id,
        capture_id: cap,
        text: "hello\x1b[31m\n\x07world\x1b[0m".to_string(),
        is_final: true,
    });
    app.tick_live_voice();
    assert_eq!(app.take_queued(), Some(Action::Send("hello[31m world[0m".to_string())));

    app.run_live_voice("talk");
    assert_eq!(app.voice.live.phase(), LivePhase::Capturing);

    let mut broken = Settings::defaults();
    broken.apply_overrides(&[
        ("voice.live_enabled", SettingValue::Bool(true)),
        ("voice.live_adapter", SettingValue::Str("mock".to_string())),
        (
            "voice.live_endpoint",
            SettingValue::Str("ftp://invalid.example/live".to_string()),
        ),
    ]);
    app.refresh_settings(&broken);
    app.run_live_voice("reconnect");
    assert_eq!(app.voice.live.phase(), LivePhase::Idle);
    assert_eq!(app.voice.live.active_capture_id(), None);
    assert!(last_notice(&app).contains("voice.live_endpoint"));
}

#[test]
fn command_live_adapter_subprocess_lifecycle_handles_events_and_shutdown() {
    use std::io::Read as _;

    let python = ["python3", "python"]
        .into_iter()
        .find(|bin| {
            std::process::Command::new(bin)
                .arg("-V")
                .output()
                .is_ok_and(|out| out.status.success())
        })
        .expect("python3 or python is required to exercise CommandLiveAdapter");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback socket");
    let port = listener.local_addr().expect("listener addr").port();
    let script = r#"
import json, os, socket, sys
hold = socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=5)
for raw in sys.stdin:
    line = raw.strip()
    if not line:
        continue
    msg = json.loads(line)
    kind = msg.get("type")
    gen = msg.get("generation", 0)
    if kind == "start":
        if os.environ.get("HORON_LIVE_MODEL") != "live-model-test":
            print(json.dumps({"type": "error", "generation": gen, "reason": "missing model env"}), flush=True)
            sys.exit(1)
        if os.environ.get("HORON_LIVE_ENDPOINT") != "wss://voice.internal.example/live":
            print(json.dumps({"type": "error", "generation": gen, "reason": "missing endpoint env"}), flush=True)
            sys.exit(1)
        print(json.dumps({"type": "ready", "generation": gen}), flush=True)
    elif kind == "begin_capture":
        cap = msg.get("capture_id", 0)
        print(json.dumps({"type": "transcript", "generation": gen, "capture_id": cap, "text": "partial", "final": False}), flush=True)
    elif kind == "end_capture":
        cap = msg.get("capture_id", 0)
        if msg.get("commit"):
            print(json.dumps({"type": "transcript", "generation": gen, "capture_id": cap, "text": "hello\x1b[7m subproc", "final": True}), flush=True)
    elif kind == "stop":
        break
"#;

    let mut app = app();
    let mut settings = Settings::defaults();
    settings.apply_overrides(&[
        ("voice.live_enabled", SettingValue::Bool(true)),
        ("voice.live_adapter", SettingValue::Str("command".to_string())),
        (
            "voice.live_endpoint",
            SettingValue::Str("wss://voice.internal.example/live".to_string()),
        ),
        ("voice.live_model", SettingValue::Str("live-model-test".to_string())),
        (
            "voice.live_command",
            SettingValue::List(vec![
                python.to_string(),
                "-u".to_string(),
                "-c".to_string(),
                script.to_string(),
                port.to_string(),
            ]),
        ),
    ]);
    app.refresh_settings(&settings);
    app.run_live_voice("consent");
    app.run_live_voice("start");

    let (mut conn, _) = listener
        .accept()
        .expect("adapter subprocess connects to loopback socket");
    conn.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .expect("set read timeout");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        app.tick_live_voice();
        if app.voice.live.phase() == LivePhase::Ready {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(app.voice.live.phase(), LivePhase::Ready);

    app.run_live_voice("talk");
    std::thread::sleep(std::time::Duration::from_millis(50));
    app.tick_live_voice();
    assert_eq!(app.take_queued(), None, "partial transcript must not submit");

    app.run_live_voice("end");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut sent = None;
    while std::time::Instant::now() < deadline {
        app.tick_live_voice();
        if let Some(action) = app.take_queued() {
            sent = Some(action);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(sent, Some(Action::Send("hello[7m subproc".to_string())));

    // Leaving shuts down the child subprocess (closing its OS socket FD) and revokes consent.
    app.run_live_voice("leave");
    assert!(!app.voice.live.consent_given());
    assert_eq!(app.voice.live.phase(), LivePhase::Idle);
    let mut eof = [0u8; 8];
    let bytes = conn.read(&mut eof).expect("read EOF after adapter termination");
    assert_eq!(bytes, 0, "leaving live mode must terminate the adapter child");

    // Non-existent command falls back immediately without leaving active state.
    let mut missing = Settings::defaults();
    missing.apply_overrides(&[
        ("voice.live_enabled", SettingValue::Bool(true)),
        ("voice.live_adapter", SettingValue::Str("command".to_string())),
        (
            "voice.live_endpoint",
            SettingValue::Str("wss://voice.internal.example/live".to_string()),
        ),
        (
            "voice.live_command",
            SettingValue::List(vec!["horon-nonexistent-live-adapter-bin".to_string()]),
        ),
    ]);
    app.refresh_settings(&missing);
    app.run_live_voice("consent");
    app.run_live_voice("start");
    assert_eq!(app.voice.live.phase(), LivePhase::Idle);
    assert!(last_notice(&app).contains("cannot start live voice adapter"));
}
