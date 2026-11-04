//! JSON-RPC 2.0 framing for the ACP stdio connection, in both directions.
//!
//! Incoming lines are classified as requests, notifications, or responses.
//! Responses answer requests the agent issued itself (`call_client`); they are
//! routed to the waiter registered for that id and never answered back.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, oneshot};

pub(super) const JSONRPC_PARSE_ERROR: i64 = -32700;
pub(super) const JSONRPC_INVALID_REQUEST: i64 = -32600;
pub(super) const JSONRPC_METHOD_NOT_FOUND: i64 = -32601;
pub(super) const JSONRPC_INVALID_PARAMS: i64 = -32602;
pub(super) const JSONRPC_INTERNAL_ERROR: i64 = -32603;
pub(super) const JSONRPC_REQUEST_CANCELLED: i64 = -32800;
/// ACP `auth_required`. Reserved for the authentication gate.
pub(super) const ACP_AUTH_REQUIRED: i64 = -32000;
/// ACP `resource_not_found`; carries `data.sessionId` for session errors.
pub(super) const ACP_RESOURCE_NOT_FOUND: i64 = -32002;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct RpcError {
    pub(super) code: i64,
    pub(super) message: String,
    pub(super) data: Option<Value>,
}

impl RpcError {
    pub(super) fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub(super) fn cancelled() -> Self {
        Self::new(JSONRPC_REQUEST_CANCELLED, "Request cancelled")
    }

    /// A session-scoped request named a session that does not exist or is
    /// not active on this connection.
    pub(super) fn session_not_found(session_id: &str) -> Self {
        Self {
            code: ACP_RESOURCE_NOT_FOUND,
            message: format!("Session not found: {session_id}"),
            data: Some(json!({ "sessionId": session_id })),
        }
    }

    pub(super) fn auth_required() -> Self {
        Self::new(ACP_AUTH_REQUIRED, "authentication required: run 'horon-agent login'")
    }

    fn from_wire(value: &Value) -> Self {
        Self {
            code: value
                .get("code")
                .and_then(Value::as_i64)
                .unwrap_or(JSONRPC_INTERNAL_ERROR),
            message: value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            data: value.get("data").cloned(),
        }
    }

    pub(super) fn to_wire(&self) -> Value {
        let mut error = json!({ "code": self.code, "message": self.message });
        if let (Some(data), Some(object)) = (&self.data, error.as_object_mut()) {
            object.insert("data".to_string(), data.clone());
        }
        error
    }
}

#[derive(Debug)]
pub(super) enum JsonRpcMessage {
    Request { id: Value, method: String, params: Value },
    Notification { method: String, params: Value },
    Response { id: Value, result: Result<Value, RpcError> },
}

impl JsonRpcMessage {
    pub(super) fn parse(line: &str) -> std::result::Result<Self, (i64, String)> {
        let value: Value = serde_json::from_str(line).map_err(|err| (JSONRPC_PARSE_ERROR, err.to_string()))?;
        let object = value.as_object().ok_or_else(|| {
            (
                JSONRPC_INVALID_REQUEST,
                "JSON-RPC message must be an object".to_string(),
            )
        })?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err((
                JSONRPC_INVALID_REQUEST,
                "JSON-RPC message must include jsonrpc=\"2.0\"".to_string(),
            ));
        }
        let id = object.get("id").cloned();
        if let Some(id) = &id
            && !is_valid_id(id)
        {
            return Err((
                JSONRPC_INVALID_REQUEST,
                "JSON-RPC id must be a string, an integer, or null".to_string(),
            ));
        }
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        match (object.get("method"), id) {
            (Some(Value::String(method)), Some(id)) => Ok(Self::Request {
                id,
                method: method.clone(),
                params,
            }),
            (Some(Value::String(method)), None) => Ok(Self::Notification {
                method: method.clone(),
                params,
            }),
            (Some(_), _) => Err((JSONRPC_INVALID_REQUEST, "JSON-RPC method must be a string".to_string())),
            (None, Some(id)) => match (object.get("result"), object.get("error")) {
                (Some(result), None) => Ok(Self::Response {
                    id,
                    result: Ok(result.clone()),
                }),
                (None, Some(error)) if error.is_object() => Ok(Self::Response {
                    id,
                    result: Err(RpcError::from_wire(error)),
                }),
                _ => Err((JSONRPC_INVALID_REQUEST, "JSON-RPC request missing method".to_string())),
            },
            (None, None) => Err((
                JSONRPC_INVALID_REQUEST,
                "JSON-RPC message has neither a method nor an id".to_string(),
            )),
        }
    }

    /// The id to answer an unparseable line with: the line's own id when it
    /// is readable and well-formed, otherwise null (JSON-RPC 2.0 section 5).
    pub(super) fn error_reply_id(line: &str) -> Value {
        serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|value| value.get("id").cloned())
            .filter(is_valid_id)
            .unwrap_or(Value::Null)
    }
}

fn is_valid_id(id: &Value) -> bool {
    match id {
        Value::Null | Value::String(_) => true,
        Value::Number(number) => number.is_i64() || number.is_u64(),
        _ => false,
    }
}

/// Identity of a JSON-RPC request id. Keyed by the raw JSON text, so the
/// integer `424242` and the string `"424242"` are different requests.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct RequestKey(String);

impl RequestKey {
    pub(super) fn of(id: &Value) -> Self {
        Self(id.to_string())
    }
}

type OutboundWaiter = oneshot::Sender<Result<Value, RpcError>>;

/// Requests the agent sent to the client and is still waiting on. Agent ids
/// are strings `horon-<n>`, so they never collide with client-issued ids.
#[derive(Default)]
pub(super) struct OutboundRequests {
    next_id: AtomicU64,
    pending: StdMutex<HashMap<RequestKey, OutboundWaiter>>,
}

impl OutboundRequests {
    fn register(&self) -> (Value, oneshot::Receiver<Result<Value, RpcError>>) {
        let id = Value::String(format!("horon-{}", self.next_id.fetch_add(1, Ordering::Relaxed)));
        let (tx, rx) = oneshot::channel();
        self.lock().insert(RequestKey::of(&id), tx);
        (id, rx)
    }

    fn take(&self, id: &Value) -> Option<OutboundWaiter> {
        self.lock().remove(&RequestKey::of(id))
    }

    fn is_pending(&self, id: &Value) -> bool {
        self.lock().contains_key(&RequestKey::of(id))
    }

    /// Route a client response to its waiter. Responses to unknown or
    /// already-resolved ids are dropped silently.
    pub(super) fn resolve(&self, id: &Value, result: Result<Value, RpcError>) {
        if let Some(waiter) = self.take(id) {
            let _ = waiter.send(result);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<RequestKey, OutboundWaiter>> {
        self.pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

pub(super) type OutputSink = Box<dyn AsyncWrite + Send + Unpin>;

/// One JSON value as one newline-terminated line. `serde_json` escapes
/// control characters, so the line never contains an embedded newline.
fn frame_line(value: &Value) -> anyhow::Result<String> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    Ok(line)
}

async fn write_line(out: &mut OutputSink, line: &str) -> anyhow::Result<()> {
    out.write_all(line.as_bytes()).await?;
    out.flush().await?;
    Ok(())
}

/// An agent-issued request whose waiter is registered but whose frame is not
/// written yet. Its `id` is known before the write, so a caller can make the
/// request cancellable (`RpcPeer::cancel_outbound`) before the client sees it.
pub(super) struct ClientCall {
    pub(super) id: Value,
    response: oneshot::Receiver<Result<Value, RpcError>>,
}

/// The client end of the connection: one serialized writer for every line we
/// put on stdout, plus the registry of agent-issued requests.
pub(super) struct RpcPeer {
    out: Mutex<OutputSink>,
    pub(super) outbound: OutboundRequests,
}

impl RpcPeer {
    pub(super) fn new(out: OutputSink) -> Self {
        Self {
            out: Mutex::new(out),
            outbound: OutboundRequests::default(),
        }
    }

    /// Write one JSON value as exactly one newline-terminated line.
    /// `serde_json` escapes control characters, so the line never contains an
    /// embedded newline.
    pub(super) async fn write_value(&self, value: Value) -> anyhow::Result<()> {
        let line = frame_line(&value)?;
        let mut out = self.out.lock().await;
        write_line(&mut out, &line).await
    }

    pub(super) async fn write_error(&self, id: Value, error: &RpcError) -> anyhow::Result<()> {
        self.write_value(json!({ "jsonrpc": "2.0", "id": id, "error": error.to_wire() }))
            .await
    }

    /// Register the waiter for a new agent-issued request without writing it.
    pub(super) fn begin_call(&self) -> ClientCall {
        let (id, response) = self.outbound.register();
        ClientCall { id, response }
    }

    /// Write `call` and wait for the client's answer, with no deadline. The
    /// wait ends early with -32800 when the call is cancelled with
    /// `cancel_outbound`.
    pub(super) async fn finish_call(&self, call: ClientCall, method: &str, params: Value) -> Result<Value, RpcError> {
        let ClientCall { id, response } = call;
        let request = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let written = async {
            let line = frame_line(&request)?;
            let mut out = self.out.lock().await;
            // Checked under the writer lock: a call cancelled before this
            // point is never shown to the client, and one cancelled after it
            // gets its `$/cancel_request` written after the request itself.
            if !self.outbound.is_pending(&id) {
                return Ok(false);
            }
            write_line(&mut out, &line).await.map(|()| true)
        };
        match written.await {
            Ok(true) => {}
            Ok(false) => return Err(RpcError::cancelled()),
            Err(err) => {
                self.outbound.take(&id);
                return Err(RpcError::new(
                    JSONRPC_INTERNAL_ERROR,
                    format!("failed to send {method} to the client: {err:#}"),
                ));
            }
        }
        response.await.unwrap_or_else(|_| Err(RpcError::cancelled()))
    }

    /// Drop a call that will never be written.
    pub(super) fn abandon_call(&self, call: ClientCall) {
        self.outbound.take(&call.id);
    }

    /// Send a request to the client and wait for its response. The waiter is
    /// registered before the request is written, so a fast response cannot be
    /// lost. On timeout the request is cancelled with `$/cancel_request`.
    #[cfg(test)]
    pub(super) async fn call_client(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, RpcError> {
        self.finish_call_within(self.begin_call(), method, params, Some(timeout))
            .await
    }

    /// [`Self::finish_call`] with an optional deadline. On expiry the request
    /// is cancelled with `$/cancel_request` and answers -32800.
    pub(super) async fn finish_call_within(
        &self,
        call: ClientCall,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, RpcError> {
        let Some(timeout) = timeout else {
            return self.finish_call(call, method, params).await;
        };
        let id = call.id.clone();
        match tokio::time::timeout(timeout, self.finish_call(call, method, params)).await {
            Ok(result) => result,
            Err(_) => {
                self.cancel_outbound(&id).await;
                Err(RpcError::new(
                    JSONRPC_REQUEST_CANCELLED,
                    format!("{method} timed out after {} ms", timeout.as_millis()),
                ))
            }
        }
    }

    /// Cancel an outstanding agent-issued request: tell the client with
    /// `$/cancel_request` and resolve the local waiter with -32800. A request
    /// that already resolved is left alone.
    pub(super) async fn cancel_outbound(&self, id: &Value) {
        let Some(waiter) = self.outbound.take(id) else {
            return;
        };
        let _ = waiter.send(Err(RpcError::cancelled()));
        let notification = json!({
            "jsonrpc": "2.0",
            "method": "$/cancel_request",
            "params": { "requestId": id },
        });
        if let Err(err) = self.write_value(notification).await {
            crate::logging::warn(&format!("ACP: failed to send $/cancel_request: {err:#}"));
        }
    }
}
