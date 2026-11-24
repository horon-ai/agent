//! The serving half of the SDK: one frame per line on stdin and stdout, the
//! `initialize` exchange, dispatch, cancellation and shutdown.
//!
//! One reader thread parses inbound frames and owns the two notifications the
//! server answers through state rather than the main loop: `cancel` flips the
//! token of the request it names, and everything else is handed to the main
//! thread in order. Only the main thread writes, so stdout carries protocol
//! frames and nothing else.

use std::collections::HashMap;
use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;

use horon_agent_extension_types::ObserverEvent;
use horon_agent_extension_types::wire::{
    CancelParams, Capability, CapabilitySet, CommandInvokeParams, FrameError, HandshakeError, InitializeParams,
    JsonRpcVersion, MAX_FRAME_BYTES, MAX_QUEUED_FRAMES, ObserverParams, RpcError, ToolCallParams, WireFrame,
    WireMethod, WireNotification, WireRequest, WireResponse, encode_frame_into, parse_frame,
};
use serde::Serialize;
use serde_json::Value;

use crate::declaration::Declaration;

/// JSON-RPC "invalid request" (`-32600`): the frame has the wrong shape here.
pub const INVALID_REQUEST: i32 = -32600;

/// JSON-RPC "invalid params" (`-32602`): a payload is not the declared shape.
pub const INVALID_PARAMS: i32 = -32602;

/// The host cancelled the request (`-32800`). A cancelled call answers nothing;
/// a handler that stops early reports this code for its own bookkeeping.
pub const CANCELLED: i32 = -32800;

/// A request arrived before `initialize` settled (`-32002`).
pub const NOT_INITIALIZED: i32 = -32002;

/// The host offered a protocol range this package does not speak (`-32001`).
pub const PROTOCOL_UNAVAILABLE: i32 = -32001;

/// One package's behaviour, called on the thread that serves the wire.
///
/// The three name lists must name exactly what the declaration declares;
/// [`Declaration::check`] refuses to serve otherwise.
pub trait Extension {
    /// Tool names this binary implements.
    fn tools(&self) -> &'static [&'static str];

    /// Executable command names this binary implements.
    fn commands(&self) -> &'static [&'static str];

    /// Events this binary observes.
    fn events(&self) -> &'static [ObserverEvent];

    /// Answer one `tool/call` for a declared tool name.
    fn call_tool(&self, name: &str, arguments: &Value, context: &mut Context<'_>) -> Result<Value, RpcError>;

    /// Answer one `command/invoke` for a declared executable command.
    fn invoke_command(&self, name: &str, arguments: &Value, context: &mut Context<'_>) -> Result<Value, RpcError>;

    /// React to one event the host observed. Nothing is answered.
    fn observe(&self, _event: ObserverEvent, _metadata: &Value, _context: &mut Context<'_>) {}
}

/// What a handler may do while it answers one frame.
pub struct Context<'a> {
    cancel: &'a AtomicBool,
    observations: &'a mut Vec<ObserverParams>,
}

impl Context<'_> {
    /// Whether the host cancelled this request. A long call should poll it and
    /// stop; the server answers nothing for a cancelled call.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Report one observation to the host. Bounded: past
    /// [`MAX_QUEUED_FRAMES`] reports in one call the extra ones are dropped, so a
    /// handler cannot grow the process without bound.
    pub fn notify(&mut self, event: ObserverEvent, metadata: Value) {
        if self.observations.len() < MAX_QUEUED_FRAMES {
            self.observations.push(ObserverParams { event, metadata });
        }
    }
}

impl Declaration {
    /// Refuse to serve when the code and the declaration name different
    /// surfaces: a declared tool with no handler could never be answered, and a
    /// handler whose declaration is missing would be unreachable.
    pub fn check<E: Extension>(&self, extension: &E) -> Result<(), SurfaceError> {
        let manifest = self.manifest();
        for tool in &manifest.tools {
            if !extension.tools().iter().any(|name| tool.name == *name) {
                return Err(SurfaceError::MissingTool(tool.name.clone()));
            }
        }
        for name in extension.tools() {
            if !manifest.tools.iter().any(|tool| tool.name == *name) {
                return Err(SurfaceError::UndeclaredTool((*name).to_owned()));
            }
        }
        for command in &manifest.commands {
            if !extension.commands().iter().any(|name| command.name == *name) {
                return Err(SurfaceError::MissingCommand(command.name.clone()));
            }
        }
        for name in extension.commands() {
            if !manifest.commands.iter().any(|command| command.name == *name) {
                return Err(SurfaceError::UndeclaredCommand((*name).to_owned()));
            }
        }
        for event in &manifest.events {
            if !extension.events().contains(event) {
                return Err(SurfaceError::MissingEvent(*event));
            }
        }
        for event in extension.events() {
            if !manifest.events.contains(event) {
                return Err(SurfaceError::UndeclaredEvent(*event));
            }
        }
        Ok(())
    }
}

/// Why a binary cannot serve the declaration it was installed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceError {
    MissingTool(String),
    UndeclaredTool(String),
    MissingCommand(String),
    UndeclaredCommand(String),
    MissingEvent(ObserverEvent),
    UndeclaredEvent(ObserverEvent),
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingTool(name) => write!(formatter, "the declaration names tool {name}, which this binary lacks"),
            Self::UndeclaredTool(name) => {
                write!(
                    formatter,
                    "this binary implements tool {name}, which the declaration never names"
                )
            }
            Self::MissingCommand(name) => {
                write!(
                    formatter,
                    "the declaration names command {name}, which this binary lacks"
                )
            }
            Self::UndeclaredCommand(name) => {
                write!(
                    formatter,
                    "this binary implements command {name}, which the declaration never names"
                )
            }
            Self::MissingEvent(event) => {
                write!(
                    formatter,
                    "the declaration names event {event:?}, which this binary does not observe"
                )
            }
            Self::UndeclaredEvent(event) => {
                write!(
                    formatter,
                    "this binary observes event {event:?}, which the declaration never names"
                )
            }
        }
    }
}

impl std::error::Error for SurfaceError {}

/// Why serving ended before the host asked for shutdown.
#[derive(Debug)]
pub enum ServeError {
    Io(io::Error),
    Frame(FrameError),
    Handshake(HandshakeError),
    Surface(SurfaceError),
    /// A lock was poisoned by a panic in another thread; the connection cannot
    /// be trusted afterwards.
    Poisoned,
}

impl fmt::Display for ServeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Frame(error) => write!(formatter, "{error}"),
            Self::Handshake(error) => write!(formatter, "initialize refused: {error}"),
            Self::Surface(error) => write!(formatter, "{error}"),
            Self::Poisoned => formatter.write_str("the connection state was poisoned"),
        }
    }
}

impl std::error::Error for ServeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Frame(error) => Some(error),
            Self::Handshake(error) => Some(error),
            Self::Surface(error) => Some(error),
            Self::Poisoned => None,
        }
    }
}

impl From<io::Error> for ServeError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<FrameError> for ServeError {
    fn from(error: FrameError) -> Self {
        Self::Frame(error)
    }
}

impl From<SurfaceError> for ServeError {
    fn from(error: SurfaceError) -> Self {
        Self::Surface(error)
    }
}

/// Serve the installed package on this process's stdin and stdout.
///
/// stdout carries protocol frames and nothing else; diagnostics belong on
/// stderr. The call returns after `shutdown` or when the host closes stdin; a
/// call still running at that point is cancelled, and no later call starts.
pub fn serve_stdio<E: Extension>(extension: E, declaration: Declaration) -> Result<(), ServeError> {
    serve(extension, declaration, io::stdin(), io::stdout())
}

/// Serve the installed package over one input and one output.
///
/// `input` is read on its own thread so a `cancel` notification is seen while a
/// handler runs; `output` is written only by the calling thread.
pub fn serve<E, R, W>(extension: E, declaration: Declaration, input: R, output: W) -> Result<(), ServeError>
where
    E: Extension,
    R: Read + Send + 'static,
    W: Write,
{
    declaration.check(&extension)?;
    let writer = Writer::new(output);
    let tokens: Arc<Mutex<HashMap<u64, Cancel>>> = Arc::new(Mutex::new(HashMap::new()));
    // The id of the call the main thread is inside, or zero, and whether the
    // host's stream has ended. The reader uses both when the stream ends: it
    // cancels that one call and no later call starts, because a handler waiting
    // for cancellation must not outlive the connection it serves.
    let running = Arc::new(AtomicU64::new(0));
    let ended = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::sync_channel::<Inbound>(MAX_QUEUED_FRAMES);
    let reader_tokens = Arc::clone(&tokens);
    let reader_running = Arc::clone(&running);
    let reader_ended = Arc::clone(&ended);
    let reader = thread::Builder::new()
        .name("horon-extension-input".to_owned())
        .spawn(move || {
            read(
                &mut Source::new(input),
                &reader_tokens,
                &reader_running,
                &reader_ended,
                &sender,
            )
        })
        .map_err(ServeError::Io)?;
    // The reader blocks on the host's stream until it ends; the process that
    // owns this connection exits after `serve` returns, so the handle is not
    // joined. A caller that keeps the stream open until `shutdown` never sees it
    // end, and one that closes it ends the thread with the test.
    drop(reader);

    let mut server = Server {
        extension: &extension,
        declaration: &declaration,
        writer,
        tokens,
        running,
        ended,
        granted: CapabilitySet::default(),
        initialized: false,
        observations: Vec::new(),
    };
    loop {
        let (frame, cancel) = match receiver.recv() {
            Ok(Inbound::Frame(frame, cancel)) => (frame, cancel),
            Ok(Inbound::End) => return Ok(()),
            Ok(Inbound::Failed(error)) => return Err(error),
            Err(_) => return Ok(()),
        };
        match frame {
            WireFrame::Request(request) => {
                if server.request(request, cancel)? {
                    return Ok(());
                }
            }
            WireFrame::Notification(notification) => server.notification(notification)?,
            // The child never sends a request, so no answer can belong to it.
            WireFrame::Response(_) => {}
        }
    }
}

struct Server<'a, E, W> {
    extension: &'a E,
    declaration: &'a Declaration,
    writer: Writer<W>,
    tokens: Arc<Mutex<HashMap<u64, Cancel>>>,
    running: Arc<AtomicU64>,
    ended: Arc<AtomicBool>,
    granted: CapabilitySet,
    initialized: bool,
    observations: Vec<ObserverParams>,
}

impl<E: Extension, W: Write> Server<'_, E, W> {
    /// Route one request. `Ok(true)` means `shutdown` settled and serving is over.
    fn request(&mut self, request: WireRequest, cancel: Option<Cancel>) -> Result<bool, ServeError> {
        match request.method {
            WireMethod::Initialize => self.initialize(request),
            WireMethod::ToolCall => self.call(request, Capability::ToolCall, cancel),
            WireMethod::CommandInvoke => self.call(request, Capability::CommandInvoke, cancel),
            WireMethod::Shutdown => {
                self.reply_ref(request.id, &Value::Null)?;
                Ok(true)
            }
            WireMethod::Cancel | WireMethod::ObserverNotify => {
                self.fail(
                    request.id,
                    RpcError::method_not_found("this method is a notification and carries no request id"),
                )?;
                Ok(false)
            }
        }
    }

    fn initialize(&mut self, request: WireRequest) -> Result<bool, ServeError> {
        let id = request.id;
        if self.initialized {
            self.fail(id, invalid_request("initialize was already answered"))?;
            return Ok(false);
        }
        let params: InitializeParams = match request.into_params() {
            Ok(params) => params,
            Err(_) => {
                self.fail(id, invalid_params())?;
                return Ok(false);
            }
        };
        match self.declaration.accept(&params) {
            Ok(result) => {
                self.reply_ref(id, &result)?;
                self.granted = result.capabilities;
                self.initialized = true;
                Ok(false)
            }
            Err(error) => {
                self.fail(id, refusal(&error))?;
                Err(ServeError::Handshake(error))
            }
        }
    }

    fn call(
        &mut self,
        request: WireRequest,
        capability: Capability,
        cancel: Option<Cancel>,
    ) -> Result<bool, ServeError> {
        let id = request.id;
        let cancel = cancel.unwrap_or_else(Cancel::new);
        let outcome = self.dispatch(request, capability, &cancel);
        self.forget_cancel(id);
        outcome
    }

    fn dispatch(&mut self, request: WireRequest, capability: Capability, cancel: &Cancel) -> Result<bool, ServeError> {
        let id = request.id;
        if !self.initialized {
            self.fail(id, not_initialized())?;
            return Ok(false);
        }
        if !self.granted.contains(&capability) {
            self.fail(id, RpcError::method_not_found(method_name(capability)))?;
            return Ok(false);
        }
        let call = match capability {
            Capability::ToolCall => match request.into_params::<ToolCallParams>() {
                Ok(params) => Call::Tool(params),
                Err(_) => {
                    self.fail(id, invalid_params())?;
                    return Ok(false);
                }
            },
            Capability::CommandInvoke => match request.into_params::<CommandInvokeParams>() {
                Ok(params) => Call::Command(params),
                Err(_) => {
                    self.fail(id, invalid_params())?;
                    return Ok(false);
                }
            },
            Capability::ObserverNotify => {
                self.fail(id, RpcError::method_not_found(method_name(capability)))?;
                return Ok(false);
            }
        };
        if let Some(name) = call.name() {
            let declared = match &call {
                Call::Tool(_) => self.declaration.manifest().tools.iter().any(|tool| tool.name == name),
                Call::Command(_) => self
                    .declaration
                    .manifest()
                    .commands
                    .iter()
                    .any(|command| command.name == name),
            };
            if !declared {
                self.fail(id, RpcError::method_not_found(name))?;
                return Ok(false);
            }
        }
        // Publish the call before checking whether the stream ended. The reader
        // writes `ended` and then reads `running`, so writing first and reading
        // second on both sides means at least one of them observes the other.
        self.running.store(id, Ordering::SeqCst);
        // A request the host cancelled before it started must never run, and
        // neither may one that arrives after the host's stream ended.
        if cancel.is_cancelled() || self.ended.load(Ordering::SeqCst) {
            self.running.store(0, Ordering::SeqCst);
            return Ok(false);
        }
        self.observations.clear();
        let outcome = {
            let mut context = Context {
                cancel: &cancel.flag,
                observations: &mut self.observations,
            };
            match &call {
                Call::Tool(params) => self.extension.call_tool(&params.tool, &params.arguments, &mut context),
                Call::Command(params) => {
                    self.extension
                        .invoke_command(&params.command, &params.arguments, &mut context)
                }
            }
        };
        self.running.store(0, Ordering::SeqCst);
        // Cancellation wins: the host already stopped waiting, so neither an
        // observation nor an answer may follow. The check runs before the
        // reports and again before the answer, because a cancel can arrive while
        // the reports are written.
        if cancel.is_cancelled() {
            return Ok(false);
        }
        self.flush_observations()?;
        if cancel.is_cancelled() {
            return Ok(false);
        }
        match outcome {
            Ok(value) => self.reply_ref(id, &value)?,
            Err(error) => self.fail(id, error)?,
        }
        Ok(false)
    }

    fn notification(&mut self, notification: WireNotification) -> Result<(), ServeError> {
        static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);
        if notification.method != WireMethod::ObserverNotify || !self.initialized {
            return Ok(());
        }
        if !self.granted.contains(&Capability::ObserverNotify) {
            return Ok(());
        }
        let params: ObserverParams = match notification.into_params() {
            Ok(params) => params,
            Err(_) => return Ok(()),
        };
        self.observations.clear();
        {
            let mut context = Context {
                cancel: &NEVER_CANCELLED,
                observations: &mut self.observations,
            };
            self.extension.observe(params.event, &params.metadata, &mut context);
        }
        self.flush_observations()
    }

    fn forget_cancel(&self, id: u64) {
        let mut tokens = match self.tokens.lock() {
            Ok(tokens) => tokens,
            Err(poisoned) => poisoned.into_inner(),
        };
        tokens.remove(&id);
    }

    fn flush_observations(&mut self) -> Result<(), ServeError> {
        if !self.granted.contains(&Capability::ObserverNotify) {
            return Ok(());
        }
        for observation in &self.observations {
            self.writer.send(&WireNotificationRef {
                jsonrpc: JsonRpcVersion::V2,
                method: WireMethod::ObserverNotify,
                params: observation,
            })?;
        }
        Ok(())
    }

    fn reply_ref(&mut self, id: u64, result: &impl Serialize) -> Result<(), ServeError> {
        self.writer.send(&WireReplyRef {
            jsonrpc: JsonRpcVersion::V2,
            id,
            result,
        })
    }

    fn fail(&mut self, id: u64, error: RpcError) -> Result<(), ServeError> {
        self.writer.send(&WireResponse::fail(id, error))
    }
}

#[derive(Serialize)]
struct WireReplyRef<'a, R: Serialize + ?Sized> {
    jsonrpc: JsonRpcVersion,
    id: u64,
    result: &'a R,
}

#[derive(Serialize)]
struct WireNotificationRef<'a, P: Serialize + ?Sized> {
    jsonrpc: JsonRpcVersion,
    method: WireMethod,
    params: &'a P,
}

enum Call {
    Tool(ToolCallParams),
    Command(CommandInvokeParams),
}

impl Call {
    fn name(&self) -> Option<&str> {
        match self {
            Self::Tool(params) => Some(&params.tool),
            Self::Command(params) => Some(&params.command),
        }
    }
}

/// The host's cancellation of one in-flight request.
#[derive(Clone, Debug)]
struct Cancel {
    flag: Arc<AtomicBool>,
}

impl Cancel {
    fn new() -> Self {
        Self {
            flag: Arc::new(AtomicBool::new(false)),
        }
    }

    fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }

    fn cancel(&self) {
        self.flag.store(true, Ordering::SeqCst);
    }
}

struct Writer<W> {
    output: W,
    buf: Vec<u8>,
}

impl<W: Write> Writer<W> {
    fn new(output: W) -> Self {
        Self {
            output,
            buf: Vec::with_capacity(256),
        }
    }

    /// One whole frame, one write, flushed: a reader never sees half a frame and
    /// two frames never interleave.
    fn send(&mut self, frame: &impl Serialize) -> Result<(), ServeError> {
        encode_frame_into(frame, &mut self.buf)?;
        self.output.write_all(&self.buf)?;
        self.output.flush()?;
        Ok(())
    }
}

enum Inbound {
    Frame(WireFrame, Option<Cancel>),
    Failed(ServeError),
    End,
}

/// Bounded newline frames: the buffer never grows past [`MAX_FRAME_BYTES`], and
/// a frame without its newline is refused instead of being parsed.
struct Source<R> {
    reader: BufReader<R>,
    frame: Vec<u8>,
}

impl<R: Read> Source<R> {
    fn new(reader: R) -> Self {
        Self {
            reader: BufReader::new(reader),
            frame: Vec::new(),
        }
    }

    fn next(&mut self) -> Result<Option<WireFrame>, ServeError> {
        self.frame.clear();
        loop {
            let available = self.reader.fill_buf().map_err(ServeError::Io)?;
            if available.is_empty() {
                return if self.frame.is_empty() {
                    Ok(None)
                } else {
                    Err(ServeError::Frame(FrameError::Unterminated))
                };
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(available.len(), |position| position + 1);
            if self.frame.len() + take > MAX_FRAME_BYTES {
                return Err(ServeError::Frame(FrameError::TooLarge { limit: MAX_FRAME_BYTES }));
            }
            self.frame.extend_from_slice(&available[..take]);
            self.reader.consume(take);
            if newline.is_some() {
                return parse_frame(&self.frame).map(Some).map_err(ServeError::Frame);
            }
        }
    }
}

fn read<R: Read>(
    source: &mut Source<R>,
    tokens: &Mutex<HashMap<u64, Cancel>>,
    running: &AtomicU64,
    ended: &AtomicBool,
    sender: &SyncSender<Inbound>,
) {
    loop {
        let frame = match source.next() {
            Ok(Some(frame)) => frame,
            Ok(None) => {
                ended.store(true, Ordering::SeqCst);
                cancel_running(tokens, running);
                let _sent = sender.send(Inbound::End);
                return;
            }
            Err(error) => {
                ended.store(true, Ordering::SeqCst);
                cancel_running(tokens, running);
                let _sent = sender.send(Inbound::Failed(error));
                return;
            }
        };
        match frame {
            WireFrame::Request(request) => {
                let cancel = register(tokens, &request);
                if sender
                    .send(Inbound::Frame(WireFrame::Request(request), cancel))
                    .is_err()
                {
                    return;
                }
            }
            WireFrame::Notification(notification) if notification.method == WireMethod::Cancel => {
                // Settled here, not on the main thread: a full queue must never
                // hide a cancellation behind a call the handler is still running.
                cancel(tokens, notification);
            }
            WireFrame::Notification(notification) => {
                // An event the host reported is best effort. A full queue drops
                // it instead of blocking the reader that also delivers cancels.
                let _queued = sender.try_send(Inbound::Frame(WireFrame::Notification(notification), None));
            }
            response => {
                if sender.send(Inbound::Frame(response, None)).is_err() {
                    return;
                }
            }
        }
    }
}

/// The stream ended or failed: a handler still inside a call must stop instead
/// of waiting for a cancellation the closed connection can no longer deliver.
fn cancel_running(tokens: &Mutex<HashMap<u64, Cancel>>, running: &AtomicU64) {
    let id = running.load(Ordering::SeqCst);
    if id == 0 {
        return;
    }
    let tokens = match tokens.lock() {
        Ok(tokens) => tokens,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(cancel) = tokens.get(&id) {
        cancel.cancel();
    }
}

/// A call the server may have to cancel gets its token before the main thread
/// can see the frame, so a `cancel` that arrives first is never lost.
fn register(tokens: &Mutex<HashMap<u64, Cancel>>, request: &WireRequest) -> Option<Cancel> {
    if !matches!(request.method, WireMethod::ToolCall | WireMethod::CommandInvoke) {
        return None;
    }
    let cancel = Cancel::new();
    let mut tokens = match tokens.lock() {
        Ok(tokens) => tokens,
        Err(poisoned) => poisoned.into_inner(),
    };
    tokens.insert(request.id, cancel.clone());
    Some(cancel)
}

fn cancel(tokens: &Mutex<HashMap<u64, Cancel>>, notification: WireNotification) {
    if notification.method != WireMethod::Cancel {
        return;
    }
    let params: CancelParams = match notification.into_params() {
        Ok(params) => params,
        Err(_) => return,
    };
    let tokens = match tokens.lock() {
        Ok(tokens) => tokens,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(cancel) = tokens.get(&params.request_id) {
        cancel.cancel();
    }
}

fn invalid_request(message: &str) -> RpcError {
    RpcError {
        code: INVALID_REQUEST,
        message: message.to_owned(),
        data: None,
    }
}

fn invalid_params() -> RpcError {
    RpcError {
        code: INVALID_PARAMS,
        message: "the request parameters are not the declared shape".to_owned(),
        data: None,
    }
}

fn not_initialized() -> RpcError {
    RpcError {
        code: NOT_INITIALIZED,
        message: "initialize must settle before any request".to_owned(),
        data: None,
    }
}

fn refusal(error: &HandshakeError) -> RpcError {
    let code = match error {
        HandshakeError::ProtocolIncompatible { .. } => PROTOCOL_UNAVAILABLE,
        // The offer named another package's identity or asked for a capability
        // this declaration never granted; that is a parameter problem.
        HandshakeError::Kind { .. }
        | HandshakeError::PackageId { .. }
        | HandshakeError::CapabilityNotDeclared(_)
        | HandshakeError::InvalidDigest
        | HandshakeError::DigestMismatch => INVALID_PARAMS,
    };
    RpcError {
        code,
        message: format!("initialize refused: {error}"),
        data: None,
    }
}

fn method_name(capability: Capability) -> &'static str {
    match capability {
        Capability::ToolCall => "tool/call",
        Capability::CommandInvoke => "command/invoke",
        Capability::ObserverNotify => "observer/notify",
    }
}
