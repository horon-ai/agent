//! Mirror-surface tests: the decisions the module makes, plus one real round
//! trip through the CLI runner.
//!
//! The browser itself is a fake here: this machine has no bridge extension, so
//! a test that needed one would be a test nobody could run. What the fake does
//! not cover — spawning the CLI, draining its pipes, enforcing the deadline,
//! reading the screenshot file — is covered by the `CliBridge` tests at the
//! bottom, which run a stub `browser` executable through the real path.

use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64};

struct RecordedCall {
    action: String,
    params: Value,
    session: Option<String>,
}

struct Fake {
    running: AtomicBool,
    start_succeeds: bool,
    tabs: Value,
    active: Value,
    screenshot: Option<Vec<u8>>,
    timing_out: Vec<&'static str>,
    install_problem: Option<String>,
    click_failures: AtomicU64,
    resets: AtomicU64,
    starts: AtomicU64,
    calls: Mutex<Vec<RecordedCall>>,
}

impl Fake {
    fn new() -> Self {
        Self {
            running: AtomicBool::new(false),
            start_succeeds: true,
            tabs: json!({ "windows": [] }),
            active: json!({}),
            screenshot: None,
            timing_out: Vec::new(),
            install_problem: None,
            click_failures: AtomicU64::new(0),
            resets: AtomicU64::new(0),
            starts: AtomicU64::new(0),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn running(mut self) -> Self {
        self.running = AtomicBool::new(true);
        self
    }

    fn tabs(mut self, tabs: Value) -> Self {
        self.tabs = tabs;
        self
    }

    fn active(mut self, active: Value) -> Self {
        self.active = active;
        self
    }

    fn screenshot(mut self, bytes: Vec<u8>) -> Self {
        self.screenshot = Some(bytes);
        self
    }

    fn timing_out(mut self, action: &'static str) -> Self {
        self.timing_out.push(action);
        self
    }

    fn not_installed(mut self, message: &str) -> Self {
        self.install_problem = Some(message.to_string());
        self
    }

    fn click_failures(&self, count: u64) {
        self.click_failures.store(count, Ordering::Relaxed);
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|call| match call.action.as_str() {
                "listTabs" => "listTabs",
                "getActiveTab" => "getActiveTab",
                "screenshot" => "screenshot",
                "click" => "click",
                "navigate" => "navigate",
                other => panic!("unexpected action {other}"),
            })
            .collect()
    }
}

impl BrowserBridge for Fake {
    fn target(&self) -> Option<BrowserTarget> {
        Some(BrowserTarget {
            name: "Chrome",
            extensions_page: "chrome://extensions",
        })
    }

    fn install_problem(&self) -> Option<String> {
        self.install_problem.clone()
    }

    fn session(&self, _session_id: &str, start: bool) -> Option<String> {
        if start {
            self.starts.fetch_add(1, Ordering::Relaxed);
            return if self.start_succeeds {
                Some("fake-session".to_string())
            } else {
                None
            };
        }
        if self.running.load(Ordering::Relaxed) {
            Some("fake-session".to_string())
        } else {
            None
        }
    }

    fn reset_session(&self, _session_id: &str) {
        self.resets.fetch_add(1, Ordering::Relaxed);
        self.running.store(true, Ordering::Relaxed);
    }

    fn call(&self, action: &str, params: Value, session: Option<&str>, _timeout: Duration) -> CallOutcome {
        self.calls.lock().unwrap().push(RecordedCall {
            action: action.to_string(),
            params: params.clone(),
            session: session.map(str::to_string),
        });
        if let Some(problem) = &self.install_problem {
            return CallOutcome::Unavailable(problem.clone());
        }
        if self.timing_out.contains(&action) {
            return CallOutcome::Timeout;
        }
        if action == "click" && self.click_failures.load(Ordering::Relaxed) > 0 {
            self.click_failures.fetch_sub(1, Ordering::Relaxed);
            return CallOutcome::Failed("No tab with id: 41.".to_string());
        }
        match action {
            "listTabs" => CallOutcome::Json(self.tabs.clone()),
            "getActiveTab" => CallOutcome::Json(self.active.clone()),
            "screenshot" => match &self.screenshot {
                Some(bytes) => CallOutcome::Image(bytes.clone()),
                None => CallOutcome::Json(json!({ "ok": true })),
            },
            _ => CallOutcome::Json(json!({ "ok": true })),
        }
    }
}

fn request(session: &str, action: &str, args: Value) -> Value {
    json!({
        "v": 1,
        "id": 7,
        "req": "browser_mirror",
        "session_id": session,
        "action": action,
        "args": args,
    })
}

fn run(bridge: &dyn BrowserBridge, request: &Value) -> Value {
    run_with(bridge, request, &SocketPush { socket: None })
}

fn run_with(bridge: &dyn BrowserBridge, request: &Value, push: &dyn ModePush) -> Value {
    let outbound = handle_with(7, request, bridge, push);
    assert_eq!(outbound.len(), 1, "one request must produce exactly one frame");
    match outbound.into_iter().next().unwrap() {
        Outbound::Reply(frame) => serde_json::to_value(frame).unwrap(),
        other => panic!("expected a reply frame, got {other:?}"),
    }
}

fn two_tabs() -> Value {
    json!({
        "windows": [
            {
                "windowId": 1,
                "tabs": [
                    { "tabId": 7, "url": "https://app.example.com/", "title": "Example App" },
                    { "tabId": 8, "url": "https://docs.example.com/", "title": "Docs" }
                ]
            }
        ]
    })
}

fn active_tab() -> Value {
    json!({
        "tabId": 8,
        "url": "https://docs.example.com/",
        "title": "Docs",
        "windowId": 1
    })
}

fn solid_png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(width, height, image::Rgb([30, 120, 200]));
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    png
}

/// Deterministic RGB noise: the worst case for JPEG, which is what a frame that
/// genuinely cannot fit the budget looks like.
fn noise_png(width: u32, height: u32) -> Vec<u8> {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut image = image::RgbImage::new(width, height);
    for pixel in image.pixels_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *pixel = image::Rgb([
            (state & 0xFF) as u8,
            ((state >> 8) & 0xFF) as u8,
            ((state >> 16) & 0xFF) as u8,
        ]);
    }
    let mut png = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    png
}

#[test]
fn an_unsupported_action_is_refused_without_touching_the_browser() {
    let fake = Fake::new().running();
    let frame = run(
        &fake,
        &request("session_mirror_unsupported", "launch_missiles", json!({})),
    );

    assert_eq!(frame["ev"], "browser_mirror_result");
    assert_eq!(frame["action"], "launch_missiles");
    assert_eq!(frame["error"]["code"], "unsupported_action");
    let message = frame["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("state, frame, navigate"),
        "the refusal must list what is supported: {message}"
    );
    assert!(fake.calls().is_empty(), "a refused action must not reach the browser");
}

#[test]
fn a_missing_browser_is_unavailable_and_answers_immediately() {
    let fake = Fake::new().running().not_installed(
        "the horon-agent browser bridge is not installed for Chrome. Install it with `horon-agent browser setup`",
    );
    let started = Instant::now();
    let frame = run(&fake, &request("session_mirror_missing", "frame", json!({})));

    assert_eq!(frame["ev"], "error");
    assert_eq!(frame["code"], "browser_unavailable");
    assert!(frame["message"].as_str().unwrap().contains("horon-agent browser setup"));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "an unavailable browser must not be waited on, took {:?}",
        started.elapsed()
    );
    assert!(
        fake.calls().is_empty(),
        "an uninstalled bridge must be reported, not called"
    );
}

#[test]
fn a_mutation_without_a_bridge_names_the_install_not_a_missing_window() {
    let fake = Fake::new().not_installed(
        "the horon-agent browser bridge is not installed for Chrome. Install it with `horon-agent browser setup`",
    );
    let frame = run(
        &fake,
        &request(
            "session_mirror_install",
            "input",
            json!({ "kind": "click", "x": 1.0, "y": 2.0 }),
        ),
    );

    assert_eq!(frame["code"], "browser_unavailable");
    let message = frame["message"].as_str().unwrap();
    assert!(message.contains("not installed"), "{message}");
    assert!(!message.contains("no browser window"), "{message}");
}

#[test]
fn a_call_that_never_answers_is_unavailable_and_names_the_timeout() {
    let fake = Fake::new().timing_out("listTabs");
    let frame = run(&fake, &request("session_mirror_timeout", "state", json!({})));

    assert_eq!(frame["ev"], "error");
    assert_eq!(frame["code"], "browser_unavailable");
    let message = frame["message"].as_str().unwrap();
    assert!(message.contains("did not answer within 2s"), "{message}");
    assert!(message.contains("chrome://extensions"), "{message}");
}

#[test]
fn a_frame_over_the_budget_is_refused_with_the_limit_in_the_message() {
    let error = encode_frame(&noise_png(1600, 1200)).unwrap_err();
    match error {
        Failure::Refused { code, message } => {
            assert_eq!(code, "frame_too_large");
            assert!(message.contains("512 KB"), "{message}");
            assert!(message.contains(&MAX_FRAME_BASE64.to_string()), "{message}");
            assert!(message.contains("refused rather than truncated"), "{message}");
        }
        Failure::Unavailable(message) => panic!("a large frame is a refusal, not an outage: {message}"),
    }
}

#[test]
fn a_frame_that_fits_is_jpeg_within_the_budget() {
    let frame = encode_frame(&solid_png(320, 200)).unwrap();

    assert_eq!(frame.media_type, "image/jpeg");
    assert!(frame.bytes > 0);
    assert!(
        frame.data.len() <= MAX_FRAME_BASE64,
        "{} bytes of base64",
        frame.data.len()
    );
    let decoded = STANDARD.decode(&frame.data).unwrap();
    assert_eq!(decoded.len() as u64, frame.bytes);
    assert_eq!(
        &decoded[..3],
        &[0xFF, 0xD8, 0xFF],
        "the payload must start with a JPEG SOI marker"
    );
}

struct FakePush {
    pushes: Mutex<Vec<(String, &'static str)>>,
}

impl FakePush {
    fn new() -> Self {
        Self {
            pushes: Mutex::new(Vec::new()),
        }
    }

    fn pushed(&self) -> Vec<(String, &'static str)> {
        self.pushes.lock().unwrap().clone()
    }
}

impl ModePush for FakePush {
    fn push(&self, session_id: &str, mode: Mode) {
        self.pushes
            .lock()
            .unwrap()
            .push((session_id.to_string(), mode.as_str()));
    }
}

#[test]
fn a_takeover_pushes_user_control_to_the_daemon() {
    let fake = Fake::new().running().tabs(two_tabs()).active(active_tab());
    let push = FakePush::new();
    let session = "session_mirror_push_takeover";

    let frame = run_with(&fake, &request(session, "takeover", json!({})), &push);

    assert_eq!(frame["state"]["mode"], "user");
    assert_eq!(
        push.pushed(),
        vec![(session.to_string(), "user")],
        "the daemon must be told before the panel is told"
    );
}

#[test]
fn a_resume_pushes_agent_control_immediately() {
    let fake = Fake::new().running().tabs(two_tabs()).active(active_tab());
    let push = FakePush::new();
    let session = "session_mirror_push_resume";

    run_with(&fake, &request(session, "takeover", json!({})), &push);
    let resumed = run_with(&fake, &request(session, "resume", json!({})), &push);

    assert_eq!(resumed["state"]["mode"], "agent");
    assert_eq!(
        push.pushed(),
        vec![(session.to_string(), "user"), (session.to_string(), "agent")],
        "a resume is pushed right away rather than waiting out the lease"
    );
}

#[test]
fn ordinary_polls_do_not_push_anything() {
    let fake = Fake::new().running().tabs(two_tabs()).active(active_tab());
    let push = FakePush::new();

    run_with(&fake, &request("session_mirror_push_idle", "state", json!({})), &push);

    assert!(
        push.pushed().is_empty(),
        "an agent-driven session has nothing to announce"
    );
}

#[test]
fn a_live_takeover_is_renewed_before_the_daemon_lease_expires() {
    let fake = Fake::new().running().tabs(two_tabs()).active(active_tab());
    let push = FakePush::new();
    let session = "session_mirror_push_renew";

    run_with(&fake, &request(session, "takeover", json!({})), &push);
    run_with(&fake, &request(session, "state", json!({})), &push);
    assert_eq!(push.pushed().len(), 1, "a fresh lease is not re-pushed on every poll");

    modes().get_mut(session).unwrap().pushed_at = Some(Instant::now() - PUSH_RENEW - Duration::from_secs(1));
    run_with(&fake, &request(session, "state", json!({})), &push);

    assert_eq!(
        push.pushed(),
        vec![(session.to_string(), "user"), (session.to_string(), "user")],
        "a panel that keeps polling keeps the daemon's lease alive"
    );
}

#[cfg(unix)]
mod push_socket {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    #[test]
    fn the_push_names_the_session_and_mode_on_the_daemon_socket() {
        // A unix socket path is capped at SUN_LEN, and the per-user temp
        // directory is already long, so this uses a short path under /tmp.
        let dir = std::path::PathBuf::from("/tmp").join(format!(
            "hm-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let daemon = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            stream.write_all(b"{\"type\":\"ack\",\"id\":0}\n").unwrap();
            line
        });

        push_browser_mode(&socket, "session_push_wire", Mode::User);

        let line = daemon.join().unwrap();
        let frame: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(frame["type"], "browser_mode");
        assert_eq!(frame["session_id"], "session_push_wire");
        assert_eq!(frame["mode"], "user");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_daemon_socket_makes_the_push_a_no_op() {
        let dir = std::path::PathBuf::from("/tmp").join(format!("hm-miss-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        push_browser_mode(&dir.join("nothing.sock"), "session_push_missing", Mode::User);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn takeover_then_resume_changes_the_reported_mode_and_state_reflects_it() {
    let fake = Fake::new().running().tabs(two_tabs()).active(active_tab());
    let session = "session_mirror_mode";

    let taken = run(&fake, &request(session, "takeover", json!({})));
    assert_eq!(taken["ev"], "browser_mirror_result");
    assert_eq!(taken["state"]["mode"], "user", "takeover answers with the new mode");

    let observed = run(&fake, &request(session, "state", json!({})));
    assert_eq!(
        observed["state"]["mode"], "user",
        "a later poll still sees user control"
    );

    let resumed = run(&fake, &request(session, "resume", json!({})));
    assert_eq!(resumed["state"]["mode"], "agent");

    let observed = run(&fake, &request(session, "state", json!({})));
    assert_eq!(
        observed["state"]["mode"], "agent",
        "resume returns control to the agent"
    );
}

#[test]
fn a_takeover_that_nobody_refreshes_expires_back_to_agent() {
    let session = "session_mirror_lease";
    modes().insert(
        session.to_string(),
        ModeEntry {
            mode: Mode::User,
            touched: Instant::now() - MODE_LEASE - Duration::from_secs(1),
            pushed_at: Some(Instant::now() - MODE_LEASE - Duration::from_secs(1)),
        },
    );

    assert_eq!(current_mode(session), Mode::Agent);
    assert!(
        modes().get(session).is_none(),
        "an expired lease is dropped, not kept for the next connection to find"
    );
}

#[test]
fn state_lists_every_tab_and_marks_the_active_one() {
    let fake = Fake::new().running().tabs(two_tabs()).active(active_tab());
    let frame = run(&fake, &request("session_mirror_state", "state", json!({})));

    assert_eq!(frame["ev"], "browser_mirror_result");
    let state = &frame["state"];
    assert_eq!(state["connected"], true);
    assert_eq!(state["mode"], "agent");
    assert_eq!(state["tabs"].as_array().unwrap().len(), 2);
    assert_eq!(state["active_tab_id"], 8);
    assert_eq!(state["url"], "https://docs.example.com/");
    assert_eq!(state["title"], "Docs");
    assert_eq!(state["tabs"][0]["id"], 7);
    assert_eq!(state["tabs"][0]["active"], false);
    assert_eq!(state["tabs"][1]["active"], true);
    // The listing is unscoped on purpose, like the tool's `list_tabs`: the
    // session daemon would hide the user's own tabs.
    assert_eq!(fake.calls(), vec!["listTabs", "getActiveTab"]);
    let calls = fake.calls.lock().unwrap();
    assert_eq!(
        calls[0].session, None,
        "the listing must not be scoped to the agent's window"
    );
    assert_eq!(calls[1].session.as_deref(), Some("fake-session"));
}

#[test]
fn state_without_an_agent_window_reports_tabs_but_not_connected() {
    let fake = Fake::new().tabs(two_tabs());
    let frame = run(&fake, &request("session_mirror_no_window", "state", json!({})));

    let state = &frame["state"];
    assert_eq!(state["connected"], false);
    assert_eq!(state["tabs"].as_array().unwrap().len(), 2);
    assert!(state["active_tab_id"].is_null());
    assert_eq!(state["url"], "");
    assert_eq!(fake.starts.load(Ordering::Relaxed), 0, "state must never open a window");
}

#[test]
fn a_frame_does_not_open_a_window() {
    let fake = Fake::new().screenshot(solid_png(64, 64));
    let frame = run(&fake, &request("session_mirror_frame_window", "frame", json!({})));

    assert_eq!(frame["ev"], "error");
    assert_eq!(frame["code"], "browser_unavailable");
    assert!(frame["message"].as_str().unwrap().contains("new_tab"));
    assert_eq!(
        fake.starts.load(Ordering::Relaxed),
        0,
        "a frame poll must not spawn a browser"
    );
    assert!(
        fake.calls().is_empty(),
        "nothing should reach the browser without a window"
    );
}

#[test]
fn a_frame_returns_jpeg_for_the_agent_window() {
    let fake = Fake::new().running().screenshot(solid_png(200, 120));
    let frame = run(&fake, &request("session_mirror_frame", "frame", json!({})));

    assert_eq!(frame["ev"], "browser_mirror_result");
    assert_eq!(frame["frame"]["media_type"], "image/jpeg");
    assert!(frame["frame"]["bytes"].as_u64().unwrap() > 0);
    assert!(frame["frame"]["data"].as_str().unwrap().len() <= MAX_FRAME_BASE64);
}

#[test]
fn a_stale_tab_id_is_refused_and_never_silently_switched() {
    let fake = Fake::new().running();
    fake.click_failures(1);
    let frame = run(
        &fake,
        &request(
            "session_mirror_stale",
            "input",
            json!({ "kind": "click", "tab_id": 41, "x": 10.0, "y": 20.0 }),
        ),
    );

    assert_eq!(frame["error"]["code"], "stale_tab");
    assert!(frame["error"]["message"].as_str().unwrap().contains("re-read `state`"));
    assert_eq!(
        fake.calls(),
        vec!["click"],
        "an explicit tab is never retried on another tab"
    );
    assert_eq!(
        fake.resets.load(Ordering::Relaxed),
        0,
        "a closed user tab is not a session problem"
    );
}

#[test]
fn an_untargeted_stale_session_is_recovered_exactly_once() {
    let fake = Fake::new().running();
    fake.click_failures(1);
    let frame = run(
        &fake,
        &request(
            "session_mirror_recover",
            "input",
            json!({ "kind": "click", "x": 10.0, "y": 20.0 }),
        ),
    );

    assert!(frame["error"].is_null(), "the retry must succeed: {frame}");
    assert_eq!(fake.calls(), vec!["click", "click"]);
    assert_eq!(fake.resets.load(Ordering::Relaxed), 1, "one restart, not a loop");
}

#[test]
fn input_kinds_map_onto_the_existing_browser_actions() {
    let fake = Fake::new().running();

    run(
        &fake,
        &request(
            "session_mirror_inputs",
            "input",
            json!({ "kind": "text", "text": "hello", "selector": "#q" }),
        ),
    );
    run(
        &fake,
        &request(
            "session_mirror_inputs",
            "input",
            json!({ "kind": "key", "key": "Enter" }),
        ),
    );
    run(
        &fake,
        &request(
            "session_mirror_inputs",
            "input",
            json!({ "kind": "scroll", "y": 240.0 }),
        ),
    );
    let calls = fake.calls.lock().unwrap();
    assert_eq!(calls[0].action, "type");
    assert_eq!(calls[0].params["text"], "hello");
    assert_eq!(calls[0].params["selector"], "#q");
    assert_eq!(
        calls[1].action, "evaluate",
        "the bridge has no press action; the tool synthesizes it"
    );
    assert!(calls[1].params["script"].as_str().unwrap().contains("keydown"));
    assert_eq!(calls[2].action, "scroll");
    assert_eq!(calls[2].params["y"], 240.0);
}

#[test]
fn a_bad_request_shape_is_refused_before_any_work() {
    let fake = Fake::new().running();
    let missing_action = json!({"v":1,"id":1,"req":"browser_mirror","session_id":"session_mirror_bad"});
    let frame = run(&fake, &missing_action);
    assert_eq!(frame["ev"], "error");
    assert_eq!(frame["code"], "invalid_request");

    let bad_session = json!({"v":1,"id":2,"req":"browser_mirror","session_id":"../etc","action":"state"});
    let frame = run(&fake, &bad_session);
    assert_eq!(frame["code"], "invalid_request");
    assert!(fake.calls().is_empty());
}

#[test]
fn the_request_and_reply_wire_shapes_round_trip() {
    let bare = json!({"req":"browser_mirror","session_id":"s1","action":"state"});
    let parsed: ApiRequest = serde_json::from_value(bare.clone()).unwrap();
    assert_eq!(
        parsed,
        ApiRequest::BrowserMirror {
            session_id: "s1".into(),
            action: "state".into(),
            args: None,
        }
    );
    assert_eq!(
        serde_json::to_value(&parsed).unwrap(),
        bare,
        "args stays absent when unused"
    );

    let frame = ServerFrame::reply(
        9,
        ApiEvent::BrowserMirrorResult {
            session_id: "s1".into(),
            action: "state".into(),
            state: Some(BrowserState {
                connected: true,
                tabs: vec![BrowserTabState {
                    id: 7,
                    url: "https://example.com/".into(),
                    title: "Example".into(),
                    active: true,
                }],
                active_tab_id: Some(7),
                url: "https://example.com/".into(),
                title: "Example".into(),
                mode: "agent".into(),
            }),
            frame: None,
            error: None,
        },
    );
    let encoded = serde_json::to_value(&frame).unwrap();
    assert_eq!(encoded["ev"], "browser_mirror_result");
    assert!(encoded["frame"].is_null());
    let decoded: ServerFrame = serde_json::from_value(encoded).unwrap();
    assert_eq!(decoded, frame);
}

#[cfg(unix)]
mod cli {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    struct Home {
        _guard: std::sync::MutexGuard<'static, ()>,
        temp: std::path::PathBuf,
        previous_home: Option<std::ffi::OsString>,
        previous_browser: Option<std::ffi::OsString>,
    }

    impl Home {
        fn new(name: &str, script: &str) -> Self {
            let guard = crate::translate::horon_agent_home_test_lock();
            let temp = std::env::temp_dir().join(format!(
                "horon-agent-mirror-{name}-{}-{}",
                std::process::id(),
                SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
            ));
            let browser_dir = temp.join("browser");
            std::fs::create_dir_all(&browser_dir).unwrap();
            let bin = browser_dir.join("browser");
            std::fs::write(&bin, script).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            let previous_home = std::env::var_os("HORON_AGENT_HOME");
            let previous_browser = std::env::var_os("HORON_AGENT_BROWSER");
            // SAFETY: the home lock serializes every test that mutates these
            // process-global variables.
            unsafe {
                std::env::set_var("HORON_AGENT_HOME", &temp);
                std::env::set_var("HORON_AGENT_BROWSER", "chrome");
            }
            Self {
                _guard: guard,
                temp,
                previous_home,
                previous_browser,
            }
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            // SAFETY: the home lock is still held through `_guard`.
            unsafe {
                match &self.previous_home {
                    Some(value) => std::env::set_var("HORON_AGENT_HOME", value),
                    None => std::env::remove_var("HORON_AGENT_HOME"),
                }
                match &self.previous_browser {
                    Some(value) => std::env::set_var("HORON_AGENT_BROWSER", value),
                    None => std::env::remove_var("HORON_AGENT_BROWSER"),
                }
            }
            std::fs::remove_dir_all(&self.temp).unwrap();
        }
    }

    /// A stub CLI that answers `listTabs` the way the bridge does.
    const LISTING_CLI: &str = r#"#!/bin/sh
case "$1" in
  listTabs)
    printf '%s\n' '{"windows":[{"windowId":1,"tabs":[{"tabId":7,"url":"https://stub.example/","title":"Stub"},{"tabId":9,"url":"https://stub.example/other","title":"Other"}]}]}'
    ;;
  getActiveTab)
    printf '%s\n' '{"tabId":7,"url":"https://stub.example/","title":"Stub","windowId":1}'
    ;;
  *)
    printf '%s\n' '{"ok":true}'
    ;;
esac
"#;

    #[test]
    fn the_real_cli_path_parses_a_listing_into_state() {
        let _home = Home::new("listing", LISTING_CLI);
        let frame = run(
            &CliBridge::new(),
            &request("session_mirror_cli_listing", "state", json!({})),
        );

        assert_eq!(frame["ev"], "browser_mirror_result", "{frame}");
        let state = &frame["state"];
        assert_eq!(state["tabs"].as_array().unwrap().len(), 2);
        assert_eq!(state["tabs"][0]["id"], 7);
        assert_eq!(state["tabs"][1]["url"], "https://stub.example/other");
        assert_eq!(state["mode"], "agent");
        // No session daemon runs for this stub, so nothing is bound and the
        // active tab is honestly absent rather than invented.
        assert_eq!(state["connected"], false);
    }

    #[test]
    fn the_real_cli_path_reads_a_screenshot_file() {
        let temp = std::env::temp_dir().join(format!("horon-agent-mirror-shot-{}", std::process::id()));
        std::fs::create_dir_all(&temp).unwrap();
        let saved = temp.join("shot.png");
        std::fs::write(&saved, solid_png(40, 30)).unwrap();
        let stdout = format!("{{\"saved\":\"{}\"}}\n", saved.display());

        match read_screenshot(stdout.as_bytes(), &temp.join("unused.png")) {
            CallOutcome::Image(bytes) => assert!(!bytes.is_empty()),
            CallOutcome::Json(value) => panic!("expected image bytes, got JSON {value}"),
            CallOutcome::Unavailable(message) => panic!("expected image bytes, got unavailable: {message}"),
            CallOutcome::Timeout => panic!("expected image bytes, got a timeout"),
            CallOutcome::Failed(message) => panic!("expected image bytes, got a failure: {message}"),
        }
        assert!(!saved.exists(), "the screenshot is removed after it is read");
        std::fs::remove_dir_all(&temp).unwrap();
    }

    #[test]
    fn a_hanging_cli_is_killed_at_the_deadline() {
        let _home = Home::new("hang", "#!/bin/sh\nexec sleep 600\n");
        let started = Instant::now();
        let frame = run(
            &CliBridge::new(),
            &request("session_mirror_cli_hang", "state", json!({})),
        );

        assert_eq!(frame["ev"], "error", "{frame}");
        assert_eq!(frame["code"], "browser_unavailable");
        assert!(
            started.elapsed() < STATE_TIMEOUT + Duration::from_secs(3),
            "a hanging CLI must be killed at its deadline, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_failing_cli_reports_its_own_message() {
        let _home = Home::new(
            "fail",
            "#!/bin/sh\nprintf 'Error: browser bridge is not connected\\n' >&2\nexit 1\n",
        );
        let frame = run(
            &CliBridge::new(),
            &request("session_mirror_cli_fail", "state", json!({})),
        );

        assert_eq!(frame["code"], "browser_unavailable");
        let message = frame["message"].as_str().unwrap();
        assert!(message.contains("browser bridge is not connected"), "{message}");
        assert!(message.contains("chrome://extensions"), "{message}");
    }
}
