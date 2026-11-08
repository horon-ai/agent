//! The ACP connection: initialize gate, read loop, request routing, and
//! cancellation of in-flight client requests.
//!
//! The read loop never awaits a request handler. Each request is registered in
//! the in-flight registry first (keyed by its raw JSON id), then runs on its
//! own task; requests naming the same `sessionId` run one after another in
//! arrival order, except `session/prompt`, which runs concurrently.
//! A session's FIFO lives only as long as its session: once the session is
//! closed, deleted, or never existed, the FIFO retires itself after the job
//! that found out, so a connection keeps no idle worker per id it has seen.
//! Notifications are handled immediately and never answered.

use super::v2::negotiate::AcpVersion;
use super::*;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicUsize;
use tokio::io::AsyncRead;
use tokio::sync::{Notify, mpsc};

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;
/// Opens one daemon connection for a session handler.
///
/// Production dials the daemon socket; a unit test replaces it with a scripted
/// [`Stream::pair`](crate::transport::Stream) so the daemon end can refuse or
/// stay silent on purpose.
pub(super) type DaemonConnector = Arc<dyn Fn() -> DaemonConnect + Send + Sync>;
/// What the session catalog knows about one session, for a v2 resume.
pub(super) type SessionCatalog = Arc<dyn Fn(&str) -> Option<CatalogInfo> + Send + Sync>;
pub(super) type DaemonConnect = Pin<Box<dyn Future<Output = Result<(ReadHalf, WriteHalf)>> + Send>>;
/// Request id -> (sessionId, signal) for each prompt not yet answered.
type UnansweredPrompts = HashMap<RequestKey, (Option<String>, Arc<CancelSignal>)>;

#[derive(Debug)]
pub(super) struct ConnectionState {
    pub(super) initialized: bool,
    pub(super) client_capabilities: Value,
    pub(super) client_info: Option<Value>,
    /// Answer ACP v2 to a client that asks for it (IS-1). Off is a true
    /// rollback: every request is answered exactly as version 1 answers it.
    pub(super) protocol_v2: bool,
    /// The surface this connection speaks; `None` until `initialize` selects
    /// one.
    pub(super) version: Option<AcpVersion>,
    /// A request other than `initialize` arrived on this connection, so a v2
    /// connection may not negotiate again.
    pub(super) other_request_seen: bool,
    /// `auth/logout` on a v2 connection: the session methods answer -32000
    /// until the next `auth/login` on this same connection (IS-15).
    pub(super) logged_out: bool,
}

impl Default for ConnectionState {
    fn default() -> Self {
        Self {
            initialized: false,
            client_capabilities: Value::Null,
            client_info: None,
            protocol_v2: true,
            version: None,
            other_request_seen: false,
            logged_out: false,
        }
    }
}

/// A cancel recorded for one prompt by `session/cancel`, `$/cancel_request`
/// or `session/close`, and whether the prompt has written its response.
#[derive(Default)]
pub(super) struct CancelSignal {
    recorded: AtomicBool,
    notify: Notify,
    answered: AtomicBool,
    answered_notify: Notify,
    /// The prompt's message is on the daemon socket, so a daemon `Cancel` can
    /// reach its turn.
    turn_written: AtomicBool,
    /// The one daemon `Cancel` this prompt may send has been sent, or the
    /// prompt's turn is over and none may be sent any more.
    daemon_cancel_used: AtomicBool,
}

impl CancelSignal {
    pub(super) fn cancel(&self) {
        self.recorded.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }

    pub(super) fn is_recorded(&self) -> bool {
        self.recorded.load(Ordering::SeqCst)
    }

    pub(super) async fn recorded(&self) {
        while !self.is_recorded() {
            self.notify.notified().await;
        }
    }

    /// Mark the prompt's message as written to the daemon. True when a cancel
    /// was already recorded and this caller now owns the daemon `Cancel`.
    ///
    /// Both sides store their flag before they read the other's (SeqCst), so
    /// a cancel racing the write is sent by at least one of them, and
    /// [`Self::claim_daemon_cancel`] makes it at most one.
    pub(super) fn mark_turn_written(&self) -> bool {
        self.turn_written.store(true, Ordering::SeqCst);
        self.is_recorded() && self.claim_daemon_cancel()
    }

    /// Record the cancel; true when this caller must send the daemon `Cancel`:
    /// the turn is on the daemon and no `Cancel` was sent for it yet. A second
    /// `session/cancel`, or `session/cancel` plus `$/cancel_request`, of the
    /// same prompt therefore never reaches the daemon, whose idle path would
    /// answer it with a lone `Interrupted`
    /// (crates/app-core/src/server/client_lifecycle.rs:4154-4164).
    pub(super) fn cancel_turn(&self) -> bool {
        self.cancel();
        self.turn_written.load(Ordering::SeqCst) && self.claim_daemon_cancel()
    }

    fn claim_daemon_cancel(&self) -> bool {
        !self.daemon_cancel_used.swap(true, Ordering::SeqCst)
    }

    /// The prompt's turn is over: no daemon `Cancel` may be sent for it any
    /// more. True when one was sent, so the daemon may still answer it.
    pub(super) fn retire_daemon_cancel(&self) -> bool {
        self.daemon_cancel_used.swap(true, Ordering::SeqCst)
    }

    fn mark_answered(&self) {
        self.answered.store(true, Ordering::SeqCst);
        self.answered_notify.notify_waiters();
    }

    /// Resolves once the request's handler has returned, so its response
    /// line is already on stdout.
    pub(super) async fn answered(&self) {
        loop {
            let notified = self.answered_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.answered.load(Ordering::SeqCst) {
                return;
            }
            notified.await;
        }
    }
}

/// Where one request's response is written: the connection, or the collector
/// of the JSON-RPC batch that carried the request on a v2 connection (IS-3).
#[derive(Clone)]
pub(super) enum Reply {
    Direct,
    Batch(Arc<BatchReply>),
}

/// The collector of one JSON-RPC batch. Every entry records its answer here and
/// the entry that records the last one writes the response array, so a batch
/// needs no task of its own and its answer races process exit exactly as a
/// single request's answer does.
pub(super) struct BatchReply {
    /// Answers still owed, one from every entry that will answer.
    open: AtomicUsize,
    answers: StdMutex<Vec<Value>>,
}

impl BatchReply {
    fn new(entries: usize) -> Self {
        Self {
            open: AtomicUsize::new(entries),
            answers: StdMutex::default(),
        }
    }

    /// Record one answer; `Some(array)` when it was the last one owed, so this
    /// caller writes the response array.
    fn record(&self, answer: Value) -> Option<Value> {
        let mut answers = self.answers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        answers.push(answer);
        let last = self.open.fetch_sub(1, Ordering::SeqCst) == 1;
        last.then(|| Value::Array(std::mem::take(&mut *answers)))
    }
}

/// `initialize` after the connection's first business is refused (IS-1).
fn late_initialize_error() -> RpcError {
    RpcError::new(JSONRPC_INVALID_REQUEST, "initialize must precede other requests")
}

/// The answer of a batch entry whose request id is null (IS-3).
fn null_id_frame() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": Value::Null,
        "error": { "code": JSONRPC_INVALID_REQUEST, "message": "JSON-RPC id must not be null" },
    })
}

/// The answer of a batch entry that is not a JSON-RPC message at all.
fn invalid_request_frame(code: i64, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": Value::Null,
        "error": { "code": code, "message": format!("Invalid JSON-RPC request: {message}") },
    })
}

struct InflightEntry {
    method: String,
    session_id: Option<String>,
    answered_cancelled: bool,
    cancel: Arc<CancelSignal>,
    reply: Reply,
}

pub(super) enum CancelTarget {
    Prompt {
        signal: Arc<CancelSignal>,
        session_id: Option<String>,
    },
    AnswerCancelled {
        reply: Reply,
    },
    Unknown,
}

#[derive(Default)]
pub(super) struct InflightRequests {
    entries: StdMutex<HashMap<RequestKey, InflightEntry>>,
}

impl InflightRequests {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<RequestKey, InflightEntry>> {
        self.entries.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Register `id`, or give the reply back when it is already in flight and
    /// `reject_duplicate` is set. JSON-RPC ids identify one pending request,
    /// and overwriting an entry would leave the first request unanswered —
    /// inside a v2 batch that means the collector stays one answer short and
    /// never writes its array. A v1 connection keeps replacing the entry, as it
    /// always did (IS-16).
    fn register(
        &self,
        id: &Value,
        method: &str,
        session_id: Option<String>,
        reply: Reply,
        reject_duplicate: bool,
    ) -> std::result::Result<Arc<CancelSignal>, Reply> {
        let key = RequestKey::of(id);
        let mut entries = self.lock();
        if reject_duplicate && entries.contains_key(&key) {
            return Err(reply);
        }
        let cancel = Arc::new(CancelSignal::default());
        entries.insert(
            key,
            InflightEntry {
                method: method.to_string(),
                session_id,
                answered_cancelled: false,
                cancel: Arc::clone(&cancel),
                reply,
            },
        );
        Ok(cancel)
    }

    /// Retire `id` before writing its response. `None` when the request was
    /// already answered with -32800, so the late result must be suppressed;
    /// an unregistered id answers directly on the connection.
    fn finish(&self, id: &Value) -> Option<Reply> {
        match self.lock().remove(&RequestKey::of(id)) {
            Some(entry) if entry.answered_cancelled => None,
            Some(entry) => Some(entry.reply),
            None => Some(Reply::Direct),
        }
    }

    /// Like `finish`, but only for a request that is still registered and
    /// unanswered.
    fn take_unanswered(&self, id: &Value) -> Option<Reply> {
        match self.lock().remove(&RequestKey::of(id)) {
            Some(entry) if !entry.answered_cancelled => Some(entry.reply),
            _ => None,
        }
    }

    fn cancel(&self, id: &Value) -> CancelTarget {
        let mut entries = self.lock();
        let Some(entry) = entries.get_mut(&RequestKey::of(id)) else {
            return CancelTarget::Unknown;
        };
        if entry.method == "session/prompt" {
            return CancelTarget::Prompt {
                signal: Arc::clone(&entry.cancel),
                session_id: entry.session_id.clone(),
            };
        }
        if entry.answered_cancelled {
            return CancelTarget::Unknown;
        }
        entry.answered_cancelled = true;
        CancelTarget::AnswerCancelled {
            reply: entry.reply.clone(),
        }
    }

    pub(super) fn prompt_signals(&self, session_id: &str) -> Vec<Arc<CancelSignal>> {
        self.lock()
            .values()
            .filter(|entry| entry.method == "session/prompt" && entry.session_id.as_deref() == Some(session_id))
            .map(|entry| Arc::clone(&entry.cancel))
            .collect()
    }
}

pub(super) struct IncomingRequest {
    pub(super) id: Value,
    pub(super) method: String,
    pub(super) params: Value,
    pub(super) cancel: Arc<CancelSignal>,
}

#[derive(Clone)]
pub(super) struct AcpRuntime {
    pub(super) peer: Arc<RpcPeer>,
    pub(super) connection: Arc<StdMutex<ConnectionState>>,
    pub(super) inflight: Arc<InflightRequests>,
    session_queues: Arc<StdMutex<HashMap<String, mpsc::UnboundedSender<Job>>>>,
    pub(super) sessions: Arc<Mutex<HashMap<String, Arc<DaemonSession>>>>,
    /// `session/prompt` requests whose response is not written yet, by
    /// request id. Unlike the in-flight registry, an entry stays until the
    /// handler has returned.
    prompts: Arc<StdMutex<UnansweredPrompts>>,
    /// Set once the daemon has reported a usable model.
    pub(super) ready: Arc<AtomicBool>,
    pub(super) profile: AcpProfile,
    pub(super) provider_choice: ProviderChoice,
    pub(super) model: Option<String>,
    pub(super) provider_profile: Option<String>,
    /// Deadlines of the client calls the session side channels make.
    pub(super) call_bounds: CallBounds,
    /// Deadlines on a `session/load` or `session/resume` attach.
    pub(super) attach_bounds: AttachBounds,
    /// `None` dials the daemon socket; `Some` is a scripted daemon (tests).
    pub(super) daemon_connector: Option<DaemonConnector>,
    /// `None` reads the host-local session catalog; `Some` is a scripted one
    /// (tests).
    pub(super) session_catalog: Option<SessionCatalog>,
}

/// One session's request FIFO: the entry in [`AcpRuntime::session_queues`] and
/// the worker task that drains it.
///
/// The worker outlives no session: when a job finishes and the session is gone,
/// the worker removes its own entry - then ends - unless a job is still queued,
/// which it drains first. Removing the entry under the map lock is what makes
/// the emptiness check race-free against `enqueue_for_session`, which sends
/// while holding that same lock.
struct SessionQueue {
    queues: Arc<StdMutex<HashMap<String, mpsc::UnboundedSender<Job>>>>,
    sessions: Arc<Mutex<HashMap<String, Arc<DaemonSession>>>>,
    session_id: String,
    sender: mpsc::UnboundedSender<Job>,
}

impl SessionQueue {
    async fn session_is_gone(&self) -> bool {
        !self.sessions.lock().await.contains_key(&self.session_id)
    }

    /// True when this FIFO holds nothing more, so its worker may end.
    fn retire_when_idle(&self, jobs: &mpsc::UnboundedReceiver<Job>) -> bool {
        let mut queues = self.queues.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !jobs.is_empty() {
            return false;
        }
        if queues
            .get(&self.session_id)
            .is_some_and(|current| current.same_channel(&self.sender))
        {
            queues.remove(&self.session_id);
        }
        true
    }
}

impl AcpRuntime {
    pub(super) fn new(
        profile: AcpProfile,
        provider_choice: ProviderChoice,
        model: Option<String>,
        provider_profile: Option<String>,
        output: OutputSink,
    ) -> Self {
        Self {
            peer: Arc::new(RpcPeer::new(output)),
            connection: Arc::default(),
            inflight: Arc::default(),
            session_queues: Arc::default(),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            prompts: Arc::default(),
            ready: Arc::default(),
            profile,
            provider_choice,
            model,
            provider_profile,
            call_bounds: CallBounds::default(),
            attach_bounds: AttachBounds::default(),
            daemon_connector: None,
            session_catalog: None,
        }
    }

    pub(super) async fn run(self) -> Result<()> {
        self.serve(tokio::io::stdin()).await
    }

    pub(super) async fn serve<R: AsyncRead + Unpin>(self, input: R) -> Result<()> {
        let mut reader = BufReader::new(input);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await? == 0 {
                return Ok(());
            }
            if line.trim().is_empty() {
                continue;
            }
            // A JSON array is a JSON-RPC batch on a v2 connection (IS-3). A v1
            // connection, and every connection before `initialize`, keeps the
            // single -32600 the parser answers with.
            if line.trim_start().starts_with('[')
                && self.spoken_version() == Some(AcpVersion::V2)
                && self.handle_batch(&line).await?
            {
                continue;
            }
            match JsonRpcMessage::parse(&line) {
                Ok(JsonRpcMessage::Request { id, method, params }) => self.on_request(id, method, params).await?,
                Ok(JsonRpcMessage::Notification { method, params }) => self.on_notification(&method, &params).await,
                Ok(JsonRpcMessage::Response { id, result }) => self.peer.outbound.resolve(&id, result),
                Err((code, message)) => {
                    let id = JsonRpcMessage::error_reply_id(&line);
                    self.peer
                        .write_error(id, &RpcError::new(code, format!("Invalid JSON-RPC request: {message}")))
                        .await?;
                }
            }
        }
    }

    fn is_initialized(&self) -> bool {
        self.lock_connection().initialized
    }

    /// The surface this connection speaks; `None` before `initialize`.
    pub(super) fn spoken_version(&self) -> Option<AcpVersion> {
        self.lock_connection().version
    }

    pub(super) fn lock_connection(&self) -> std::sync::MutexGuard<'_, ConnectionState> {
        self.connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    async fn on_request(&self, id: Value, method: String, params: Value) -> Result<()> {
        if id.is_null() && self.spoken_version() == Some(AcpVersion::V2) {
            return self
                .peer
                .write_error(
                    Value::Null,
                    &RpcError::new(JSONRPC_INVALID_REQUEST, "JSON-RPC id must not be null"),
                )
                .await;
        }
        self.dispatch_incoming(id, method, params, Reply::Direct).await
    }

    /// Route one incoming request: `initialize`, the initialized gate, extension
    /// rejection, then the concurrent request path with `reply` collecting its
    /// response.
    async fn dispatch_incoming(&self, id: Value, method: String, params: Value, reply: Reply) -> Result<()> {
        if method == "initialize" {
            // The spec asks senders not to batch lifecycle-sensitive messages
            // such as `initialize` (spec transports L26674-26680), and a batch
            // exists only after a version is negotiated, so an initialize entry
            // is always a repeat after another request: it answers per entry.
            if let Reply::Batch(_) = &reply {
                return self.send_reply(Some(reply), id, Err(late_initialize_error())).await;
            }
            return self.handle_initialize(id, &params).await;
        }
        // Any other request closes the window in which a v2 connection may
        // negotiate again (IS-1).
        self.lock_connection().other_request_seen = true;
        if !self.is_initialized() {
            return self
                .send_reply(
                    Some(reply),
                    id,
                    Err(RpcError::new(JSONRPC_INVALID_REQUEST, "connection not initialized")),
                )
                .await;
        }
        if method.starts_with('_') {
            return self
                .send_reply(
                    Some(reply),
                    id,
                    Err(RpcError::new(
                        JSONRPC_METHOD_NOT_FOUND,
                        format!("Unsupported HoronAgent ACP extension method: {method}"),
                    )),
                )
                .await;
        }

        let session_id = params.get("sessionId").and_then(Value::as_str).map(str::to_string);
        let reject_duplicate = self.spoken_version() == Some(AcpVersion::V2);
        let cancel = match self
            .inflight
            .register(&id, &method, session_id.clone(), reply, reject_duplicate)
        {
            Ok(cancel) => cancel,
            Err(reply) => {
                return self
                    .send_reply(
                        Some(reply),
                        id.clone(),
                        Err(RpcError::new(
                            JSONRPC_INVALID_REQUEST,
                            format!("Request id {id} is already in flight"),
                        )),
                    )
                    .await;
            }
        };
        let prompt = (method == "session/prompt").then(|| {
            let key = RequestKey::of(&id);
            self.lock_prompts()
                .insert(key.clone(), (session_id.clone(), Arc::clone(&cancel)));
            key
        });
        let ordered_session = match method.as_str() {
            "session/prompt" | "session/cancel" => None,
            _ => session_id,
        };
        let runtime = self.clone();
        let job: Job = Box::pin(async move {
            let answered = Arc::clone(&cancel);
            runtime
                .handle_request(IncomingRequest {
                    id,
                    method,
                    params,
                    cancel,
                })
                .await;
            if let Some(key) = prompt {
                runtime.lock_prompts().remove(&key);
            }
            answered.mark_answered();
        });
        match ordered_session {
            Some(session_id) => self.enqueue_for_session(session_id, job),
            None => {
                tokio::spawn(job);
            }
        }
        Ok(())
    }

    fn enqueue_for_session(&self, session_id: String, job: Job) {
        let mut queues = self
            .session_queues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let job = match queues.get(&session_id) {
            Some(queue) => match queue.send(job) {
                Ok(()) => return,
                Err(mpsc::error::SendError(job)) => job,
            },
            None => job,
        };
        let (queue, mut jobs) = mpsc::unbounded_channel::<Job>();
        let worker = SessionQueue {
            queues: Arc::clone(&self.session_queues),
            sessions: Arc::clone(&self.sessions),
            session_id: session_id.clone(),
            sender: queue.clone(),
        };
        tokio::spawn(async move {
            while let Some(job) = jobs.recv().await {
                job.await;
                if worker.session_is_gone().await && worker.retire_when_idle(&jobs) {
                    break;
                }
            }
        });
        let _ = queue.send(job);
        queues.insert(session_id, queue);
    }

    /// How many per-session FIFOs this connection keeps. Test-only: the map is
    /// internal bookkeeping whose size no caller needs.
    #[cfg(test)]
    pub(super) fn session_queue_len(&self) -> usize {
        self.session_queues
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    async fn handle_request(&self, request: IncomingRequest) {
        let id = request.id.clone();
        let method = request.method.clone();
        let result = if self.spoken_version() == Some(AcpVersion::V2) {
            self.handle_request_v2(request).await
        } else {
            self.handle_request_v1(request).await
        };
        if let Err(err) = result {
            crate::logging::warn(&format!("ACP: {method} failed: {err:#}"));
            let reply = self.inflight.take_unanswered(&id);
            if reply.is_some() {
                let error = RpcError::new(JSONRPC_INTERNAL_ERROR, format!("{method} failed: {err:#}"));
                if let Err(err) = self.send_reply(reply, id, Err(error)).await {
                    crate::logging::warn(&format!("ACP: failed to write error response: {err:#}"));
                }
            }
        }
    }

    /// The v1 method table (IS-16: byte for byte what the v1 plan ships).
    async fn handle_request_v1(&self, request: IncomingRequest) -> Result<()> {
        let id = request.id.clone();
        let method = request.method.clone();
        match method.as_str() {
            "authenticate" => self.handle_authenticate(request).await,
            "session/new" => self.handle_session_new(request).await,
            "session/load" => self.handle_session_load(request, true).await,
            "session/resume" => self.handle_session_load(request, false).await,
            "session/prompt" => self.handle_session_prompt(request).await,
            "session/cancel" => self.handle_session_cancel_request(request).await,
            "session/close" => self.handle_session_close(request).await,
            "session/list" => self.handle_session_list(request).await,
            "session/delete" => self.handle_session_delete(request).await,
            "session/set_config_option" => self.handle_set_config_option(request).await,
            "session/set_mode" => self.handle_set_mode(request).await,
            "session/set_model" => {
                self.handle_compat_config_option(request, CONFIG_ID_MODEL, &["modelId", "model"], "session/set_model")
                    .await
            }
            "session/set_reasoning_effort" => {
                self.handle_compat_config_option(
                    request,
                    CONFIG_ID_EFFORT,
                    &["effort", "reasoningEffort"],
                    "session/set_reasoning_effort",
                )
                .await
            }
            _ => {
                self.write_error_value(
                    id.clone(),
                    JSONRPC_METHOD_NOT_FOUND,
                    format!("Unsupported ACP method: {method}"),
                )
                .await
            }
        }
    }

    /// The v2 method table (IS-3): exactly the stable meta set, and the session
    /// methods gated by this connection's own `auth/logout` (IS-15).
    async fn handle_request_v2(&self, request: IncomingRequest) -> Result<()> {
        let id = request.id.clone();
        let method = request.method.clone();
        if self.lock_connection().logged_out
            && matches!(method.as_str(), "session/new" | "session/resume" | "session/prompt")
        {
            return self
                .write_error_value(id, ACP_AUTH_REQUIRED, "Authentication required".to_string())
                .await;
        }
        match method.as_str() {
            "auth/login" => self.handle_auth_login(request).await,
            "auth/logout" => self.handle_auth_logout(request).await,
            "session/new" => self.handle_session_new_v2(request).await,
            "session/resume" => self.handle_session_resume_v2(request).await,
            "session/prompt" => self.handle_session_prompt_v2(request).await,
            "session/cancel" => self.handle_session_cancel_request(request).await,
            "session/close" => self.handle_session_close(request).await,
            "session/list" => self.handle_session_list(request).await,
            "session/delete" => self.handle_session_delete(request).await,
            "session/set_config_option" => self.handle_set_config_option(request).await,
            "tool_call/cancel" => self.handle_tool_call_cancel(request).await,
            "terminal/cancel" => self.handle_terminal_cancel(request).await,
            // Every other method, removed or unknown, is not part of the v2
            // surface: `session/load`, `session/set_mode`, `authenticate`,
            // `logout`, `session/set_model`, `session/set_reasoning_effort`
            // and anything a later draft adds.
            _ => {
                self.write_error_value(
                    id.clone(),
                    JSONRPC_METHOD_NOT_FOUND,
                    format!("Unsupported ACP method: {method}"),
                )
                .await
            }
        }
    }

    async fn handle_initialize(&self, id: Value, params: &Value) -> Result<()> {
        let (protocol_v2, current, other_request_seen) = {
            let connection = self.lock_connection();
            (
                connection.protocol_v2,
                connection.version,
                connection.other_request_seen,
            )
        };
        if current == Some(AcpVersion::V2) && other_request_seen {
            return self.peer.write_error(id, &late_initialize_error()).await;
        }
        let version = match v2::negotiate::select(params, protocol_v2) {
            Ok(version) => version,
            // A repeat that selects nothing leaves the initialization already
            // in place untouched.
            Err(error) => return self.peer.write_error(id, &error.to_rpc()).await,
        };
        // A connection that already served requests keeps the surface it
        // negotiated: its subscriptions were built for that version, and a v1
        // one may carry the client fs and terminal capabilities v2 never uses.
        if other_request_seen && current.is_some_and(|current| current != version) {
            return self.peer.write_error(id, &late_initialize_error()).await;
        }
        // v1 names the client's fields `clientCapabilities` and `clientInfo`; v2
        // names them `capabilities` and `info`, so a v2 client's elicitation
        // support is read from where it sends it.
        let (capabilities_key, info_key) = match version {
            AcpVersion::V1 => ("clientCapabilities", "clientInfo"),
            AcpVersion::V2 => ("capabilities", "info"),
        };
        let capabilities = params.get(capabilities_key).cloned().unwrap_or_else(|| json!({}));
        let result = match version {
            AcpVersion::V1 => initialize_result(self.profile),
            AcpVersion::V2 => v2::shapes::initialize_response(self.profile, MCP_REMOTE_TRANSPORTS.http),
        };
        {
            let mut connection = self.lock_connection();
            connection.initialized = true;
            connection.version = Some(version);
            connection.client_capabilities = capabilities;
            connection.client_info = params.get(info_key).cloned();
        }
        self.peer
            .write_value(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
            .await
    }

    async fn on_notification(&self, method: &str, params: &Value) {
        if !self.is_initialized() {
            return;
        }
        match method {
            "session/cancel" => {
                if let Ok(session_id) = required_session_id(params) {
                    self.cancel_session_turn(&session_id).await;
                }
            }
            "$/cancel_request" => self.handle_cancel_request(params).await,
            _ => {}
        }
    }

    async fn handle_cancel_request(&self, params: &Value) {
        let Some(request_id) = params.get("requestId") else {
            return;
        };
        match self.inflight.cancel(request_id) {
            CancelTarget::Prompt { signal, session_id } if self.spoken_version() == Some(AcpVersion::V2) => {
                signal.cancel();
                if let Some(session_id) = session_id {
                    self.cancel_prompt_request_v2(&session_id, request_id).await;
                }
            }
            CancelTarget::Prompt { signal, session_id } => {
                let send = signal.cancel_turn();
                if let Some(session_id) = session_id {
                    self.send_daemon_cancel(&session_id, send).await;
                }
            }
            CancelTarget::AnswerCancelled { reply } => {
                if let Err(err) = self
                    .send_reply(Some(reply), request_id.clone(), Err(RpcError::cancelled()))
                    .await
                {
                    crate::logging::warn(&format!("ACP: failed to answer $/cancel_request: {err:#}"));
                }
            }
            CancelTarget::Unknown => {}
        }
    }

    /// Cancel the session's in-flight prompt. With no prompt in flight this is
    /// a no-op: ACP cancels an ongoing prompt turn, and a daemon `Cancel` for
    /// a session with no running turn only produces a lone `Interrupted`
    /// (client_lifecycle.rs:4154-4164) that the session's next prompt would
    /// read as its own.
    pub(super) async fn cancel_session_turn(&self, session_id: &str) {
        if self.spoken_version() == Some(AcpVersion::V2) {
            let _ = self.cancel_session_v2(session_id).await;
            return;
        }
        let signals = self.inflight.prompt_signals(session_id);
        if signals.is_empty() {
            return;
        }
        let mut send = false;
        for signal in signals {
            send |= signal.cancel_turn();
        }
        self.send_daemon_cancel(session_id, send).await;
    }

    /// Cancel the session's turn in the daemon when `send` says this cancel
    /// owns the prompt's one daemon `Cancel`, then every permission request
    /// and client call of that turn the client has not answered yet. The
    /// daemon sees the cancel before the `deny` or -32800 each withdrawn
    /// request answers with.
    async fn send_daemon_cancel(&self, session_id: &str, send: bool) {
        let session = self.sessions.lock().await.get(session_id).cloned();
        if let Some(session) = session
            && send
        {
            let cancel_id = session.next_id();
            let _ = session.send(&Request::Cancel { id: cancel_id }).await;
        }
        self.withdraw_session_calls(session_id).await;
    }

    /// Withdraw every permission request and client call of the session the
    /// client has not answered yet, without a daemon `Cancel`. Closing a
    /// session ends the calls still open even when no prompt is in flight; a
    /// withdrawal is a no-op for a call the peer already cancelled, so this
    /// runs safely after [`Self::send_daemon_cancel`] as well.
    pub(super) async fn withdraw_session_calls(&self, session_id: &str) {
        let session = self.sessions.lock().await.get(session_id).cloned();
        if let Some(session) = session {
            session.permissions.cancel_open(&self.peer).await;
            session.client_calls.cancel_open(&self.peer).await;
        }
    }

    pub(super) async fn write_result(&self, id: Value, result: Value) -> Result<()> {
        let reply = self.inflight.finish(&id);
        self.send_reply(reply, id, Ok(result)).await
    }

    pub(super) async fn write_error_value(&self, id: Value, code: i64, message: String) -> Result<()> {
        self.write_rpc_error(id, RpcError::new(code, message)).await
    }

    pub(super) async fn write_rpc_error(&self, id: Value, error: RpcError) -> Result<()> {
        let reply = self.inflight.finish(&id);
        self.send_reply(reply, id, Err(error)).await
    }

    /// Write one response to where its request came from: the connection, or
    /// the batch collecting this batch entry's answers. `None` suppresses a
    /// result the request was already answered for.
    async fn send_reply(
        &self,
        reply: Option<Reply>,
        id: Value,
        result: std::result::Result<Value, RpcError>,
    ) -> Result<()> {
        let Some(reply) = reply else {
            return Ok(());
        };
        let value = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({ "jsonrpc": "2.0", "id": id, "error": error.to_wire() }),
        };
        match reply {
            Reply::Direct => self.peer.write_value(value).await,
            Reply::Batch(collector) => {
                // The entry that owes the last answer writes the whole array.
                if let Some(array) = collector.record(value) {
                    self.peer.write_value(array).await?;
                }
                Ok(())
            }
        }
    }

    /// One JSON-RPC batch line on a v2 connection (IS-3). Every request entry
    /// runs on the normal concurrent path and answers into one collector; the
    /// entry that owes the last answer writes the single response array, so the
    /// read loop never waits for a handler and no task is spawned for the
    /// batch. Nothing is written when the batch held no entry that answers.
    /// False when the line is no JSON array, so the caller keeps the single
    /// -32600.
    async fn handle_batch(&self, line: &str) -> Result<bool> {
        let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(line) else {
            return Ok(false);
        };
        if entries.is_empty() {
            return self
                .peer
                .write_error(
                    Value::Null,
                    &RpcError::new(JSONRPC_INVALID_REQUEST, "JSON-RPC batch must not be empty"),
                )
                .await
                .map(|()| true);
        }

        enum Entry {
            Request {
                id: Value,
                method: String,
                params: Value,
            },
            Notification {
                method: String,
                params: Value,
            },
            Response {
                id: Value,
                result: std::result::Result<Value, RpcError>,
            },
            /// An entry the batch answers itself.
            Answered(Value),
        }

        let mut plan = Vec::with_capacity(entries.len());
        for entry in &entries {
            plan.push(match JsonRpcMessage::parse(&entry.to_string()) {
                Ok(JsonRpcMessage::Request { id, method, params }) => {
                    if id.is_null() {
                        Entry::Answered(null_id_frame())
                    } else {
                        Entry::Request { id, method, params }
                    }
                }
                Ok(JsonRpcMessage::Notification { method, params }) => Entry::Notification { method, params },
                Ok(JsonRpcMessage::Response { id, result }) => Entry::Response { id, result },
                Err((code, message)) => Entry::Answered(invalid_request_frame(code, message)),
            });
        }

        let answered = plan.iter().filter(|entry| matches!(entry, Entry::Answered(_))).count();
        let pending = answered
            + plan
                .iter()
                .filter(|entry| matches!(entry, Entry::Request { .. }))
                .count();
        if pending == 0 {
            for entry in plan {
                match entry {
                    Entry::Notification { method, params } => self.on_notification(&method, &params).await,
                    Entry::Response { id, result } => self.peer.outbound.resolve(&id, result),
                    Entry::Request { .. } | Entry::Answered(_) => {}
                }
            }
            return Ok(true);
        }
        let collector = Arc::new(BatchReply::new(pending));
        let mut array = None;
        for entry in plan {
            match entry {
                Entry::Request { id, method, params } => {
                    self.dispatch_incoming(id, method, params, Reply::Batch(Arc::clone(&collector)))
                        .await?;
                }
                Entry::Notification { method, params } => self.on_notification(&method, &params).await,
                Entry::Response { id, result } => self.peer.outbound.resolve(&id, result),
                Entry::Answered(frame) => array = collector.record(frame).or(array),
            }
        }
        if let Some(array) = array {
            self.peer.write_value(array).await?;
        }
        Ok(true)
    }

    pub(super) fn client_capabilities(&self) -> Value {
        self.lock_connection().client_capabilities.clone()
    }

    fn lock_prompts(&self) -> std::sync::MutexGuard<'_, UnansweredPrompts> {
        self.prompts.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The prompts of `session_id` that have not written their response.
    pub(super) fn unanswered_prompts(&self, session_id: &str) -> Vec<Arc<CancelSignal>> {
        self.lock_prompts()
            .values()
            .filter(|(session, _)| session.as_deref() == Some(session_id))
            .map(|(_, signal)| Arc::clone(signal))
            .collect()
    }

    pub(super) async fn write_notification(&self, method: &str, params: Value) -> Result<()> {
        self.peer
            .write_value(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }
}
