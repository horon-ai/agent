//! Daemon-originated client requests: `fs/*` and `terminal/*` routed to the ACP
//! client, and their answers routed back.
//!
//! The daemon's tools ask for a client method with `client_request`
//! (crates/app-core/src/tool/client_bridge.rs). This side channel checks the
//! capability the client advertised in `initialize`; a method the client did not
//! advertise is answered `-32601` without contacting it (spec: the agent MUST NOT
//! call a method whose capability is absent). Otherwise the same method goes to
//! the client with the agent `sessionId` added, and its result or error goes back
//! to the daemon as `client_response`.
//!
//! A successful `terminal/create` that names a tool call is shown on that call
//! before the daemon hears about it: the `tool_call_update` embedding the
//! terminal is queued on the running prompt's ordered stream, so it follows the
//! call's `tool_call` and precedes its result.
//!
//! Every client call of a session is tracked while it is open. A cancelled turn
//! (`session/cancel`, `$/cancel_request` of the prompt, `session/close`)
//! withdraws each one with `$/cancel_request`, and the daemon gets `-32800`, so
//! the waiting tool fails at once instead of at its own deadline. The terminal
//! cleanup calls (`terminal/kill`, `terminal/release`) are never withdrawn: they
//! are what a cancelled tool sends to leave nothing running in the editor.
//!
//! Deadlines mirror the daemon's own (client_fs.rs `FS_TIMEOUT`,
//! client_terminal.rs `CONTROL_TIMEOUT`, ask.rs `CLIENT_ANSWER_TIMEOUT`), so a
//! client that never answers is withdrawn when the daemon stops waiting.
//! `terminal/wait_for_exit` has none here: the command's deadline belongs to the
//! daemon, which ends the wait with `terminal/kill`.

use super::*;
use crate::protocol::ClientRpcError;
use std::sync::Mutex as StdMutex;

/// How long the daemon is given to acknowledge a forwarded answer.
const DAEMON_REPLY_BOUND: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug)]
pub(super) struct CallBounds {
    pub(super) fs: Duration,
    pub(super) terminal_control: Duration,
    pub(super) elicitation: Duration,
}

impl Default for CallBounds {
    fn default() -> Self {
        Self {
            fs: Duration::from_secs(60),
            terminal_control: Duration::from_secs(30),
            elicitation: Duration::from_secs(300),
        }
    }
}

impl CallBounds {
    fn for_method(&self, method: &str) -> Option<Duration> {
        match method {
            "fs/read_text_file" | "fs/write_text_file" => Some(self.fs),
            "terminal/wait_for_exit" => None,
            "elicitation/create" => Some(self.elicitation),
            _ => Some(self.terminal_control),
        }
    }
}

/// The `clientCapabilities` flag a daemon-originated method needs, as a JSON
/// pointer. `None` for anything that is not one of the seven client methods.
pub(super) fn required_capability(method: &str) -> Option<&'static str> {
    match method {
        "fs/read_text_file" => Some("/fs/readTextFile"),
        "fs/write_text_file" => Some("/fs/writeTextFile"),
        "terminal/create" | "terminal/output" | "terminal/wait_for_exit" | "terminal/kill" | "terminal/release" => {
            Some("/terminal")
        }
        _ => None,
    }
}

pub(super) fn client_serves(client_capabilities: &Value, method: &str) -> bool {
    required_capability(method)
        .is_some_and(|pointer| client_capabilities.pointer(pointer).and_then(Value::as_bool) == Some(true))
}

fn is_cleanup(method: &str) -> bool {
    matches!(method, "terminal/kill" | "terminal/release")
}

struct OpenClientCall {
    client_call_id: Value,
    method: String,
}

/// The client calls of one session that have not been answered, keyed by the
/// daemon id they serve (a `client_request` id or an `ask_requested` id).
#[derive(Default)]
pub(super) struct ClientCalls {
    open: StdMutex<HashMap<String, OpenClientCall>>,
}

impl ClientCalls {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, OpenClientCall>> {
        self.open.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn open(&self, key: &str, client_call_id: Value, method: &str) {
        self.lock().insert(
            key.to_string(),
            OpenClientCall {
                client_call_id,
                method: method.to_string(),
            },
        );
    }

    fn close(&self, key: &str) {
        self.lock().remove(key);
    }

    #[cfg(test)]
    pub(super) fn open_len(&self) -> usize {
        self.lock().len()
    }

    /// The turn was cancelled: withdraw every open call except terminal
    /// cleanup. Each waiting handler then answers the daemon -32800.
    pub(super) async fn cancel_open(&self, peer: &RpcPeer) {
        let ids: Vec<Value> = self
            .lock()
            .values()
            .filter(|call| !is_cleanup(&call.method))
            .map(|call| call.client_call_id.clone())
            .collect();
        for id in ids {
            peer.cancel_outbound(&id).await;
        }
    }
}

/// A client answer the daemon can use: an object result, or the client's own
/// error. Anything else is reported as an internal error rather than passed on.
pub(super) fn answer_for_daemon(method: &str, result: std::result::Result<Value, RpcError>) -> ClientAnswerWire {
    match result {
        Ok(value) if value.is_object() => ClientAnswerWire::Result(value),
        Ok(value) => ClientAnswerWire::Error(ClientRpcError {
            code: JSONRPC_INTERNAL_ERROR,
            message: format!("the ACP client answered {method} with a non-object result: {value}"),
        }),
        Err(error) => ClientAnswerWire::Error(ClientRpcError {
            code: error.code,
            message: error.message,
        }),
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum ClientAnswerWire {
    Result(Value),
    Error(ClientRpcError),
}

pub(super) fn client_params(session_id: &str, params: Value) -> std::result::Result<Value, String> {
    let mut params = match params {
        Value::Null => json!({}),
        Value::Object(_) => params,
        other => return Err(format!("client request params must be an object, got {other}")),
    };
    params["sessionId"] = json!(session_id);
    Ok(params)
}

impl AcpSideChannel {
    /// Open a tracked client call, wait for it within the method's deadline,
    /// and retire it.
    pub(super) async fn call_tracked(
        &self,
        key: &str,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, RpcError> {
        let call = self.peer.begin_call();
        self.client_calls.open(key, call.id.clone(), method);
        let result = self
            .peer
            .finish_call_within(call, method, params, self.bounds.for_method(method))
            .await;
        self.client_calls.close(key);
        result
    }

    pub(super) async fn route_client_request(
        &self,
        request_id: String,
        tool_call_id: Option<String>,
        method: String,
        params: Value,
    ) {
        let capabilities = self.client_capabilities();
        let answer = if !client_serves(&capabilities, &method) {
            ClientAnswerWire::Error(ClientRpcError {
                code: JSONRPC_METHOD_NOT_FOUND,
                message: format!("the ACP client did not advertise the capability {method} needs"),
            })
        } else {
            match client_params(&self.session_id, params) {
                Err(message) => ClientAnswerWire::Error(ClientRpcError {
                    code: JSONRPC_INVALID_PARAMS,
                    message,
                }),
                Ok(params) => {
                    let result = self.call_tracked(&request_id, &method, params).await;
                    answer_for_daemon(&method, result)
                }
            }
        };
        if method == "terminal/create"
            && let (Some(tool_call_id), ClientAnswerWire::Result(result)) = (tool_call_id, &answer)
            && let Some(terminal_id) = result.get("terminalId").and_then(Value::as_str)
        {
            self.show_terminal(tool_call_id, terminal_id.to_string()).await;
        }
        let (result, error) = match answer {
            ClientAnswerWire::Result(value) => (Some(value), None),
            ClientAnswerWire::Error(error) => (None, Some(error)),
        };
        let id = self.daemon.next_id();
        let response = Request::ClientResponse {
            id,
            request_id: request_id.clone(),
            result,
            error,
        };
        self.send_to_daemon(id, &response, &format!("client_response {request_id} ({method})"))
            .await;
    }

    /// Queue the terminal on the running prompt's stream and wait until it is
    /// written. With no prompt to write it there is no turn to show it in.
    async fn show_terminal(&self, tool_call_id: String, terminal_id: String) {
        let (written, done) = tokio::sync::oneshot::channel();
        let update = LocalUpdate::TerminalAttached {
            tool_call_id,
            terminal_id,
            written,
        };
        match self.daemon.send_to_foreground(update) {
            Ok(()) => {
                if done.await.is_err() {
                    crate::logging::info(&format!(
                        "ACP: terminal for {} not shown: the prompt stream ended first",
                        self.session_id
                    ));
                }
            }
            Err(_) => crate::logging::info(&format!(
                "ACP: terminal for {} not shown: no prompt is running",
                self.session_id
            )),
        }
    }

    /// Send a request whose reply only confirms it, and log a refusal.
    pub(super) async fn send_to_daemon(&self, id: u64, request: &Request, what: &str) {
        let settled = match self.daemon.request(id, request).await {
            Ok(mut reply) => match tokio::time::timeout(DAEMON_REPLY_BOUND, wait_for_accepted(&mut reply, id)).await {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!("no reply within {}s", DAEMON_REPLY_BOUND.as_secs())),
            },
            Err(err) => Err(err),
        };
        if let Err(err) = settled {
            crate::logging::warn(&format!("ACP: {what} for {} not applied: {err:#}", self.session_id));
        }
    }

    pub(super) fn client_capabilities(&self) -> Value {
        self.connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .client_capabilities
            .clone()
    }
}

impl AcpRuntime {
    pub(super) async fn write_local_update(
        &self,
        session_id: &str,
        mapper: &mut EventMapper,
        update: LocalUpdate,
    ) -> Result<()> {
        match update {
            LocalUpdate::TerminalAttached {
                tool_call_id,
                terminal_id,
                written,
            } => {
                let update = mapper.terminal_attached(tool_call_id, terminal_id);
                self.write_notification("session/update", json!({ "sessionId": session_id, "update": update }))
                    .await?;
                let _ = written.send(());
                Ok(())
            }
        }
    }
}
