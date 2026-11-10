//! One long-lived language server process, spoken to over JSON-RPC.
//!
//! Clients are cached process-wide by `(server, workspace root)`: starting
//! rust-analyzer costs seconds and indexes the whole workspace, so a second
//! question about the same project must reuse the first process. A client that
//! reaches ten idle minutes shuts itself down, and one whose process died is
//! replaced on the next request.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex as StdMutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, Notify, mpsc, oneshot};
use url::Url;

use crate::server::ServerSpec;
use crate::{
    ClientStatus, CodeAction, Diagnostic, DiagnosticSeverity, FileEdit, Location, Symbol, TextEdit, WorkspaceEdit,
};

/// A server with no request for this long is shut down.
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// How often the idle monitor looks at a client.
const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(60);
/// `initialize` waits for indexing setup, which can be slower than a query.
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);
/// Time a shutting-down server gets to exit on its own.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

type PendingMap = Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>;
type DiagnosticsMap = Mutex<HashMap<String, DiagnosticsState>>;

struct DiagnosticsState {
    /// Bumped on every publish, so a waiter can tell a fresh report from a
    /// stale one that arrived before its own `didOpen`.
    generation: u64,
    items: Vec<Diagnostic>,
}

/// A running language server and the state needed to talk to it.
pub struct Client {
    spec: &'static ServerSpec,
    root: PathBuf,
    pid: u32,
    /// Held (shared with the reader task, which reaps it on EOF) only to kill
    /// the process; the reader and writer tasks own the pipes.
    child: Arc<StdMutex<Option<Child>>>,
    writer: mpsc::UnboundedSender<Vec<u8>>,
    pending: Arc<PendingMap>,
    diagnostics: Arc<DiagnosticsMap>,
    diagnostics_notify: Arc<Notify>,
    /// Files already opened, and the last `didChange` version sent for each.
    opened: Mutex<HashMap<PathBuf, i32>>,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
    last_used_millis: AtomicU64,
}

impl Client {
    /// Return the live client for `(spec, root)`, starting one if needed.
    pub async fn get(spec: &'static ServerSpec, root: PathBuf) -> Result<Arc<Client>> {
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let key = (spec.name.to_string(), root.clone());
        if let Some(client) = cache().lock().await.get(&key).filter(|client| client.is_alive()) {
            client.touch();
            return Ok(Arc::clone(client));
        }

        let client = Arc::new(Self::spawn(spec, root.clone()).await?);
        {
            let mut cache = cache().lock().await;
            if let Some(existing) = cache.get(&key).filter(|client| client.is_alive()).cloned() {
                existing.touch();
                return Ok(existing);
            }
            cache.insert(key, Arc::clone(&client));
        }
        spawn_idle_monitor(&client);
        Ok(client)
    }

    /// The server's short name.
    pub fn server_name(&self) -> &'static str {
        self.spec.name
    }

    /// The workspace root the server was started in.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The server process id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Whether the process is still answering.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Whole seconds since the last request that touched this client.
    pub fn idle_secs(&self) -> u64 {
        let now = now_millis();
        now.saturating_sub(self.last_used_millis.load(Ordering::Relaxed)) / 1000
    }

    /// Diagnostics for one file, waiting until the server publishes a report
    /// for the content currently on disk or `timeout` elapses.
    pub async fn diagnostics(&self, path: &Path, timeout: Duration) -> Result<Vec<Diagnostic>> {
        let path = canonical(path);
        let uri = path_to_uri(&path);
        self.touch();

        let deadline = Instant::now() + timeout;
        let baseline = self
            .diagnostics
            .lock()
            .await
            .get(&uri)
            .map(|state| state.generation)
            .unwrap_or(0);
        let notified = self.diagnostics_notify.notified();
        tokio::pin!(notified);

        self.ensure_open(&path).await?;

        loop {
            if let Some(state) = self.diagnostics.lock().await.get(&uri)
                && state.generation > baseline
            {
                return Ok(state.items.clone());
            }
            let now = Instant::now();
            if now >= deadline {
                return Ok(self
                    .diagnostics
                    .lock()
                    .await
                    .get(&uri)
                    .map(|state| state.items.clone())
                    .unwrap_or_default());
            }
            let _ = tokio::time::timeout(deadline - now, notified.as_mut()).await;
        }
    }

    /// Places the definition of the symbol at `line`/`column` (both 1-based).
    pub async fn definition(&self, path: &Path, line: u32, column: u32, timeout: Duration) -> Result<Vec<Location>> {
        let path = canonical(path);
        self.touch();
        self.ensure_open(&path).await?;
        let value = self
            .request("textDocument/definition", position_params(&path, line, column), timeout)
            .await?;
        if value.is_null() {
            return Ok(Vec::new());
        }
        let response: lsp_types::GotoDefinitionResponse =
            serde_json::from_value(value).context("unexpected textDocument/definition response")?;
        Ok(match response {
            lsp_types::GotoDefinitionResponse::Scalar(location) => vec![location_from_lsp(location)],
            lsp_types::GotoDefinitionResponse::Array(locations) => {
                locations.into_iter().map(location_from_lsp).collect()
            }
            lsp_types::GotoDefinitionResponse::Link(links) => links
                .into_iter()
                .filter_map(|link| {
                    Some(Location {
                        path: uri_to_path(link.target_uri.as_str())?,
                        line: link.target_selection_range.start.line + 1,
                        column: link.target_selection_range.start.character + 1,
                    })
                })
                .collect(),
        })
    }

    /// Every reference to the symbol at `line`/`column` (both 1-based).
    pub async fn references(&self, path: &Path, line: u32, column: u32, timeout: Duration) -> Result<Vec<Location>> {
        let path = canonical(path);
        self.touch();
        self.ensure_open(&path).await?;
        let mut params = position_params(&path, line, column);
        params["context"] = json!({ "includeDeclaration": true });
        let value = self.request("textDocument/references", params, timeout).await?;
        if value.is_null() {
            return Ok(Vec::new());
        }
        let locations: Vec<lsp_types::Location> =
            serde_json::from_value(value).context("unexpected textDocument/references response")?;
        Ok(locations.into_iter().map(location_from_lsp).collect())
    }

    /// The hover text for the symbol at `line`/`column` (both 1-based).
    pub async fn hover(&self, path: &Path, line: u32, column: u32, timeout: Duration) -> Result<Option<String>> {
        let path = canonical(path);
        self.touch();
        self.ensure_open(&path).await?;
        let value = self
            .request("textDocument/hover", position_params(&path, line, column), timeout)
            .await?;
        if value.is_null() {
            return Ok(None);
        }
        let hover: lsp_types::Hover =
            serde_json::from_value(value).context("unexpected textDocument/hover response")?;
        let text = hover_text(hover.contents);
        Ok(if text.trim().is_empty() { None } else { Some(text) })
    }

    /// The outline of one file.
    pub async fn document_symbols(&self, path: &Path, timeout: Duration) -> Result<Vec<Symbol>> {
        let path = canonical(path);
        self.touch();
        self.ensure_open(&path).await?;
        let params = json!({ "textDocument": { "uri": path_to_uri(&path) } });
        let value = self.request("textDocument/documentSymbol", params, timeout).await?;
        if value.is_null() {
            return Ok(Vec::new());
        }
        let response: lsp_types::DocumentSymbolResponse =
            serde_json::from_value(value).context("unexpected textDocument/documentSymbol response")?;
        Ok(match response {
            lsp_types::DocumentSymbolResponse::Flat(items) => items.into_iter().map(symbol_information).collect(),
            lsp_types::DocumentSymbolResponse::Nested(symbols) => symbols
                .into_iter()
                .map(|symbol| document_symbol(symbol, &path))
                .collect(),
        })
    }

    /// Workspace-wide symbols matching `query`.
    pub async fn workspace_symbols(&self, query: &str, timeout: Duration) -> Result<Vec<Symbol>> {
        self.touch();
        let value = self
            .request("workspace/symbol", json!({ "query": query }), timeout)
            .await?;
        if value.is_null() {
            return Ok(Vec::new());
        }
        let response: lsp_types::WorkspaceSymbolResponse =
            serde_json::from_value(value).context("unexpected workspace/symbol response")?;
        Ok(match response {
            lsp_types::WorkspaceSymbolResponse::Flat(items) => items.into_iter().map(symbol_information).collect(),
            lsp_types::WorkspaceSymbolResponse::Nested(items) => items
                .into_iter()
                .map(|item| {
                    let (path, line, column) = match item.location {
                        lsp_types::OneOf::Left(location) => (
                            uri_to_path(location.uri.as_str()),
                            location.range.start.line + 1,
                            location.range.start.character + 1,
                        ),
                        lsp_types::OneOf::Right(location) => (uri_to_path(location.uri.as_str()), 1, 1),
                    };
                    Symbol {
                        name: item.name,
                        kind: symbol_kind_name(item.kind),
                        path,
                        line,
                        column,
                        children: Vec::new(),
                    }
                })
                .collect(),
        })
    }

    /// The workspace edit that renames the symbol at `line`/`column` (both
    /// 1-based) to `new_name`.
    ///
    /// `None` means the server declined to rename, which is how it reports a
    /// position that names no renameable symbol.
    pub async fn rename(
        &self,
        path: &Path,
        line: u32,
        column: u32,
        new_name: &str,
        timeout: Duration,
    ) -> Result<Option<WorkspaceEdit>> {
        let path = canonical(path);
        self.touch();
        self.ensure_open(&path).await?;
        let mut params = position_params(&path, line, column);
        params["newName"] = Value::String(new_name.to_string());
        let value = self.request("textDocument/rename", params, timeout).await?;
        if value.is_null() {
            return Ok(None);
        }
        Ok(Some(parse_workspace_edit(&value)?))
    }

    /// The code actions the server offers for a 0-based range.
    pub async fn code_actions(
        &self,
        path: &Path,
        start_line: u32,
        start_character: u32,
        end_line: u32,
        end_character: u32,
        timeout: Duration,
    ) -> Result<Vec<CodeAction>> {
        let path = canonical(path);
        self.touch();
        self.ensure_open(&path).await?;
        let params = json!({
            "textDocument": { "uri": path_to_uri(&path) },
            "range": {
                "start": { "line": start_line, "character": start_character },
                "end": { "line": end_line, "character": end_character },
            },
            "context": { "diagnostics": [] },
        });
        let value = self.request("textDocument/codeAction", params, timeout).await?;
        let Some(actions) = value.as_array() else {
            return Ok(Vec::new());
        };
        let mut parsed = Vec::with_capacity(actions.len());
        for action in actions {
            let Some(title) = action.get("title").and_then(Value::as_str) else {
                continue;
            };
            let edit = action
                .get("edit")
                .filter(|edit| edit.is_object())
                .map(parse_workspace_edit)
                .transpose()?;
            let disabled = action
                .get("disabled")
                .and_then(|disabled| disabled.get("reason"))
                .and_then(Value::as_str)
                .map(str::to_string);
            parsed.push(CodeAction {
                title: title.to_string(),
                kind: action.get("kind").and_then(Value::as_str).map(str::to_string),
                command_only: edit.is_none() && action.get("command").is_some_and(|command| command.is_object()),
                edit,
                disabled,
            });
        }
        Ok(parsed)
    }

    /// Ask the server to exit, then make sure the process and its children are
    /// gone. Safe to call twice.
    pub async fn shutdown(&self) {
        if self.is_alive() {
            let _ = self.request("shutdown", Value::Null, SHUTDOWN_GRACE).await;
            let _ = self.notify("exit", Value::Null);
        }
        self.stop_process();
    }

    fn stop_process(&self) {
        self.alive.store(false, Ordering::SeqCst);
        if let Some(mut child) = take_child(&self.child) {
            let _ = child.start_kill();
            // Without a wait the exited server becomes a zombie; the reader
            // task reaps it when it sees EOF, and this task covers shutdown
            // that happens before the EOF arrives.
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
        // Pid zero would name this process's own group, so it is refused
        // rather than signalled.
        if self.pid != 0 {
            horon_agent_platform::process::kill_process_group(self.pid);
        }
    }

    fn touch(&self) {
        self.last_used_millis.store(now_millis(), Ordering::Relaxed);
    }

    fn send(&self, message: &Value) -> Result<()> {
        let frame = crate::framing::encode(message.to_string().as_bytes());
        self.writer
            .send(frame)
            .map_err(|_| anyhow!("language server '{}' is not running", self.spec.name))
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        if !self.is_alive() {
            bail!("language server '{}' is not running", self.spec.name);
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().await.insert(id, sender);

        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        if let Err(error) = self.send(&message) {
            self.pending.lock().await.remove(&id);
            return Err(error);
        }

        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(message))) => Err(anyhow!("{method} failed: {message}")),
            Ok(Err(_)) => Err(anyhow!("language server '{}' stopped responding", self.spec.name)),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(anyhow!("{method} timed out after {}s", timeout.as_secs()))
            }
        }
    }

    async fn initialize(&self) -> Result<()> {
        let root_uri = path_to_uri(&self.root);
        let folder_name = self
            .root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string());

        let capabilities = json!({
            "general": {
                "positionEncodings": ["utf-16"],
            },
            "workspace": {
                "applyEdit": false,
                "configuration": true,
                "workspaceFolders": true,
                "symbol": {},
            },
            "textDocument": {
                "synchronization": {
                    "dynamicRegistration": false,
                    "didSave": false,
                    "willSave": false,
                    "willSaveWaitUntil": false,
                },
                "publishDiagnostics": {
                    "relatedInformation": false,
                    "versionSupport": false,
                    "codeDescriptionSupport": false,
                    "dataSupport": false,
                },
                "definition": { "linkSupport": true },
                "references": {},
                "hover": { "contentFormat": ["markdown", "plaintext"] },
                "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
            },
            "window": {
                "workDoneProgress": true,
            },
        });

        let mut params = json!({
            "processId": std::process::id(),
            "clientInfo": {
                "name": "horon-agent",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "rootUri": root_uri,
            "workspaceFolders": [{ "uri": root_uri, "name": folder_name }],
            "capabilities": capabilities,
        });
        // rust-analyzer otherwise starts `cargo check` on save, which spends
        // minutes of CPU on a workspace nobody asked to build.
        if self.spec.name == "rust-analyzer" {
            params["initializationOptions"] = json!({ "checkOnSave": false });
        }

        self.request("initialize", params, INITIALIZE_TIMEOUT)
            .await
            .context("initialize request failed")?;
        self.notify("initialized", json!({}))?;
        Ok(())
    }

    /// Send the current on-disk content, opening the document the first time.
    async fn ensure_open(&self, path: &Path) -> Result<()> {
        let text = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("could not read {}", path.display()))?;
        let uri = path_to_uri(path);

        let mut opened = self.opened.lock().await;
        match opened.get(path).copied() {
            None => {
                self.notify(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": {
                            "uri": uri,
                            "languageId": language_id(path),
                            "version": 1,
                            "text": text,
                        }
                    }),
                )?;
                opened.insert(path.to_path_buf(), 1);
            }
            Some(version) => {
                let version = version + 1;
                self.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": uri, "version": version },
                        "contentChanges": [{ "text": text }],
                    }),
                )?;
                opened.insert(path.to_path_buf(), version);
            }
        }
        Ok(())
    }

    async fn spawn(spec: &'static ServerSpec, root: PathBuf) -> Result<Self> {
        let mut command = Command::new(spec.program);
        command
            .args(spec.args)
            .current_dir(&root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        horon_agent_platform::process::isolate_process_group(command.as_std_mut());

        let mut child = command
            .spawn()
            .with_context(|| format!("could not start {} ({})", spec.name, spec.program))?;
        let pid = child.id().unwrap_or_default();
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("{}: stdin is unavailable", spec.name))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("{}: stdout is unavailable", spec.name))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("{}: stderr is unavailable", spec.name))?;

        let (writer_tx, mut writer_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let pending: Arc<PendingMap> = Arc::new(Mutex::new(HashMap::new()));
        let diagnostics: Arc<DiagnosticsMap> = Arc::new(Mutex::new(HashMap::new()));
        let diagnostics_notify = Arc::new(Notify::new());
        let alive = Arc::new(AtomicBool::new(true));
        let child_slot = Arc::new(StdMutex::new(Some(child)));

        {
            let name = spec.name;
            let alive = Arc::clone(&alive);
            tokio::spawn(async move {
                let mut stdin = stdin;
                while let Some(frame) = writer_rx.recv().await {
                    if stdin.write_all(&frame).await.is_err() || stdin.flush().await.is_err() {
                        horon_agent_logging::warn(&format!("lsp [{name}]: stdin closed; marking the server dead"));
                        alive.store(false, Ordering::SeqCst);
                        break;
                    }
                }
            });
        }

        {
            let name = spec.name;
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        horon_agent_logging::debug(&format!("lsp [{name}] stderr: {trimmed}"));
                    }
                }
            });
        }

        {
            let pending = Arc::clone(&pending);
            let diagnostics = Arc::clone(&diagnostics);
            let notify = Arc::clone(&diagnostics_notify);
            let alive = Arc::clone(&alive);
            let writer = writer_tx.clone();
            let child_slot = Arc::clone(&child_slot);
            let name = spec.name;
            let root = root.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stdout);
                loop {
                    match crate::framing::read_message(&mut reader).await {
                        Ok(Some(body)) => match serde_json::from_slice::<Value>(&body) {
                            Ok(message) => {
                                handle_message(message, &pending, &diagnostics, &notify, &writer, name, &root).await;
                            }
                            Err(error) => {
                                horon_agent_logging::warn(&format!("lsp [{name}]: unparsable message: {error}"));
                            }
                        },
                        Ok(None) => {
                            horon_agent_logging::debug(&format!("lsp [{name}]: stdout closed"));
                            break;
                        }
                        Err(error) => {
                            horon_agent_logging::warn(&format!("lsp [{name}]: read error: {error}"));
                            break;
                        }
                    }
                }
                alive.store(false, Ordering::SeqCst);
                pending.lock().await.clear();
                if let Some(mut child) = take_child(&child_slot) {
                    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
                }
            });
        }

        let client = Self {
            spec,
            root,
            pid,
            child: child_slot,
            writer: writer_tx,
            pending,
            diagnostics,
            diagnostics_notify,
            opened: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            alive,
            last_used_millis: AtomicU64::new(now_millis()),
        };
        client.initialize().await?;
        Ok(client)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop_process();
    }
}

/// Every live language server process.
pub async fn running_clients() -> Vec<ClientStatus> {
    let mut cache = cache().lock().await;
    cache.retain(|_, client| client.is_alive());
    let mut statuses: Vec<ClientStatus> = cache
        .values()
        .map(|client| ClientStatus {
            server: client.spec.name,
            root: client.root.clone(),
            pid: client.pid,
            idle_secs: client.idle_secs(),
        })
        .collect();
    statuses.sort_by(|a, b| (a.server, &a.root).cmp(&(b.server, &b.root)));
    statuses
}

type CacheKey = (String, PathBuf);

static CACHE: LazyLock<Mutex<HashMap<CacheKey, Arc<Client>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn cache() -> &'static Mutex<HashMap<CacheKey, Arc<Client>>> {
    &CACHE
}

fn spawn_idle_monitor(client: &Arc<Client>) {
    let weak: Weak<Client> = Arc::downgrade(client);
    let key: CacheKey = (client.spec.name.to_string(), client.root.clone());
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(IDLE_CHECK_INTERVAL).await;
            let Some(client) = weak.upgrade() else {
                return;
            };
            if !client.is_alive() {
                return;
            }
            if client.idle_secs() < IDLE_TIMEOUT.as_secs() {
                continue;
            }
            horon_agent_logging::debug(&format!(
                "lsp [{}]: shutting down after {}s idle",
                client.spec.name,
                client.idle_secs()
            ));
            client.shutdown().await;
            cache().lock().await.remove(&key);
            return;
        }
    });
}

/// Handle one message from the server: a response, a notification, or a
/// server-to-client request that must be answered so the server keeps going.
async fn handle_message(
    message: Value,
    pending: &PendingMap,
    diagnostics: &DiagnosticsMap,
    notify: &Notify,
    writer: &mpsc::UnboundedSender<Vec<u8>>,
    server_name: &str,
    root: &Path,
) {
    if let Some(method) = message.get("method").and_then(Value::as_str) {
        if let Some(id) = message.get("id") {
            let result = server_request_result(method, message.get("params"), server_name, root);
            let response = json!({ "jsonrpc": "2.0", "id": id, "result": result });
            let _ = writer.send(crate::framing::encode(response.to_string().as_bytes()));
            return;
        }

        if method == "textDocument/publishDiagnostics" {
            let Some(params) = message.get("params") else {
                return;
            };
            match serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(params.clone()) {
                Ok(params) => {
                    let uri = params.uri.as_str().to_string();
                    let path = uri_to_path(&uri).unwrap_or_default();
                    let items: Vec<Diagnostic> = params
                        .diagnostics
                        .into_iter()
                        .map(|diagnostic| Diagnostic {
                            path: path.clone(),
                            line: diagnostic.range.start.line + 1,
                            column: diagnostic.range.start.character + 1,
                            severity: severity_of(diagnostic.severity),
                            message: diagnostic.message,
                        })
                        .collect();
                    let mut map = diagnostics.lock().await;
                    let generation = map.get(&uri).map(|state| state.generation).unwrap_or(0) + 1;
                    map.insert(uri, DiagnosticsState { generation, items });
                    drop(map);
                    notify.notify_waiters();
                }
                Err(error) => {
                    horon_agent_logging::warn(&format!("lsp [{server_name}]: unparsable publishDiagnostics: {error}"));
                }
            }
        }
        return;
    }

    let Some(id) = message.get("id").and_then(Value::as_u64) else {
        return;
    };
    let Some(sender) = pending.lock().await.remove(&id) else {
        return;
    };
    let outcome = match message.get("error") {
        Some(error) => Err(error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| error.to_string())),
        None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
    };
    let _ = sender.send(outcome);
}

/// The result for a request the server sent us.
fn server_request_result(method: &str, params: Option<&Value>, server_name: &str, root: &Path) -> Value {
    match method {
        // A language server asks for its settings through us. The defaults are
        // what we want, so a `rust-analyzer` section only pins the check mode
        // we already requested at initialize time.
        "workspace/configuration" => {
            let count = params
                .and_then(|params| params.get("items"))
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            Value::Array(
                (0..count)
                    .map(|index| {
                        let section = params
                            .and_then(|params| params.get("items"))
                            .and_then(|items| items.get(index))
                            .and_then(|item| item.get("section"))
                            .and_then(Value::as_str)
                            .unwrap_or("");
                        if server_name == "rust-analyzer" && section.starts_with("rust-analyzer") {
                            json!({ "checkOnSave": false })
                        } else {
                            json!({})
                        }
                    })
                    .collect(),
            )
        }
        "workspace/workspaceFolders" => {
            let name = root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.display().to_string());
            json!([{ "uri": path_to_uri(root), "name": name }])
        }
        "workspace/applyEdit" => json!({ "applied": false }),
        "window/showMessageRequest" => Value::Null,
        _ => Value::Null,
    }
}

fn position_params(path: &Path, line: u32, column: u32) -> Value {
    json!({
        "textDocument": { "uri": path_to_uri(path) },
        "position": {
            "line": line.saturating_sub(1),
            "character": column.saturating_sub(1),
        },
    })
}

fn location_from_lsp(location: lsp_types::Location) -> Location {
    Location {
        path: uri_to_path(location.uri.as_str()).unwrap_or_default(),
        line: location.range.start.line + 1,
        column: location.range.start.character + 1,
    }
}

fn symbol_information(info: lsp_types::SymbolInformation) -> Symbol {
    Symbol {
        name: info.name,
        kind: symbol_kind_name(info.kind),
        path: uri_to_path(info.location.uri.as_str()),
        line: info.location.range.start.line + 1,
        column: info.location.range.start.character + 1,
        children: Vec::new(),
    }
}

fn document_symbol(symbol: lsp_types::DocumentSymbol, path: &Path) -> Symbol {
    Symbol {
        name: symbol.name,
        kind: symbol_kind_name(symbol.kind),
        path: Some(path.to_path_buf()),
        line: symbol.selection_range.start.line + 1,
        column: symbol.selection_range.start.character + 1,
        children: symbol
            .children
            .unwrap_or_default()
            .into_iter()
            .map(|child| document_symbol(child, path))
            .collect(),
    }
}

fn severity_of(severity: Option<lsp_types::DiagnosticSeverity>) -> DiagnosticSeverity {
    use lsp_types::DiagnosticSeverity as LspSeverity;
    match severity {
        Some(value) if value == LspSeverity::WARNING => DiagnosticSeverity::Warning,
        Some(value) if value == LspSeverity::INFORMATION => DiagnosticSeverity::Information,
        Some(value) if value == LspSeverity::HINT => DiagnosticSeverity::Hint,
        _ => DiagnosticSeverity::Error,
    }
}

fn symbol_kind_name(kind: lsp_types::SymbolKind) -> &'static str {
    // `SymbolKind` is a newtype whose `Debug` prints the variant in PascalCase.
    match format!("{kind:?}").as_str() {
        "File" => "file",
        "Module" => "module",
        "Namespace" => "namespace",
        "Package" => "package",
        "Class" => "class",
        "Method" => "method",
        "Property" => "property",
        "Field" => "field",
        "Constructor" => "constructor",
        "Enum" => "enum",
        "Interface" => "interface",
        "Function" => "function",
        "Variable" => "variable",
        "Constant" => "constant",
        "String" => "string",
        "Number" => "number",
        "Boolean" => "boolean",
        "Array" => "array",
        "Object" => "object",
        "Key" => "key",
        "Null" => "null",
        "EnumMember" => "enum member",
        "Struct" => "struct",
        "Event" => "event",
        "Operator" => "operator",
        "TypeParameter" => "type parameter",
        _ => "symbol",
    }
}

fn hover_text(contents: lsp_types::HoverContents) -> String {
    fn marked(value: lsp_types::MarkedString) -> String {
        match value {
            lsp_types::MarkedString::String(text) => text,
            lsp_types::MarkedString::LanguageString(lsp_types::LanguageString { language, value }) => {
                format!("```{language}\n{value}\n```")
            }
        }
    }

    match contents {
        lsp_types::HoverContents::Scalar(value) => marked(value),
        lsp_types::HoverContents::Array(values) => values.into_iter().map(marked).collect::<Vec<_>>().join("\n\n"),
        lsp_types::HoverContents::Markup(markup) => markup.value,
    }
}

fn language_id(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("rs") => "rust",
        Some("ts") | Some("mts") | Some("cts") => "typescript",
        Some("tsx") => "typescriptreact",
        Some("js") | Some("mjs") | Some("cjs") => "javascript",
        Some("jsx") => "javascriptreact",
        Some("go") => "go",
        Some("py") => "python",
        _ => "plaintext",
    }
}

/// Read a `WorkspaceEdit` from a server response.
///
/// `changes` (a URI map) and `documentChanges` (an ordered list) may both be
/// present; edits for the same file are merged in the order they were listed.
/// Resource operations inside `documentChanges` name no text edit and are
/// skipped, because the tool only applies text edits.
fn parse_workspace_edit(value: &Value) -> Result<WorkspaceEdit> {
    let mut edit = WorkspaceEdit::default();
    if let Some(changes) = value.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes {
            let Some(path) = uri_to_path(uri) else {
                continue;
            };
            push_file_edits(&mut edit, path, edits)?;
        }
    }
    if let Some(changes) = value.get("documentChanges").and_then(Value::as_array) {
        for change in changes {
            let Some(uri) = change
                .get("textDocument")
                .and_then(|document| document.get("uri"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            let Some(path) = uri_to_path(uri) else {
                continue;
            };
            if let Some(edits) = change.get("edits") {
                push_file_edits(&mut edit, path, edits)?;
            }
        }
    }
    Ok(edit)
}

fn push_file_edits(edit: &mut WorkspaceEdit, path: PathBuf, edits: &Value) -> Result<()> {
    let Some(edits) = edits.as_array() else {
        return Ok(());
    };
    let mut text_edits = Vec::with_capacity(edits.len());
    for entry in edits {
        // An `AnnotatedTextEdit` wraps the real edit under `textEdit`.
        let entry = entry.get("textEdit").unwrap_or(entry);
        let Some(range) = entry.get("range") else {
            continue;
        };
        let (start_line, start_character) = parse_position(range.get("start"));
        let (end_line, end_character) = parse_position(range.get("end"));
        text_edits.push(TextEdit {
            start_line,
            start_character,
            end_line,
            end_character,
            new_text: match entry.get("newText").and_then(Value::as_str) {
                Some(text) => text.to_string(),
                None => String::new(),
            },
        });
    }
    if let Some(existing) = edit.files.iter_mut().find(|file| file.path == path) {
        existing.edits.extend(text_edits);
    } else {
        edit.files.push(FileEdit {
            path,
            edits: text_edits,
        });
    }
    Ok(())
}

/// A 0-based LSP position as `(line, character)`. A missing or oversized value
/// reads as zero, which no real edit uses.
fn parse_position(value: Option<&Value>) -> (u32, u32) {
    fn unit(value: Option<&Value>) -> u32 {
        value
            .and_then(Value::as_u64)
            .map_or(0, |value| u32::try_from(value).unwrap_or(u32::MAX))
    }
    (
        unit(value.and_then(|position| position.get("line"))),
        unit(value.and_then(|position| position.get("character"))),
    )
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Remove and return the child process, if it has not already been taken.
fn take_child(slot: &StdMutex<Option<Child>>) -> Option<Child> {
    let mut slot = slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    slot.take()
}

fn path_to_uri(path: &Path) -> String {
    match Url::from_file_path(path) {
        Ok(url) => url.to_string(),
        Err(()) => format!("file://{}", path.to_string_lossy().replace('\\', "/")),
    }
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    Url::parse(uri).ok().and_then(|url| url.to_file_path().ok())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
