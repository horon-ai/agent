//! Session-scoped bridge from daemon tool code to the ACP client that serves
//! `fs/*` and `terminal/*` requests for it.
//!
//! The daemon's tools run inside a turn and hold no connection handle, so the
//! ACP connection that attached to a session installs a [`ClientBridge`] here
//! and tools look it up by session id. Presence is the capability signal: no
//! installed bridge means the tool touches local disk or spawns locally, which
//! is exactly what every non-ACP client already gets.
//!
//! Ownership is not the same as presence. A session can be live in several
//! attachments at once, so a bridge answers only while the running turn was
//! started by the connection that installed it ([`super::acp_session::turn_owned_by`]).
//! A desktop turn on an ACP-attached session therefore keeps local behaviour.
//!
//! Concurrency differs from [`super::sdk`] on purpose: pending client requests
//! are unlimited per session, each with its own one-shot answer channel, because
//! one turn can issue many independent file and terminal reads. A request that
//! is never answered costs its caller one bounded timeout, and the entry is
//! removed either way.

use super::acp_session;
use crate::protocol::{AcpClientCapabilities, ClientRpcError, ServerEvent};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

/// What a client answered: the method result, or the JSON-RPC error it sent.
pub(crate) type ClientAnswer = Result<Value, ClientRpcError>;

pub(crate) struct ClientBridge {
    connection_id: String,
    caps: AcpClientCapabilities,
    tx: mpsc::UnboundedSender<ServerEvent>,
    pending: Mutex<HashMap<String, oneshot::Sender<ClientAnswer>>>,
}

struct Installed {
    token: u64,
    bridge: Arc<ClientBridge>,
}

fn registry() -> &'static Mutex<HashMap<String, Installed>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Installed>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn lock() -> MutexGuard<'static, HashMap<String, Installed>> {
    registry().lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether these capabilities let the client serve anything at all.
///
/// A Subscribe that names ACP but advertises no method installs no bridge: a
/// bridge that can never be called would only turn local behaviour off.
pub(crate) fn serves_client_methods(caps: &AcpClientCapabilities) -> bool {
    caps.fs_read_text_file || caps.fs_write_text_file || caps.terminal
}

/// Offer `connection_id`'s client as the fs/terminal backend for `session_id`.
///
/// Replaces any previous installation and returns the token a later [`clear`]
/// must present.
pub(crate) fn install(
    session_id: &str,
    connection_id: &str,
    caps: AcpClientCapabilities,
    tx: mpsc::UnboundedSender<ServerEvent>,
) -> u64 {
    static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);
    let token = NEXT_TOKEN.fetch_add(1, Ordering::Relaxed);
    let bridge = Arc::new(ClientBridge {
        connection_id: connection_id.to_string(),
        caps,
        tx,
        pending: Mutex::new(HashMap::new()),
    });
    lock().insert(session_id.to_string(), Installed { token, bridge });
    token
}

/// Withdraw an installation and fail everything still waiting on it.
///
/// Only the token's own installation is withdrawn, so an older transport
/// finishing its disconnect cannot take away a newer channel. Dropping the
/// pending senders is what ends the tools' waits instead of leaving a
/// disconnected editor's turn parked.
pub(crate) fn clear(session_id: &str, token: u64) {
    let removed = {
        let mut installed = lock();
        match installed.get(session_id) {
            Some(entry) if entry.token == token => installed.remove(session_id),
            _ => None,
        }
    };
    if let Some(installed) = removed
        && let Ok(mut pending) = installed.bridge.pending.lock()
    {
        pending.clear();
    }
}

/// The bridge for `session_id`, or `None` when the running turn belongs to
/// another connection (or none is running).
pub(crate) fn for_session(session_id: &str) -> Option<Arc<ClientBridge>> {
    let bridge = lock().get(session_id).map(|entry| Arc::clone(&entry.bridge))?;
    acp_session::turn_owned_by(session_id, &bridge.connection_id).then_some(bridge)
}

/// The bridge for `session_id`, but only for the connection that installed it.
///
/// Answer routing uses this instead of [`for_session`]: an answer belongs to a
/// request this connection was sent, and it must still resolve after the turn
/// that issued it ends. A response from any other connection is refused.
pub(crate) fn bridge_for_connection(session_id: &str, connection_id: &str) -> Option<Arc<ClientBridge>> {
    let bridge = lock().get(session_id).map(|entry| Arc::clone(&entry.bridge))?;
    (bridge.connection_id == connection_id).then_some(bridge)
}

impl ClientBridge {
    pub(crate) fn caps(&self) -> &AcpClientCapabilities {
        &self.caps
    }

    #[cfg(test)]
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.lock().map(|pending| pending.len()).unwrap_or(0)
    }

    /// Ask the client to serve one method and wait for its answer.
    ///
    /// The waiter is registered before the request is sent, so an answer that
    /// arrives immediately still finds it. `timeout` belongs to the caller: a
    /// file read and a terminal wait have different deadlines, and neither
    /// shares a global constant with the `ask` tool.
    pub(crate) async fn call(
        &self,
        method: &str,
        params: Value,
        tool_call_id: Option<String>,
        timeout: Duration,
    ) -> anyhow::Result<Value> {
        let (request_id, answer) = self.call_answer(method, params, tool_call_id, timeout).await?;
        match answer {
            Ok(value) => Ok(value),
            Err(error) => Err(anyhow::anyhow!(
                "client request `{request_id}` failed with error {}: {}",
                error.code,
                error.message
            )),
        }
    }

    /// [`Self::call`] with the client's own answer kept, so a caller can act on
    /// the JSON-RPC error code (`-32002` is "no such path" for the fs methods).
    ///
    /// The request id comes back with the answer: only this method knows it, and
    /// a caller's error text names it.
    pub(crate) async fn call_answer(
        &self,
        method: &str,
        params: Value,
        tool_call_id: Option<String>,
        timeout: Duration,
    ) -> anyhow::Result<(String, ClientAnswer)> {
        let request_id = format!("cb-{}", uuid::Uuid::new_v4());
        let (answer_tx, answer_rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
            pending.insert(request_id.clone(), answer_tx);
        }
        let _guard = PendingGuard {
            pending: &self.pending,
            request_id: request_id.clone(),
        };
        let sent = self.tx.send(ServerEvent::ClientRequest {
            request_id: request_id.clone(),
            tool_call_id,
            method: method.to_string(),
            params,
        });
        if sent.is_err() {
            anyhow::bail!("the ACP client is not connected");
        }
        match tokio::time::timeout(timeout, answer_rx).await {
            Ok(Ok(answer)) => Ok((request_id, answer)),
            Ok(Err(_)) => Err(anyhow::anyhow!(
                "the ACP client connection closed before answering request `{request_id}`"
            )),
            Err(_) => Err(anyhow::anyhow!("client request timed out")),
        }
    }

    /// Deliver one client answer to the call waiting on it.
    ///
    /// An unknown id is an error, never a silent success: the caller answers the
    /// client's `client_response` with that error so a stale or duplicated
    /// answer is visible instead of dropped.
    pub(crate) fn complete(&self, request_id: &str, answer: ClientAnswer) -> anyhow::Result<()> {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(request_id);
        let Some(answer_tx) = pending else {
            anyhow::bail!("unknown client request");
        };
        answer_tx
            .send(answer)
            .map_err(|_| anyhow::anyhow!("client request was cancelled"))
    }
}

struct PendingGuard<'a> {
    pending: &'a Mutex<HashMap<String, oneshot::Sender<ClientAnswer>>>,
    request_id: String,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&self.request_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn caps_fs() -> AcpClientCapabilities {
        AcpClientCapabilities {
            fs_read_text_file: true,
            ..AcpClientCapabilities::default()
        }
    }

    fn channel() -> (mpsc::UnboundedSender<ServerEvent>, mpsc::UnboundedReceiver<ServerEvent>) {
        mpsc::unbounded_channel()
    }

    fn installed(session: &str, connection: &str) -> (u64, u64, mpsc::UnboundedReceiver<ServerEvent>) {
        let (tx, rx) = channel();
        let session_token = acp_session::install(session, connection, tx.clone());
        let bridge_token = install(session, connection, caps_fs(), tx);
        (session_token, bridge_token, rx)
    }

    async fn next_request(rx: &mut mpsc::UnboundedReceiver<ServerEvent>) -> (String, String, Value) {
        let event = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("the bridge must send a client request")
            .expect("the event channel must stay open");
        match event {
            ServerEvent::ClientRequest {
                request_id,
                method,
                params,
                ..
            } => (request_id, method, params),
            other => panic!("expected a client request, got {other:?}"),
        }
    }

    #[test]
    fn bridge_not_installed_without_acp() {
        assert!(for_session("bridge-no-acp").is_none());
        let (tx, _rx) = channel();
        let token = acp_session::install("bridge-no-acp", "conn-a", tx);
        assert!(
            for_session("bridge-no-acp").is_none(),
            "an ACP session without advertised capabilities must not route fs/terminal calls"
        );
        acp_session::clear("bridge-no-acp", token);
        assert!(!serves_client_methods(&AcpClientCapabilities::default()));
        assert!(serves_client_methods(&caps_fs()));
    }

    #[tokio::test]
    async fn bridge_call_resolves_on_client_response() {
        let session = "bridge-resolves";
        let (session_token, bridge_token, mut rx) = installed(session, "conn-a");
        let turn = acp_session::claim_turn(session, Some("conn-a"));
        let bridge = for_session(session).expect("the owning connection's turn sees the bridge");

        let caller = {
            let bridge = Arc::clone(&bridge);
            tokio::spawn(async move {
                bridge
                    .call(
                        "fs/read_text_file",
                        json!({"path": "/tmp/unsaved.txt"}),
                        Some("call-1".to_string()),
                        Duration::from_secs(5),
                    )
                    .await
            })
        };
        let (request_id, method, params) = next_request(&mut rx).await;
        assert_eq!(method, "fs/read_text_file");
        assert_eq!(params["path"], "/tmp/unsaved.txt");
        assert!(request_id.starts_with("cb-"), "{request_id}");
        bridge
            .complete(&request_id, Ok(json!({"content": "buffer text"})))
            .expect("the pending call must accept its answer");
        let answer = caller
            .await
            .expect("the call task must not panic")
            .expect("the call resolves");
        assert_eq!(answer["content"], "buffer text");
        assert_eq!(bridge.pending_len(), 0);

        drop(turn);
        clear(session, bridge_token);
        acp_session::clear(session, session_token);
    }

    #[tokio::test]
    async fn bridge_call_times_out_and_cleans_pending() {
        let session = "bridge-timeout";
        let (session_token, bridge_token, mut rx) = installed(session, "conn-a");
        let turn = acp_session::claim_turn(session, Some("conn-a"));
        let bridge = for_session(session).expect("installed");

        let error = bridge
            .call(
                "fs/read_text_file",
                json!({"path": "/tmp/never-answered.txt"}),
                None,
                Duration::from_millis(50),
            )
            .await
            .expect_err("an unanswered request must time out");
        let _ = next_request(&mut rx).await;
        assert_eq!(error.to_string(), "client request timed out");
        assert_eq!(
            bridge.pending_len(),
            0,
            "a timed-out request must not stay in the pending map"
        );

        drop(turn);
        clear(session, bridge_token);
        acp_session::clear(session, session_token);
    }

    #[tokio::test]
    async fn bridge_concurrent_calls_resolve_independently() {
        let session = "bridge-concurrent";
        let (session_token, bridge_token, mut rx) = installed(session, "conn-a");
        let turn = acp_session::claim_turn(session, Some("conn-a"));
        let bridge = for_session(session).expect("installed");

        let mut callers = Vec::new();
        for index in 0..3 {
            let bridge = Arc::clone(&bridge);
            callers.push(tokio::spawn(async move {
                bridge
                    .call(
                        "fs/read_text_file",
                        json!({"path": format!("/tmp/file-{index}.txt")}),
                        None,
                        Duration::from_secs(5),
                    )
                    .await
            }));
        }
        let mut requests = Vec::new();
        for _ in 0..3 {
            requests.push(next_request(&mut rx).await);
        }
        let unique: std::collections::HashSet<_> = requests.iter().map(|(id, _, _)| id.clone()).collect();
        assert_eq!(unique.len(), 3, "every concurrent request needs its own id");
        for (request_id, _, params) in requests.iter().rev() {
            let path = params["path"].as_str().expect("path");
            bridge
                .complete(request_id, Ok(json!({"content": path})))
                .expect("each pending call accepts its own answer");
        }
        for (index, caller) in callers.into_iter().enumerate() {
            let answer = caller.await.expect("no panic").expect("resolved");
            assert_eq!(answer["content"], format!("/tmp/file-{index}.txt"));
        }
        assert_eq!(bridge.pending_len(), 0);

        drop(turn);
        clear(session, bridge_token);
        acp_session::clear(session, session_token);
    }

    #[tokio::test]
    async fn bridge_cleared_on_disconnect_fails_waiters() {
        let session = "bridge-disconnect";
        let (session_token, bridge_token, mut rx) = installed(session, "conn-a");
        let turn = acp_session::claim_turn(session, Some("conn-a"));
        let bridge = for_session(session).expect("installed");

        let caller = {
            let bridge = Arc::clone(&bridge);
            tokio::spawn(async move {
                bridge
                    .call(
                        "fs/read_text_file",
                        json!({"path": "/tmp/x"}),
                        None,
                        Duration::from_secs(30),
                    )
                    .await
            })
        };
        let _ = next_request(&mut rx).await;
        clear(session, bridge_token);
        let error = caller
            .await
            .expect("no panic")
            .expect_err("a disconnect must end the wait instead of parking the turn");
        assert!(error.to_string().contains("closed before answering"), "{error}");
        assert_eq!(bridge.pending_len(), 0);
        assert!(for_session(session).is_none());

        drop(turn);
        acp_session::clear(session, session_token);
    }

    #[tokio::test]
    async fn client_response_unknown_id_errors() {
        let session = "bridge-unknown-id";
        let (session_token, bridge_token, mut rx) = installed(session, "conn-a");
        let turn = acp_session::claim_turn(session, Some("conn-a"));
        let bridge = for_session(session).expect("installed");

        let first = bridge
            .complete("cb-does-not-exist", Ok(json!({})))
            .expect_err("an unknown request id must be reported");
        assert_eq!(first.to_string(), "unknown client request");

        let caller = {
            let bridge = Arc::clone(&bridge);
            tokio::spawn(async move {
                bridge
                    .call(
                        "fs/read_text_file",
                        json!({"path": "/tmp/y"}),
                        None,
                        Duration::from_secs(5),
                    )
                    .await
            })
        };
        let (request_id, _, _) = next_request(&mut rx).await;
        bridge
            .complete(&request_id, Ok(json!({"content": "once"})))
            .expect("the first answer resolves the call");
        let repeat = bridge
            .complete(&request_id, Ok(json!({"content": "twice"})))
            .expect_err("a repeated answer must be rejected, not panic");
        assert_eq!(repeat.to_string(), "unknown client request");
        let answer = caller.await.expect("no panic").expect("resolved once");
        assert_eq!(answer["content"], "once");

        drop(turn);
        clear(session, bridge_token);
        acp_session::clear(session, session_token);
    }

    #[test]
    fn desktop_turn_in_shared_session_uses_local_fs() {
        let session = "bridge-shared-session";
        let (session_token, bridge_token, _rx) = installed(session, "conn-acp");
        assert!(
            for_session(session).is_none(),
            "no turn is running yet, so nothing routes to the editor"
        );

        let desktop_turn = acp_session::claim_turn(session, Some("conn-desktop"));
        assert!(
            for_session(session).is_none(),
            "a turn started by another client on the same session must use local fs"
        );
        drop(desktop_turn);

        let acp_turn = acp_session::claim_turn(session, Some("conn-acp"));
        assert!(for_session(session).is_some());
        drop(acp_turn);

        clear(session, bridge_token);
        acp_session::clear(session, session_token);
    }

    #[test]
    fn turn_owner_cleared_when_turn_ends() {
        let session = "bridge-turn-end";
        let (session_token, bridge_token, _rx) = installed(session, "conn-acp");
        let turn = acp_session::claim_turn(session, Some("conn-acp"));
        assert!(for_session(session).is_some());

        drop(turn);
        assert!(
            for_session(session).is_none(),
            "a finished ACP turn must not leave the editor bridge answerable"
        );

        let _daemon_turn = acp_session::claim_turn(session, None);
        assert!(
            for_session(session).is_none(),
            "a turn the daemon started on the same session must use local fs"
        );

        clear(session, bridge_token);
        acp_session::clear(session, session_token);
    }

    #[tokio::test]
    async fn late_client_response_after_owner_change_still_resolves() {
        let session = "bridge-late-response";
        let (session_token, bridge_token, mut rx) = installed(session, "conn-a");
        let turn = acp_session::claim_turn(session, Some("conn-a"));
        let bridge = for_session(session).expect("installed");

        let caller = {
            let bridge = Arc::clone(&bridge);
            tokio::spawn(async move {
                bridge
                    .call(
                        "fs/read_text_file",
                        json!({"path": "/tmp/late"}),
                        None,
                        Duration::from_secs(5),
                    )
                    .await
            })
        };
        let (request_id, _, _) = next_request(&mut rx).await;

        drop(turn);
        assert!(
            for_session(session).is_none(),
            "the turn is over, so tool lookups no longer route to the editor"
        );
        let answering = bridge_for_connection(session, "conn-a").expect("the answer still belongs to this connection");
        answering
            .complete(&request_id, Ok(json!({"content": "late but valid"})))
            .expect("an answer to a request this connection was sent must still resolve it");
        let answer = caller.await.expect("no panic").expect("resolved");
        assert_eq!(answer["content"], "late but valid");

        clear(session, bridge_token);
        acp_session::clear(session, session_token);
    }

    #[tokio::test]
    async fn client_response_from_other_connection_rejected() {
        let session = "bridge-other-connection";
        let (session_token, bridge_token, mut rx) = installed(session, "conn-a");
        let turn = acp_session::claim_turn(session, Some("conn-a"));
        let bridge = for_session(session).expect("installed");

        let caller = {
            let bridge = Arc::clone(&bridge);
            tokio::spawn(async move {
                bridge
                    .call(
                        "fs/read_text_file",
                        json!({"path": "/tmp/guarded"}),
                        None,
                        Duration::from_secs(5),
                    )
                    .await
            })
        };
        let (request_id, _, _) = next_request(&mut rx).await;

        assert!(
            bridge_for_connection(session, "conn-other").is_none(),
            "a response from another connection must not be routed to the editor's bridge"
        );

        let answering = bridge_for_connection(session, "conn-a").expect("the owning connection answers");
        answering
            .complete(&request_id, Ok(json!({"content": "from the owner"})))
            .expect("the owning connection resolves the call");
        let answer = caller.await.expect("no panic").expect("resolved");
        assert_eq!(answer["content"], "from the owner");

        drop(turn);
        clear(session, bridge_token);
        acp_session::clear(session, session_token);
    }

    #[test]
    fn a_resubscribe_does_not_leave_the_old_bridge_answerable() {
        let session = "bridge-resubscribe";
        let (first_session, first_bridge, _first_rx) = installed(session, "conn-first");
        let first_turn = acp_session::claim_turn(session, Some("conn-first"));
        assert!(for_session(session).is_some());

        let (second_session, second_bridge, _second_rx) = installed(session, "conn-second");
        assert!(
            for_session(session).is_none(),
            "a re-subscribe starts a new attachment with no turn of its own yet"
        );
        let second_turn = acp_session::claim_turn(session, Some("conn-second"));
        assert!(for_session(session).is_some());

        clear(session, first_bridge);
        acp_session::clear(session, first_session);
        drop(first_turn);
        assert!(
            for_session(session).is_some(),
            "the previous attachment's disconnect and guard must not withdraw the newer bridge"
        );

        drop(second_turn);
        clear(session, second_bridge);
        acp_session::clear(session, second_session);
        assert!(for_session(session).is_none());
    }
}
