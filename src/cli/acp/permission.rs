//! `session/request_permission`: the editor's half of the daemon's blocking
//! approval gate (crates/app-core/src/tool/permission_bridge.rs).
//!
//! The daemon sends `tool_permission_requested` for a call its gate holds and
//! waits for a `permission_decision`. This side channel asks the client with
//! the four ACP option kinds and maps the answer back: `allow-once` and
//! `allow-always` allow, `reject-once` and `reject-always` deny, and anything
//! else (a `cancelled` outcome, an unknown `optionId`, a malformed result or a
//! client error) denies. The two `always` answers are remembered per tool name
//! for the session and answered without asking again.
//!
//! The client call has no deadline of its own; the daemon gate owns the wait.
//! It ends early in two ways. A decision made on another surface arrives as
//! `tool_permission_resolved`: the client call is withdrawn with
//! `$/cancel_request` and no decision is sent, since one already stands. A
//! cancelled prompt turn (`session/cancel`, `$/cancel_request` of the prompt,
//! `session/close`) cancels every open call the same way and answers each one
//! `deny`.
//!
//! A v2 session asks with the v2 request (IS-10): the prompt copy goes in
//! `title` and `description`, and `subject` says what needs permission - the
//! command and its absolute cwd for `bash`, the tool call upsert for every
//! other tool. Options, remembered answers and outcome handling are shared.

use super::*;
use crate::protocol::PermissionDecision;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex as StdMutex;

pub(super) const OPTION_ALLOW_ONCE: &str = "allow-once";
pub(super) const OPTION_ALLOW_ALWAYS: &str = "allow-always";
/// v2 only: approve this one file and stop asking for it.
pub(super) const OPTION_ALLOW_PATH: &str = "allow-path";
/// v2 only: approve this file's folder and stop asking for anything under it.
pub(super) const OPTION_ALLOW_FOLDER: &str = "allow-folder";
pub(super) const OPTION_REJECT_ONCE: &str = "reject-once";
pub(super) const OPTION_REJECT_ALWAYS: &str = "reject-always";

/// How many request ids resolved before their client call opened are kept.
const RESOLVED_MEMORY: usize = 256;
const DECISION_REPLY_BOUND: Duration = Duration::from_secs(30);

struct OpenCall {
    client_call_id: Value,
    withdrawn: bool,
}

#[derive(Default)]
struct Inner {
    remembered: HashMap<String, bool>,
    open: HashMap<String, OpenCall>,
    resolved_early: VecDeque<String>,
    /// A cancelled turn is withdrawing this session's calls. A call that opens
    /// while this is set is never asked, so the notification does not depend
    /// on whether `cancel_open` or the daemon reached the request first (#263).
    cancelled_turn: bool,
}

/// What [`PermissionState::open`] decided for one request.
#[derive(Debug, PartialEq, Eq)]
enum OpenOutcome {
    /// The client call is registered and must be asked.
    Asked,
    /// The request was already decided elsewhere; the client must not be asked.
    AlreadyResolved,
    /// The turn was cancelled: every call of it is withdrawn, including this
    /// one, so the client must not be asked.
    CancelledTurn,
}

/// Per-session permission state: remembered `always` answers and the client
/// calls still waiting for an answer, keyed by daemon request id.
#[derive(Default)]
pub(super) struct PermissionState {
    inner: StdMutex<Inner>,
}

impl PermissionState {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(super) fn remembered(&self, tool_name: &str) -> Option<bool> {
        self.lock().remembered.get(tool_name).copied()
    }

    fn remember(&self, tool_name: &str, approved: bool) {
        self.lock().remembered.insert(tool_name.to_string(), approved);
    }

    /// Record the client call for `request_id` and say whether the client must
    /// be asked.
    ///
    /// The cancelled-turn flag is read under the same lock `cancel_open` sets
    /// it with, so a call that opens while a cancel is withdrawing the turn is
    /// never asked: it cannot be missed by the withdrawal pass, whatever order
    /// the two surfaces ran in (#263).
    fn open(&self, request_id: &str, client_call_id: Value) -> OpenOutcome {
        let mut inner = self.lock();
        if let Some(position) = inner.resolved_early.iter().position(|id| id == request_id) {
            inner.resolved_early.remove(position);
            return OpenOutcome::AlreadyResolved;
        }
        if inner.cancelled_turn {
            return OpenOutcome::CancelledTurn;
        }
        inner.open.insert(
            request_id.to_string(),
            OpenCall {
                client_call_id,
                withdrawn: false,
            },
        );
        OpenOutcome::Asked
    }

    /// Retire the call for `request_id`; true when it was withdrawn because
    /// the request was decided elsewhere.
    fn close(&self, request_id: &str) -> bool {
        self.lock().open.remove(request_id).is_some_and(|call| call.withdrawn)
    }

    #[cfg(test)]
    pub(super) fn open_len(&self) -> usize {
        self.lock().open.len()
    }

    /// The request was decided on another surface: withdraw its client call,
    /// or remember the id when the call has not opened yet.
    pub(super) async fn withdraw(&self, request_id: &str, peer: &RpcPeer) {
        let client_call_id = {
            let mut inner = self.lock();
            match inner.open.get_mut(request_id) {
                Some(call) => {
                    call.withdrawn = true;
                    Some(call.client_call_id.clone())
                }
                None => {
                    inner.resolved_early.push_back(request_id.to_string());
                    while inner.resolved_early.len() > RESOLVED_MEMORY {
                        inner.resolved_early.pop_front();
                    }
                    None
                }
            }
        };
        if let Some(id) = client_call_id {
            peer.cancel_outbound(&id).await;
        }
    }

    /// The prompt turn was cancelled: cancel every open client call. Each
    /// waiting handler then answers the daemon `deny`.
    ///
    /// The cancelled-turn flag is set under the same lock the ids are read
    /// with, so a request that opens while this pass runs is not asked at all
    /// ([`OpenOutcome::CancelledTurn`]): the notification then never depends on
    /// which surface reached the request first (#263). The flag holds until
    /// [`Self::end_cancelled_turn`].
    pub(super) async fn cancel_open(&self, peer: &RpcPeer) {
        let ids: Vec<Value> = {
            let mut inner = self.lock();
            inner.cancelled_turn = true;
            inner
                .open
                .values()
                .filter(|call| !call.withdrawn)
                .map(|call| call.client_call_id.clone())
                .collect()
        };
        for id in ids {
            peer.cancel_outbound(&id).await;
        }
    }

    /// The cancelled turn's successor is starting: from here on a request may
    /// be asked again.
    pub(super) fn end_cancelled_turn(&self) {
        self.lock().cancelled_turn = false;
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Answer {
    pub(super) decision: PermissionDecision,
    pub(super) remember: Option<bool>,
}

pub(super) fn answer_from_outcome(result: &std::result::Result<Value, RpcError>) -> Answer {
    let deny = Answer {
        decision: PermissionDecision::Deny,
        remember: None,
    };
    let Ok(result) = result else {
        return deny;
    };
    let outcome = &result["outcome"];
    if outcome["outcome"] != "selected" {
        return deny;
    }
    match outcome["optionId"].as_str() {
        Some(OPTION_ALLOW_ONCE) => Answer {
            decision: PermissionDecision::Allow,
            remember: None,
        },
        Some(OPTION_ALLOW_ALWAYS) => Answer {
            decision: PermissionDecision::Allow,
            remember: Some(true),
        },
        // A scoped answer is not "always for this tool": it is one file or one
        // folder, stored by the daemon. It must not touch the per-tool memory,
        // or a later call on a different path would skip the question.
        Some(OPTION_ALLOW_PATH) => Answer {
            decision: PermissionDecision::AllowPath,
            remember: None,
        },
        Some(OPTION_ALLOW_FOLDER) => Answer {
            decision: PermissionDecision::AllowFolder,
            remember: None,
        },
        Some(OPTION_REJECT_ALWAYS) => Answer {
            decision: PermissionDecision::Deny,
            remember: Some(false),
        },
        _ => deny,
    }
}

/// The options every permission request offers.
///
/// v1 keeps exactly the four it always had. v2 adds the two scoped answers when
/// the call names one path: an editor on v2 can say "yes, and not again for this
/// file" instead of answering the same file every time.
fn permission_options(tool_name: &str, scoped: bool, v2: bool) -> Vec<Value> {
    let mut options = vec![
        json!({ "optionId": OPTION_ALLOW_ONCE, "name": "Allow once", "kind": "allow_once" }),
        json!({ "optionId": OPTION_ALLOW_ALWAYS, "name": format!("Always allow {tool_name}"), "kind": "allow_always" }),
    ];
    if v2 && scoped {
        options
            .push(json!({ "optionId": OPTION_ALLOW_PATH, "name": "Always allow this file", "kind": "allow_always" }));
        options.push(
            json!({ "optionId": OPTION_ALLOW_FOLDER, "name": "Always allow this folder", "kind": "allow_always" }),
        );
    }
    options.push(json!({ "optionId": OPTION_REJECT_ONCE, "name": "Reject", "kind": "reject_once" }));
    options.push(json!({ "optionId": OPTION_REJECT_ALWAYS, "name": format!("Always reject {tool_name}"), "kind": "reject_always" }));
    options
}

pub(super) fn request_permission_params(
    session_id: &str,
    tool_call_id: &str,
    tool_name: &str,
    title: &str,
    input: &Value,
) -> Value {
    let mut tool_call = json!({
        "toolCallId": tool_call_id,
        "title": title,
        "kind": tool_kind(tool_name),
        "status": "pending",
    });
    if !input.is_null() {
        tool_call["rawInput"] = input.clone();
    }
    json!({
        "sessionId": session_id,
        "toolCall": tool_call,
        "options": permission_options(tool_name, false, false),
    })
}

/// IS-10: the v2 `session/request_permission` params. A bash call with a
/// command names that command and the session's absolute `cwd`; every other
/// call (and a bash call without a command) carries the same `tool_call_update`
/// the v2 mapper creates the call with, plus its input.
pub(super) fn request_permission_params_v2(
    session_id: &str,
    tool_call_id: &str,
    tool_name: &str,
    input: &Value,
    cwd: &Path,
) -> Value {
    let display_title = tool_title(tool_name);
    let title = format!("Allow {}?", lowercase_first(&display_title));
    let description = format!("HoronAgent wants to run {tool_name}.");
    let tool_call_id = Some(tool_call_id).filter(|id| !id.is_empty());
    let command = input.get("command").and_then(Value::as_str);
    let subject = match command {
        Some(command) if tool_name == "bash" => v2::shapes::command_subject(command, cwd, tool_call_id),
        _ => {
            let mut patch = v2::shapes::ToolCallPatch::created(&display_title, tool_name, tool_kind(tool_name));
            if !input.is_null() {
                patch.raw_input = Some(input.clone());
            }
            let mut tool_call = v2::shapes::tool_call_update(tool_call_id.unwrap_or_default(), &patch);
            if let Some(update) = tool_call.as_object_mut() {
                update.remove("sessionUpdate");
            }
            v2::shapes::tool_call_subject(tool_call)
        }
    };
    v2::shapes::permission_request(
        session_id,
        &title,
        Some(&description),
        Some(subject),
        &permission_options(
            tool_name,
            crate::approval_grants::names_single_path(tool_name, input),
            true,
        ),
    )
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_lowercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The side channel an ACP session runs: permission requests and withdrawals,
/// client requests (client_router.rs), ask elicitation (elicitation.rs), model
/// changes made outside any request, and everything else as
/// [`DefaultSideChannel`] handles it.
#[derive(Clone)]
pub(super) struct AcpSideChannel {
    pub(super) peer: Arc<RpcPeer>,
    pub(super) daemon: DaemonRequester,
    pub(super) session_id: String,
    pub(super) permissions: Arc<PermissionState>,
    pub(super) client_calls: Arc<ClientCalls>,
    pub(super) connection: Arc<StdMutex<ConnectionState>>,
    pub(super) bounds: CallBounds,
    pub(super) ui_state: Arc<Mutex<SessionUiState>>,
    pub(super) mode: Arc<Mutex<AcpMode>>,
    /// The session's cwd, which a v2 command permission subject names.
    pub(super) cwd: Arc<Mutex<Option<PathBuf>>>,
    /// A v2 session's foreground state, told about every request that waits
    /// on the user (IS-5 `requires_action`); `None` on v1.
    pub(super) user_actions: Option<v2::state::UserActions>,
}

impl SideChannel for AcpSideChannel {
    fn handle(&self, link: Arc<DaemonLink>, event: ServerEvent) -> SideFuture {
        let this = self.clone();
        match event {
            ServerEvent::ToolPermissionRequested {
                request_id,
                tool_call_id,
                tool_name,
                title,
                input,
            } => Box::pin(async move {
                this.ask(request_id, &tool_call_id, &tool_name, &title, &input).await;
            }),
            ServerEvent::ToolPermissionResolved { request_id, .. } => Box::pin(async move {
                this.permissions.withdraw(&request_id, &this.peer).await;
            }),
            ServerEvent::ClientRequest {
                request_id,
                tool_call_id,
                method,
                params,
            } => Box::pin(async move {
                this.route_client_request(request_id, tool_call_id, method, params)
                    .await;
            }),
            ServerEvent::AskToolCall {
                request_id,
                tool_call_id,
            } => {
                if let Some(user_actions) = &this.user_actions {
                    user_actions.note_ask_tool_call(request_id, tool_call_id);
                }
                Box::pin(async {})
            }
            ServerEvent::AskRequested { request_id, questions } => {
                let questions = questions_from_wire(&questions);
                Box::pin(async move { this.elicit(request_id, questions).await })
            }
            ServerEvent::ModelChanged {
                model,
                provider_name,
                error: None,
                ..
            } => Box::pin(async move { this.model_changed(model, provider_name).await }),
            other => DefaultSideChannel.handle(link, other),
        }
    }
}

impl AcpSideChannel {
    async fn ask(&self, request_id: String, tool_call_id: &str, tool_name: &str, title: &str, input: &Value) {
        let decision = match self.permissions.remembered(tool_name) {
            Some(true) => PermissionDecision::Allow,
            Some(false) => PermissionDecision::Deny,
            None => {
                let call = self.peer.begin_call();
                match self.permissions.open(&request_id, call.id.clone()) {
                    OpenOutcome::Asked => {}
                    // Already decided elsewhere: the daemon settled the gate.
                    OpenOutcome::AlreadyResolved => {
                        self.peer.abandon_call(call);
                        return;
                    }
                    // The turn was cancelled after the withdrawal pass took its
                    // snapshot. The gate is still open on the daemon, so it is
                    // answered here instead of asked: a question for a turn
                    // nobody waits for would stay open forever (#263).
                    OpenOutcome::CancelledTurn => {
                        self.peer.abandon_call(call);
                        self.send_decision(request_id, PermissionDecision::Deny).await;
                        return;
                    }
                }
                let params = if self.user_actions.is_some() {
                    let cwd = self.absolute_cwd().await;
                    request_permission_params_v2(&self.session_id, tool_call_id, tool_name, input, &cwd)
                } else {
                    request_permission_params(&self.session_id, tool_call_id, tool_name, title, input)
                };
                let waiting = self.user_actions.as_ref().map(v2::state::UserActions::begin);
                let result = self.peer.finish_call(call, "session/request_permission", params).await;
                drop(waiting);
                if self.permissions.close(&request_id) {
                    return;
                }
                let answer = answer_from_outcome(&result);
                if let Some(approved) = answer.remember {
                    self.permissions.remember(tool_name, approved);
                }
                answer.decision
            }
        };
        self.send_decision(request_id, decision).await;
    }

    /// The session cwd, which v2 requires to be absolute; the process directory
    /// stands in when the session has none, as it does for the v2 mapper.
    async fn absolute_cwd(&self) -> PathBuf {
        self.cwd
            .lock()
            .await
            .clone()
            .filter(|cwd| cwd.is_absolute())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    async fn send_decision(&self, request_id: String, decision: PermissionDecision) {
        let id = self.daemon.next_id();
        let request = Request::PermissionDecision {
            id,
            request_id: request_id.clone(),
            decision,
        };
        let settled = match self.daemon.request(id, &request).await {
            Ok(mut reply) => {
                match tokio::time::timeout(DECISION_REPLY_BOUND, wait_for_accepted(&mut reply, id)).await {
                    Ok(result) => result,
                    Err(_) => Err(anyhow::anyhow!("no reply within {}s", DECISION_REPLY_BOUND.as_secs())),
                }
            }
            Err(err) => Err(err),
        };
        if let Err(err) = settled {
            crate::logging::warn(&format!(
                "ACP: permission decision {decision:?} for {request_id} in {} not applied: {err:#}",
                self.session_id
            ));
        }
    }

    async fn model_changed(&self, model: String, provider_name: Option<String>) {
        {
            let mut state = self.ui_state.lock().await;
            state.model = Some(model);
            if provider_name.is_some() {
                state.provider_name = provider_name;
            }
        }
        let config_options = current_config_options(&self.ui_state, &self.mode).await;
        // Only a v2 session has foreground state to report user actions to.
        let update = if self.user_actions.is_some() {
            v2::mapper::config_option_update(&config_options)
        } else {
            json!({
                "sessionUpdate": "config_option_update",
                "configOptions": config_options,
            })
        };
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": self.session_id,
                "update": update,
            }
        });
        if let Err(err) = self.peer.write_value(notification).await {
            crate::logging::warn(&format!("ACP: failed to write config_option_update: {err:#}"));
        }
    }
}

impl AcpRuntime {
    pub(super) fn install_session_bridge(&self, session: &DaemonSession) {
        session.install_side_channel(Arc::new(AcpSideChannel {
            peer: Arc::clone(&self.peer),
            daemon: session.requester(),
            session_id: session.session_id.clone(),
            permissions: Arc::clone(&session.permissions),
            client_calls: Arc::clone(&session.client_calls),
            connection: Arc::clone(&self.connection),
            bounds: self.call_bounds,
            ui_state: Arc::clone(&session.ui_state),
            mode: Arc::clone(&session.mode),
            cwd: Arc::clone(&session.cwd),
            user_actions: session.v2.get().map(|v2| v2.user_actions()),
        }));
    }
}
