//! `browser_mirror`: the agent's own browser, watchable and driveable from a
//! client panel.
//!
//! The browser automation already exists as the agent's `browser` tool. This
//! module is a thin harness-API surface over the same machinery: the same
//! bridge CLI (`~/.horon-agent/browser/browser`), the same action names the
//! tool maps to (`listTabs`, `getActiveTab`, `navigate`, `screenshot`,
//! `evaluate`, ...), and the same per-session daemon
//! (`horon_agent_base::browser::session_name_for`). Because both callers bind
//! to that one daemon name, the mirror shows the tab the agent is driving
//! instead of opening a second browser of its own.
//!
//! Why the bridge process runs the CLI: the legacy protocol has no browser
//! request, and a mirror must keep working while the agent is idle, so there
//! is no daemon-side path to borrow. Everything else is borrowed: readiness
//! guidance, daemon naming, the stale-window recovery, and the action
//! vocabulary all come from `horon-agent-base`.
//!
//! Bounds are part of the contract, not a nicety:
//!
//! - Every CLI call has a deadline. A call that overruns is killed and
//!   reported as `browser_unavailable`; nothing here can hang a connection.
//! - `state` never starts a daemon, so polling it cannot open a window, and it
//!   answers within a few seconds (two calls, two seconds each).
//! - `frame` never starts a daemon either: a one-second frame poll must not
//!   spawn browsers. Mutations may start one, which the CLI itself caps at 10s.
//! - A frame is at most 512 KiB of base64. A screenshot that cannot be encoded
//!   inside that is refused with the limit in the message, never truncated.

use super::Outbound;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use horon_agent_base::browser::{
    apply_bridge_browser_env, bridge_browser_name, browser_binary_path, browser_session_running,
    ensure_browser_session_for, is_stale_session_target_error, reset_browser_session_for, resolve_target_browser,
    session_name_for,
};
use horon_agent_base::browser_detect::BrowserDetection;
use horon_agent_harness_api::{
    ApiEvent, ApiRequest, BrowserFrame, BrowserMirrorError, BrowserState, BrowserTabState, ErrorCode, ServerFrame,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Largest `frame.data` this surface will return, in base64 bytes.
const MAX_FRAME_BASE64: usize = 512 * 1024;

/// Deadline for each of the two calls a `state` makes. Two of these plus the
/// session check is the most a poll can cost, which keeps the documented
/// "a few seconds" true even when the browser is wedged.
const STATE_TIMEOUT: Duration = Duration::from_secs(2);

/// Deadline for a screenshot. A screenshot of a loaded page is fast; a bridge
/// that needs longer than this is not answering.
const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// Deadline for one mutating action (navigation, input, tab control).
const ACTION_TIMEOUT: Duration = Duration::from_secs(5);

/// JPEG qualities tried in order until the frame fits the base64 budget.
/// Screenshots arrive as PNG, which is several times larger than a usable JPEG;
/// the ladder keeps a full-window capture inside the budget at the cost of
/// fidelity, and a frame that cannot fit is refused rather than truncated.
const JPEG_QUALITIES: [u8; 4] = [70, 50, 35, 25];

/// How long a `takeover` survives without the panel that took over touching it.
///
/// The mode is a lease rather than a flag: a panel polls `state` while it is
/// visible, which refreshes the lease, while a connection that died stops
/// refreshing it. That is what keeps a dropped connection from leaving a
/// takeover behind forever. The daemon applies the same lease to what it was
/// pushed, so a vanished panel releases the browser tool too.
const MODE_LEASE: Duration = Duration::from_secs(60);

/// How often a live takeover re-announces itself to the daemon.
///
/// The daemon's copy expires on the same lease, so renewing at half of it keeps
/// the gate up while a panel polls, without opening a socket on every poll.
const PUSH_RENEW: Duration = Duration::from_secs(30);

/// Deadline for one push. A takeover must never hang the panel on a wedged
/// daemon; the daemon's own lease expires a stale mode anyway.
const PUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// Who is driving the mirrored browser.
///
/// The bridge is the authority: `takeover` sets `User`, `resume` sets `Agent`,
/// and every change is pushed to the daemon, whose browser tool refuses to
/// drive while the cached copy says `User`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Agent,
    User,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Mode::Agent => "agent",
            Mode::User => "user",
        }
    }
}

struct ModeEntry {
    mode: Mode,
    touched: Instant,
    /// When the daemon last heard this mode. `None` means it has not, so the
    /// next poll pushes instead of waiting out a renewal interval.
    pushed_at: Option<Instant>,
}

static MODES: LazyLock<Mutex<HashMap<String, ModeEntry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn modes() -> std::sync::MutexGuard<'static, HashMap<String, ModeEntry>> {
    MODES.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Refresh an existing lease for this session and report the mode the daemon
/// needs to hear about again. Absent entries stay absent.
fn touch(session_id: &str) -> Option<Mode> {
    let mut modes = modes();
    match modes.get_mut(session_id) {
        Some(entry) if entry.touched.elapsed() <= MODE_LEASE => {
            entry.touched = Instant::now();
            let renew = entry.mode == Mode::User && entry.pushed_at.is_none_or(|at| at.elapsed() >= PUSH_RENEW);
            if renew {
                entry.pushed_at = Some(Instant::now());
                return Some(entry.mode);
            }
            None
        }
        Some(_) => {
            modes.remove(session_id);
            None
        }
        None => None,
    }
}

fn current_mode(session_id: &str) -> Mode {
    let mut modes = modes();
    match modes.get(session_id) {
        Some(entry) if entry.touched.elapsed() <= MODE_LEASE => entry.mode,
        Some(_) => {
            modes.remove(session_id);
            Mode::Agent
        }
        None => Mode::Agent,
    }
}

fn set_mode(session_id: &str, mode: Mode) {
    let mut modes = modes();
    if mode == Mode::Agent {
        modes.remove(session_id);
    } else {
        let now = Instant::now();
        modes.insert(
            session_id.to_string(),
            ModeEntry {
                mode,
                touched: now,
                pushed_at: Some(now),
            },
        );
    }
}

/// Announces a session's driver mode to the daemon.
///
/// The daemon caches it and gates its own browser tool on the cached copy, so
/// the tool path never calls back here. A push that cannot reach the daemon
/// leaves the daemon on its permissive default, which is the safe way to fail.
trait ModePush: Send + Sync {
    fn push(&self, session_id: &str, mode: Mode);
}

struct SocketPush {
    socket: Option<PathBuf>,
}

impl ModePush for SocketPush {
    fn push(&self, session_id: &str, mode: Mode) {
        if let Some(socket) = &self.socket {
            push_browser_mode(socket, session_id, mode);
        }
    }
}

/// One-shot push over a fresh daemon connection.
///
/// A dedicated connection rather than the client's attached stream: a daemon
/// that does not know this request answers with an error, and that error must
/// not surface as a session error on a desktop panel. Such a daemon answers
/// `invalid request`; the reply is read and discarded, so an old daemon simply
/// keeps its permissive default.
#[cfg(unix)]
fn push_browser_mode(socket: &Path, session_id: &str, mode: Mode) {
    use std::io::{BufRead, BufReader, Write};
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(socket) else {
        return;
    };
    let _ = stream.set_write_timeout(Some(PUSH_TIMEOUT));
    let _ = stream.set_read_timeout(Some(PUSH_TIMEOUT));
    let frame = json!({
        "type": "browser_mode",
        "id": 0,
        "session_id": session_id,
        "mode": mode.as_str(),
    });
    if stream.write_all(format!("{frame}\n").as_bytes()).is_err() {
        return;
    }
    let mut reply = String::new();
    let _ = BufReader::new(stream).read_line(&mut reply);
}

#[cfg(not(unix))]
fn push_browser_mode(_socket: &Path, _session_id: &str, _mode: Mode) {
    // The API bridge is Unix-only, so no other platform reaches a takeover.
}

/// What the mirror should tell a user about the browser it targets.
pub(super) struct BrowserTarget {
    name: &'static str,
    extensions_page: &'static str,
}

/// The browser bridge as the mirror needs it: raw facts, no policy.
///
/// Kept as a trait so the mirror's decisions (timeouts, refusals, the stale-tab
/// rule) are testable without a browser, while the production implementation
/// stays a thin wrapper over `horon-agent-base`.
pub(super) trait BrowserBridge {
    /// Which browser this mirror targets, for guidance messages.
    fn target(&self) -> Option<BrowserTarget>;

    /// Why no call can succeed at all: no browser resolved, or no bridge CLI
    /// installed. `None` means calls are worth attempting.
    ///
    /// Separate from a call's own `Unavailable`, because the actions that need
    /// an existing window would otherwise report "no window yet" to a user who
    /// has not installed the bridge.
    fn install_problem(&self) -> Option<String>;

    /// The session daemon name, starting one only when `start` is set.
    fn session(&self, session_id: &str, start: bool) -> Option<String>;

    /// Restart the session daemon after its bound window or tab disappeared.
    fn reset_session(&self, session_id: &str);

    /// One bridge CLI call.
    fn call(&self, action: &str, params: Value, session: Option<&str>, timeout: Duration) -> CallOutcome;
}

/// The outcome of one bridge CLI call, before the mirror interprets it.
pub(super) enum CallOutcome {
    /// The CLI answered with JSON.
    Json(Value),
    /// The CLI wrote an image file, which was read and removed.
    Image(Vec<u8>),
    /// There is nothing to call: no browser to target, or no CLI installed.
    Unavailable(String),
    /// The CLI was killed at its deadline.
    Timeout,
    /// The CLI, bridge, or extension reported an error.
    Failed(String),
}

/// A mirror call that cannot produce a result at all.
#[derive(Debug)]
enum Failure {
    /// Answered with an `Error` frame and `ErrorCode::BrowserUnavailable`.
    Unavailable(String),
    /// Answered inside the result frame, because the browser did answer.
    Refused { code: &'static str, message: String },
}

#[derive(Default)]
struct Outcome {
    state: Option<BrowserState>,
    frame: Option<BrowserFrame>,
}

impl Outcome {
    fn state(state: BrowserState) -> Self {
        Self {
            state: Some(state),
            frame: None,
        }
    }

    fn frame(frame: BrowserFrame) -> Self {
        Self {
            state: None,
            frame: Some(frame),
        }
    }
}

pub(super) fn handle(api_id: u64, request: &Value, socket: Option<&Path>) -> Vec<Outbound> {
    handle_with(
        api_id,
        request,
        &CliBridge::new(),
        &SocketPush {
            socket: socket.map(Path::to_path_buf),
        },
    )
}

fn handle_with(api_id: u64, request: &Value, bridge: &dyn BrowserBridge, push: &dyn ModePush) -> Vec<Outbound> {
    let parsed = serde_json::from_value::<ApiRequest>(request.clone());
    let Ok(ApiRequest::BrowserMirror {
        session_id,
        action,
        args,
    }) = parsed
    else {
        return error_reply(
            api_id,
            ErrorCode::InvalidRequest,
            "browser_mirror needs a session_id and an action",
        );
    };
    if !valid_session_id(&session_id) {
        return error_reply(
            api_id,
            ErrorCode::InvalidRequest,
            "session_id must be a non-empty session id of at most 64 characters (letters, digits, `-`, `_`)",
        );
    }
    let args = match args {
        Some(Value::Object(map)) => Value::Object(map),
        None => json!({}),
        Some(_) => {
            return error_reply(
                api_id,
                ErrorCode::InvalidRequest,
                "browser_mirror args must be a JSON object",
            );
        }
    };
    // Every request from this session keeps a live takeover alive; a panel that
    // closed stopped sending them, and its lease expires on its own. A lease
    // that was refreshed also renews the daemon's copy before it expires.
    // `takeover` and `resume` push their own new mode below, so renewing first
    // would announce the mode they are about to replace.
    if !matches!(action.as_str(), "takeover" | "resume")
        && let Some(mode) = touch(&session_id)
    {
        push.push(&session_id, mode);
    }

    match dispatch(&session_id, &action, &args, bridge, push) {
        Ok(outcome) => vec![Outbound::Reply(ServerFrame::reply(
            api_id,
            ApiEvent::BrowserMirrorResult {
                session_id,
                action,
                state: outcome.state,
                frame: outcome.frame,
                error: None,
            },
        ))],
        Err(Failure::Unavailable(message)) => error_reply(api_id, ErrorCode::BrowserUnavailable, &message),
        Err(Failure::Refused { code, message }) => vec![Outbound::Reply(ServerFrame::reply(
            api_id,
            ApiEvent::BrowserMirrorResult {
                session_id,
                action,
                state: None,
                frame: None,
                error: Some(BrowserMirrorError {
                    code: code.to_string(),
                    message,
                }),
            },
        ))],
    }
}

fn dispatch(
    session_id: &str,
    action: &str,
    args: &Value,
    bridge: &dyn BrowserBridge,
    push: &dyn ModePush,
) -> Result<Outcome, Failure> {
    match action {
        "state" => Ok(Outcome::state(state(session_id, bridge)?)),
        "frame" => Ok(Outcome::frame(frame(session_id, args, bridge)?)),
        // A takeover only becomes real once the browser answered: reporting a
        // mode for a browser that is not there would be a lie the next poll
        // would have to walk back.
        "takeover" => {
            let mut snapshot = state(session_id, bridge)?;
            set_mode(session_id, Mode::User);
            push.push(session_id, Mode::User);
            snapshot.mode = Mode::User.as_str().to_string();
            Ok(Outcome::state(snapshot))
        }
        "resume" => {
            let mut snapshot = state(session_id, bridge)?;
            set_mode(session_id, Mode::Agent);
            push.push(session_id, Mode::Agent);
            snapshot.mode = Mode::Agent.as_str().to_string();
            Ok(Outcome::state(snapshot))
        }
        "navigate" | "back" | "forward" | "reload" | "new_tab" | "select_tab" | "close_tab" | "input" => {
            act(session_id, action, args, bridge)?;
            Ok(Outcome::default())
        }
        other => Err(Failure::Refused {
            code: "unsupported_action",
            message: format!(
                "unsupported browser_mirror action `{other}`; supported actions: state, frame, navigate, back, \
                 forward, reload, new_tab, select_tab, close_tab, input, takeover, resume"
            ),
        }),
    }
}

fn state(session_id: &str, bridge: &dyn BrowserBridge) -> Result<BrowserState, Failure> {
    // Listing tabs runs without the session daemon on purpose, exactly like the
    // browser tool's `list_tabs`: a scoped listing only shows the agent's own
    // window and hides the tabs the user already has open and signed in.
    let listing = match bridge.call("listTabs", json!({}), None, STATE_TIMEOUT) {
        CallOutcome::Json(value) => value,
        CallOutcome::Image(_) => {
            return Err(Failure::Refused {
                code: "bridge_error",
                message: "listTabs returned an image".to_string(),
            });
        }
        CallOutcome::Unavailable(message) => return Err(Failure::Unavailable(message)),
        CallOutcome::Timeout => {
            return Err(Failure::Unavailable(not_responding(
                bridge.target().as_ref(),
                STATE_TIMEOUT,
            )));
        }
        CallOutcome::Failed(details) => return Err(Failure::Unavailable(unreachable(bridge, &details))),
    };
    let mut tabs = parse_tabs(&listing);

    // Never start a daemon here: `state` is polled once a second while a panel
    // is visible, and a poll that opens a browser window is a bug.
    let active = match bridge.session(session_id, false) {
        Some(name) => match bridge.call("getActiveTab", json!({}), Some(&name), STATE_TIMEOUT) {
            CallOutcome::Json(value) => parse_tab(&value),
            // A running daemon whose window or tab is gone is not a reason to
            // fail the whole snapshot; it is exactly what `connected: false`
            // reports, and a mutation recovers it.
            _ => None,
        },
        None => None,
    };
    let active_tab_id = active.as_ref().map(|tab| tab.id);
    if let Some(id) = active_tab_id {
        for tab in &mut tabs {
            tab.active = tab.id == id;
        }
    }
    let (url, title) = match &active {
        Some(tab) => (tab.url.clone(), tab.title.clone()),
        None => (String::new(), String::new()),
    };
    Ok(BrowserState {
        connected: active.is_some(),
        tabs,
        active_tab_id,
        url,
        title,
        mode: current_mode(session_id).as_str().to_string(),
    })
}

fn frame(session_id: &str, args: &Value, bridge: &dyn BrowserBridge) -> Result<BrowserFrame, Failure> {
    if let Some(problem) = bridge.install_problem() {
        return Err(Failure::Unavailable(problem));
    }
    let Some(session) = bridge.session(session_id, false) else {
        return Err(Failure::Unavailable(no_window_message()));
    };
    let explicit_tab = tab_id(args);
    let mut params = json!({});
    if let Some(id) = explicit_tab {
        params["tabId"] = json!(id);
    }
    let bytes = match bridge.call("screenshot", params, Some(&session), FRAME_TIMEOUT) {
        CallOutcome::Image(bytes) => bytes,
        CallOutcome::Json(value) => {
            return Err(Failure::Refused {
                code: "bridge_error",
                message: format!("the browser bridge returned no screenshot: {value}"),
            });
        }
        CallOutcome::Unavailable(message) => return Err(Failure::Unavailable(message)),
        CallOutcome::Timeout => {
            return Err(Failure::Unavailable(not_responding(
                bridge.target().as_ref(),
                FRAME_TIMEOUT,
            )));
        }
        CallOutcome::Failed(details) => return Err(classify(&details, explicit_tab.is_some())),
    };
    encode_frame(&bytes)
}

fn act(session_id: &str, action: &str, args: &Value, bridge: &dyn BrowserBridge) -> Result<(), Failure> {
    // Opening a tab or navigating may create the agent's window; every other
    // action needs the window that already exists, and clicking into a browser
    // that is not there is not a thing a panel can mean.
    let may_start = matches!(action, "navigate" | "new_tab");
    if let Some(problem) = bridge.install_problem() {
        return Err(Failure::Unavailable(problem));
    }
    let Some(session) = bridge.session(session_id, may_start) else {
        return Err(Failure::Unavailable(if may_start {
            format!(
                "the browser bridge could not open a browser window for this session. Start the browser, check that \
                 the Browser Agent Bridge extension is enabled, and retry. {}",
                install_hint(bridge.target().as_ref())
            )
        } else {
            no_window_message()
        }));
    };
    let explicit_tab = tab_id(args);
    let call = plan(action, args, &session, bridge)?;

    let mut outcome = bridge.call(call.action, call.params.clone(), Some(&session), ACTION_TIMEOUT);
    // The user may have closed the agent's window. The tool recovers exactly
    // this way: restart the session daemon once and retry. An explicit tab id is
    // never retried, because a closed user tab is not a session problem, and
    // silently acting on a different tab is worse than refusing.
    if explicit_tab.is_none()
        && let CallOutcome::Failed(details) = &outcome
        && is_stale_session_target_error(details)
    {
        bridge.reset_session(session_id);
        if let Some(restarted) = bridge.session(session_id, true) {
            outcome = bridge.call(call.action, call.params, Some(&restarted), ACTION_TIMEOUT);
        }
    }

    match outcome {
        CallOutcome::Json(_) => Ok(()),
        CallOutcome::Image(_) => Err(Failure::Refused {
            code: "bridge_error",
            message: format!("`{action}` returned an image"),
        }),
        CallOutcome::Unavailable(message) => Err(Failure::Unavailable(message)),
        CallOutcome::Timeout => Err(Failure::Unavailable(not_responding(
            bridge.target().as_ref(),
            ACTION_TIMEOUT,
        ))),
        CallOutcome::Failed(details) => Err(classify(&details, explicit_tab.is_some())),
    }
}

struct PlannedCall {
    action: &'static str,
    params: Value,
}

fn plan(action: &str, args: &Value, session: &str, bridge: &dyn BrowserBridge) -> Result<PlannedCall, Failure> {
    let mut params = json!({});
    if let Some(id) = tab_id(args) {
        params["tabId"] = json!(id);
    }
    match action {
        "navigate" => {
            let Some(url) = string_arg(args, "url") else {
                return Err(invalid_args("navigate needs args.url"));
            };
            params["url"] = json!(url);
            // A mirror must not block its poll loop on a page that keeps a
            // request open; the panel re-reads `state` and `frame` anyway.
            params["wait"] = json!(bool_arg(args, "wait").unwrap_or(false));
            Ok(PlannedCall {
                action: "navigate",
                params,
            })
        }
        "back" => Ok(evaluate("history.back(); return location.href;", params)),
        "forward" => Ok(evaluate("history.forward(); return location.href;", params)),
        "reload" => {
            // A script-driven `location.reload()` tears down the context the
            // reply travels through, so a successful reload would report a
            // failure. Re-navigating the active tab's own URL is observable.
            let Some(url) = active_url(session, bridge)? else {
                return Err(Failure::Refused {
                    code: "no_active_tab",
                    message: "reload needs an active tab; open one with `new_tab`".to_string(),
                });
            };
            params["url"] = json!(url);
            params["wait"] = json!(false);
            Ok(PlannedCall {
                action: "navigate",
                params,
            })
        }
        "new_tab" => {
            if let Some(url) = string_arg(args, "url") {
                params["url"] = json!(url);
            }
            Ok(PlannedCall {
                action: "newSession",
                params,
            })
        }
        "select_tab" => {
            if tab_id(args).is_none() {
                return Err(invalid_args("select_tab needs args.tab_id"));
            }
            if let Some(focus) = bool_arg(args, "focus") {
                params["focus"] = json!(focus);
            }
            Ok(PlannedCall {
                action: "setActiveTab",
                params,
            })
        }
        "close_tab" => {
            if tab_id(args).is_none() {
                return Err(invalid_args("close_tab needs args.tab_id"));
            }
            Ok(PlannedCall {
                action: "closeTab",
                params,
            })
        }
        "input" => plan_input(args, params),
        other => Err(Failure::Refused {
            code: "unsupported_action",
            message: format!("unsupported browser_mirror action `{other}`"),
        }),
    }
}

fn plan_input(args: &Value, params: Value) -> Result<PlannedCall, Failure> {
    let Some(kind) = string_arg(args, "kind") else {
        return Err(invalid_args(
            "input needs args.kind: click, scroll, key, text, or paste",
        ));
    };
    match kind {
        "click" => {
            let mut params = params;
            if let Some(x) = number_arg(args, "x") {
                params["x"] = json!(x);
            }
            if let Some(y) = number_arg(args, "y") {
                params["y"] = json!(y);
            }
            if let Some(selector) = string_arg(args, "selector") {
                params["selector"] = json!(selector);
            }
            if let Some(text) = string_arg(args, "text") {
                params["text"] = json!(text);
            }
            let aimed = params
                .as_object()
                .is_some_and(|map| map.contains_key("x") || map.contains_key("selector") || map.contains_key("text"));
            if !aimed {
                return Err(invalid_args("input kind `click` needs x/y, a selector, or text"));
            }
            Ok(PlannedCall {
                action: "click",
                params,
            })
        }
        "scroll" => {
            let x = number_arg(args, "x").unwrap_or(0.0);
            let y = number_arg(args, "y").unwrap_or(0.0);
            match string_arg(args, "selector") {
                // Scrolling a container is the tool's own evaluate fallback: the
                // bridge's selector scroll means scrollIntoView, not a delta.
                Some(selector) if x != 0.0 || y != 0.0 => {
                    let script = scroll_script(selector, x, y)?;
                    Ok(evaluate(&script, params))
                }
                Some(selector) => {
                    let mut params = params;
                    params["selector"] = json!(selector);
                    Ok(PlannedCall {
                        action: "scroll",
                        params,
                    })
                }
                None if x != 0.0 || y != 0.0 => {
                    let mut params = params;
                    params["x"] = json!(x);
                    params["y"] = json!(y);
                    Ok(PlannedCall {
                        action: "scroll",
                        params,
                    })
                }
                None => Err(invalid_args("input kind `scroll` needs x/y deltas or a selector")),
            }
        }
        "key" => {
            let Some(key) = string_arg(args, "key") else {
                return Err(invalid_args("input kind `key` needs args.key"));
            };
            let script = press_script(key, string_arg(args, "selector"))?;
            Ok(evaluate(&script, params))
        }
        "text" => {
            let Some(text) = string_arg(args, "text") else {
                return Err(invalid_args("input kind `text` needs args.text"));
            };
            let mut params = params;
            params["text"] = json!(text);
            if let Some(selector) = string_arg(args, "selector") {
                params["selector"] = json!(selector);
            }
            if let Some(clear) = bool_arg(args, "clear") {
                params["clear"] = json!(clear);
            }
            Ok(PlannedCall { action: "type", params })
        }
        "paste" => {
            let Some(text) = string_arg(args, "text") else {
                return Err(invalid_args("input kind `paste` needs args.text"));
            };
            let script = paste_script(text, string_arg(args, "selector"))?;
            Ok(evaluate(&script, params))
        }
        other => Err(invalid_args(&format!(
            "unsupported input kind `{other}`; use click, scroll, key, text, or paste"
        ))),
    }
}

/// An `evaluate` call, the tool's escape hatch for everything the bridge does
/// not name directly. `pageWorld` matches what the tool's `press` uses.
fn evaluate(script: &str, mut params: Value) -> PlannedCall {
    params["script"] = json!(script);
    params["pageWorld"] = json!(true);
    PlannedCall {
        action: "evaluate",
        params,
    }
}

fn active_url(session: &str, bridge: &dyn BrowserBridge) -> Result<Option<String>, Failure> {
    match bridge.call("getActiveTab", json!({}), Some(session), STATE_TIMEOUT) {
        CallOutcome::Json(value) => Ok(parse_tab(&value).map(|tab| tab.url)),
        CallOutcome::Unavailable(message) => Err(Failure::Unavailable(message)),
        CallOutcome::Timeout => Err(Failure::Unavailable(not_responding(
            bridge.target().as_ref(),
            STATE_TIMEOUT,
        ))),
        CallOutcome::Failed(details) => Err(classify(&details, false)),
        CallOutcome::Image(_) => Err(Failure::Refused {
            code: "bridge_error",
            message: "getActiveTab returned an image".to_string(),
        }),
    }
}

/// The key-press script the browser tool builds for `press`.
///
/// Kept in step with `crates/app-core/src/tool/browser_request.rs`: the bridge
/// has no `press` action, so both callers synthesize the same events.
fn press_script(key: &str, selector: Option<&str>) -> Result<String, Failure> {
    let selector_expr = match selector {
        Some(selector) => format!("document.querySelector({})", js_string(selector)?),
        None => "null".to_string(),
    };
    Ok(format!(
        r#"return (() => {{
  const target = {selector_expr} || document.activeElement || document.body;
  if (!target) throw new Error('No target available for key press');
  if (typeof target.focus === 'function') target.focus();
  const key = {key_literal};
  const eventInit = {{ key, bubbles: true, cancelable: true }};
  target.dispatchEvent(new KeyboardEvent('keydown', eventInit));
  target.dispatchEvent(new KeyboardEvent('keypress', eventInit));
  if (key === 'Enter' && target.form && typeof target.form.submit === 'function') {{
    target.form.submit();
  }}
  target.dispatchEvent(new KeyboardEvent('keyup', eventInit));
  return {{ pressed: true, key, tag: target.tagName || null }};
}})();"#,
        key_literal = js_string(key)?,
    ))
}

/// Insert text without per-character key events, the way a paste does.
fn paste_script(text: &str, selector: Option<&str>) -> Result<String, Failure> {
    let selector_expr = match selector {
        Some(selector) => format!("document.querySelector({})", js_string(selector)?),
        None => "null".to_string(),
    };
    Ok(format!(
        r#"return (() => {{
  const target = {selector_expr} || document.activeElement || document.body;
  if (!target) throw new Error('No target available for paste');
  if (typeof target.focus === 'function') target.focus();
  const text = {text_literal};
  let inserted = false;
  if (typeof document.execCommand === 'function') {{
    inserted = document.execCommand('insertText', false, text);
  }}
  if (!inserted && 'value' in target) {{
    const start = typeof target.selectionStart === 'number' ? target.selectionStart : target.value.length;
    const end = typeof target.selectionEnd === 'number' ? target.selectionEnd : start;
    target.value = target.value.slice(0, start) + text + target.value.slice(end);
    target.dispatchEvent(new Event('input', {{ bubbles: true }}));
    inserted = true;
  }}
  return {{ pasted: inserted, length: text.length }};
}})();"#,
        text_literal = js_string(text)?,
    ))
}

/// The tool's container-scroll fallback: the bridge's `scroll` with a selector
/// means scrollIntoView, which is not a delta.
fn scroll_script(selector: &str, x: f64, y: f64) -> Result<String, Failure> {
    Ok(format!(
        "const element=document.querySelector({}); if(!element) throw new Error('Scroll container not found'); \
         element.scrollBy({{left:{x},top:{y},behavior:'instant'}}); \
         return {{scrolled:true,x:element.scrollLeft,y:element.scrollTop}};",
        js_string(selector)?,
    ))
}

/// A JSON string literal, safe to splice into a script.
fn js_string(value: &str) -> Result<String, Failure> {
    serde_json::to_string(value).map_err(|error| Failure::Refused {
        code: "invalid_args",
        message: format!("could not encode a script argument: {error}"),
    })
}

fn parse_tabs(value: &Value) -> Vec<BrowserTabState> {
    let mut tabs = Vec::new();
    if let Some(windows) = value.get("windows").and_then(Value::as_array) {
        for window in windows {
            if let Some(window_tabs) = window.get("tabs").and_then(Value::as_array) {
                for tab in window_tabs {
                    if let Some(tab) = parse_tab(tab) {
                        tabs.push(tab);
                    }
                }
            }
        }
    } else if let Some(listed) = value.get("tabs").and_then(Value::as_array) {
        for tab in listed {
            if let Some(tab) = parse_tab(tab) {
                tabs.push(tab);
            }
        }
    }
    tabs
}

fn parse_tab(value: &Value) -> Option<BrowserTabState> {
    let id = value.get("tabId").or_else(|| value.get("id")).and_then(Value::as_i64)?;
    Some(BrowserTabState {
        id,
        url: text_field(value.get("url")),
        title: text_field(value.get("title")),
        active: value.get("active").and_then(Value::as_bool).unwrap_or(false),
    })
}

fn text_field(value: Option<&Value>) -> String {
    match value.and_then(Value::as_str) {
        Some(text) => text.to_string(),
        None => String::new(),
    }
}

/// Encode a screenshot as JPEG inside the base64 budget.
fn encode_frame(raw: &[u8]) -> Result<BrowserFrame, Failure> {
    let image = match image::load_from_memory(raw) {
        Ok(image) => image,
        Err(error) => {
            return Err(Failure::Refused {
                code: "bridge_error",
                message: format!("the screenshot was not a decodable image: {error}"),
            });
        }
    };
    let mut smallest_base64 = 0usize;
    let mut smallest_quality = 0u8;
    for quality in JPEG_QUALITIES {
        let mut encoded = Vec::new();
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, quality);
        if let Err(error) = encoder.encode_image(&image) {
            return Err(Failure::Refused {
                code: "bridge_error",
                message: format!("could not encode the frame as JPEG: {error}"),
            });
        }
        // Base64 length without padding differences: four characters per three
        // bytes, rounded up.
        let base64_len = encoded.len().div_ceil(3) * 4;
        if base64_len <= MAX_FRAME_BASE64 {
            return Ok(BrowserFrame {
                media_type: "image/jpeg".to_string(),
                data: STANDARD.encode(&encoded),
                bytes: encoded.len() as u64,
            });
        }
        smallest_base64 = base64_len;
        smallest_quality = quality;
    }
    Err(Failure::Refused {
        code: "frame_too_large",
        message: format!(
            "the frame is {smallest_base64} bytes of base64 even at JPEG quality {smallest_quality}, over the \
             {MAX_FRAME_BASE64}-byte (512 KB) limit; it was refused rather than truncated. Reduce the browser \
             window size, or capture a smaller area."
        ),
    })
}

fn classify(details: &str, explicit_tab: bool) -> Failure {
    let lower = details.to_ascii_lowercase();
    if lower.contains("no tab with id") {
        return Failure::Refused {
            code: "stale_tab",
            message: if explicit_tab {
                format!("that tab no longer exists ({details}); re-read `state` and target an open tab")
            } else {
                format!("the agent's tab no longer exists ({details}); re-read `state` before acting")
            },
        };
    }
    if details.contains("Unknown action:") {
        return Failure::Refused {
            code: "bridge_error",
            message: format!(
                "the installed browser extension does not support this action: {details}. Update it with \
                 `horon-agent browser setup`, then retry."
            ),
        };
    }
    Failure::Refused {
        code: "bridge_error",
        message: details.to_string(),
    }
}

fn invalid_args(message: &str) -> Failure {
    Failure::Refused {
        code: "invalid_args",
        message: message.to_string(),
    }
}

fn no_window_message() -> String {
    "the agent has no browser window for this session yet; open one with `new_tab` (or let the agent run a browser \
     action), then retry"
        .to_string()
}

fn install_hint(target: Option<&BrowserTarget>) -> String {
    match target {
        Some(target) => format!(
            "Install it with `horon-agent browser setup`, then load the Browser Agent Bridge extension in {} ({}) and \
             keep the browser open.",
            target.name, target.extensions_page
        ),
        None => "Install a supported browser, or point HORON_AGENT_BROWSER at one, then run `horon-agent browser \
                 setup`."
            .to_string(),
    }
}

fn not_responding(target: Option<&BrowserTarget>, timeout: Duration) -> String {
    match target {
        Some(target) => format!(
            "the browser bridge did not answer within {}s. Start {}, then check that the Browser Agent Bridge \
             extension is installed and enabled ({}).",
            timeout.as_secs(),
            target.name,
            target.extensions_page
        ),
        None => format!(
            "the browser bridge did not answer within {}s. Start the browser, then check that the Browser Agent \
             Bridge extension is installed and enabled.",
            timeout.as_secs()
        ),
    }
}

fn unreachable(bridge: &dyn BrowserBridge, details: &str) -> String {
    match bridge.target() {
        Some(target) => format!(
            "the browser bridge is not reachable: {details}. Start {} and check that the Browser Agent Bridge \
             extension is enabled ({}).",
            target.name, target.extensions_page
        ),
        None => format!("the browser bridge is not reachable: {details}"),
    }
}

fn error_reply(api_id: u64, code: ErrorCode, message: &str) -> Vec<Outbound> {
    vec![Outbound::Reply(ServerFrame::reply(
        api_id,
        ApiEvent::Error {
            code,
            message: message.to_string(),
            details: None,
        },
    ))]
}

fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 64
        && session_id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
}

fn tab_id(args: &Value) -> Option<i64> {
    args.get("tab_id").and_then(Value::as_i64)
}

fn string_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn number_arg(args: &Value, key: &str) -> Option<f64> {
    args.get(key).and_then(Value::as_f64)
}

fn bool_arg(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(Value::as_bool)
}

/// The production bridge: `horon-agent-base` helpers plus the bridge CLI.
struct CliBridge {
    target: Result<BrowserDetection, String>,
}

impl CliBridge {
    fn new() -> Self {
        Self {
            target: match resolve_target_browser(None) {
                Ok(target) => Ok(target),
                Err(error) => Err(format!("{error:#}")),
            },
        }
    }

    fn browser(&self) -> Option<&'static str> {
        match &self.target {
            Ok(target) => Some(bridge_browser_name(target.kind)),
            Err(_) => None,
        }
    }

    fn unavailable_message(&self) -> String {
        match &self.target {
            Ok(target) => format!(
                "the horon-agent browser bridge is not installed for {}. Install it with `horon-agent browser setup`, \
                 then load the Browser Agent Bridge extension in {} ({}) and keep the browser open.",
                target.kind.display_name(),
                target.kind.display_name(),
                target.kind.extensions_page()
            ),
            Err(error) => format!(
                "no browser is available to mirror: {error}. Install a supported browser, or set \
                 HORON_AGENT_BROWSER, then run `horon-agent browser setup`."
            ),
        }
    }
}

impl BrowserBridge for CliBridge {
    fn target(&self) -> Option<BrowserTarget> {
        match &self.target {
            Ok(target) => Some(BrowserTarget {
                name: target.kind.display_name(),
                extensions_page: target.kind.extensions_page(),
            }),
            Err(_) => None,
        }
    }

    fn install_problem(&self) -> Option<String> {
        if self.target.is_err() || !browser_binary_path().exists() {
            return Some(self.unavailable_message());
        }
        None
    }

    fn session(&self, session_id: &str, start: bool) -> Option<String> {
        let browser = self.browser()?;
        if start {
            return ensure_browser_session_for(session_id, Some(browser));
        }
        if browser_session_running(session_id, Some(browser)) {
            Some(session_name_for(session_id, Some(browser)))
        } else {
            None
        }
    }

    fn reset_session(&self, session_id: &str) {
        reset_browser_session_for(session_id, self.browser());
    }

    fn call(&self, action: &str, params: Value, session: Option<&str>, timeout: Duration) -> CallOutcome {
        let Some(browser) = self.browser() else {
            return CallOutcome::Unavailable(self.unavailable_message());
        };
        let bin = browser_binary_path();
        if !bin.exists() {
            return CallOutcome::Unavailable(self.unavailable_message());
        }

        let mut params = params;
        // The tool asks for `filename` and reads `saved`; the CLI's direct mode
        // reads `path`. Send both, the way `upload` does, so either reader
        // finds the file it expects.
        let screenshot = if action == "screenshot" {
            let path = temp_screenshot_path();
            if let Some(map) = params.as_object_mut() {
                let rendered = path.to_string_lossy().to_string();
                map.insert("filename".into(), json!(rendered));
                map.insert("path".into(), json!(rendered));
            }
            Some(path)
        } else {
            None
        };

        let mut command = Command::new(&bin);
        command
            .arg(action)
            .arg(params.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_bridge_browser_env(&mut command, Some(browser));
        if let Some(name) = session {
            command.env("BROWSER_SESSION", name);
        }

        let output = match run_capped(command, timeout) {
            Ok(Some(output)) => output,
            Ok(None) => return CallOutcome::Timeout,
            Err(error) => return CallOutcome::Unavailable(error),
        };
        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let details = if stderr.trim().is_empty() {
                stdout.trim().to_string()
            } else if stdout.trim().is_empty() {
                stderr.trim().to_string()
            } else {
                format!("{}\n{}", stderr.trim(), stdout.trim())
            };
            return CallOutcome::Failed(details);
        }
        if let Some(path) = screenshot {
            return read_screenshot(&output.stdout, &path);
        }
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() {
            return CallOutcome::Json(json!({ "ok": true }));
        }
        match serde_json::from_str::<Value>(&text) {
            Ok(value) => CallOutcome::Json(value),
            Err(_) => CallOutcome::Json(json!({ "raw": text })),
        }
    }
}

struct CliOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Run the bridge CLI with a hard deadline, killing it when it overruns.
///
/// `Ok(None)` means the deadline passed. Pipes are drained on their own threads
/// so a large reply cannot fill the pipe buffer and deadlock the wait.
fn run_capped(mut command: Command, timeout: Duration) -> Result<Option<CliOutput>, String> {
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start the browser bridge: {error}"))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || read_all(stdout));
    let stderr_reader = std::thread::spawn(move || read_all(stderr));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() >= deadline => {
                if let Err(error) = child.kill() {
                    eprintln!("[browser mirror] could not kill the timed-out bridge: {error}");
                }
                if let Err(error) = child.wait() {
                    eprintln!("[browser mirror] could not reap the timed-out bridge: {error}");
                }
                break None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                eprintln!("[browser mirror] could not wait for the bridge: {error}");
                if let Err(kill_error) = child.kill() {
                    eprintln!("[browser mirror] could not kill the unresponsive bridge: {kill_error}");
                }
                break None;
            }
        }
    };
    // A panicked reader means its half of the output was lost. Say so rather
    // than silently reporting a short reply as the whole one.
    let stdout = stdout_reader.join().unwrap_or_else(|_| {
        eprintln!("[browser mirror] the bridge's stdout reader panicked; its output is incomplete");
        Vec::new()
    });
    let stderr = stderr_reader.join().unwrap_or_else(|_| {
        eprintln!("[browser mirror] the bridge's stderr reader panicked; its output is incomplete");
        Vec::new()
    });
    match status {
        Some(status) => Ok(Some(CliOutput { status, stdout, stderr })),
        None => Ok(None),
    }
}

fn read_all(mut stream: Option<impl Read + Send + 'static>) -> Vec<u8> {
    let mut buffer = Vec::new();
    if let Some(stream) = stream.as_mut()
        && let Err(error) = stream.read_to_end(&mut buffer)
    {
        eprintln!("[browser mirror] could not read the bridge's output: {error}");
    }
    buffer
}

fn read_screenshot(stdout: &[u8], requested: &Path) -> CallOutcome {
    let text = String::from_utf8_lossy(stdout).trim().to_string();
    let reply: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let saved = match reply.get("saved").and_then(Value::as_str) {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => requested.to_path_buf(),
    };
    let bytes = match std::fs::read(&saved) {
        Ok(bytes) => bytes,
        Err(error) => {
            return CallOutcome::Failed(format!(
                "the browser bridge did not write a screenshot to {}: {error}",
                saved.display()
            ));
        }
    };
    if let Err(error) = std::fs::remove_file(&saved) {
        eprintln!(
            "[browser mirror] could not remove the screenshot {}: {error}",
            saved.display()
        );
    }
    CallOutcome::Image(bytes)
}

static NEXT_SCREENSHOT: AtomicU64 = AtomicU64::new(1);

fn temp_screenshot_path() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    let sequence = NEXT_SCREENSHOT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "horon-agent-mirror-{}-{stamp}-{sequence}.png",
        std::process::id()
    ))
}

#[cfg(test)]
#[path = "browser_mirror_tests.rs"]
mod browser_mirror_tests;
