//! Text file I/O through the ACP client that serves the session.
//!
//! When the editor advertises `fs.readTextFile` / `fs.writeTextFile`, the text
//! tools read and write the editor's buffer instead of the daemon's disk. While
//! the user has unsaved changes the buffer is the source of truth: a read must
//! return what the editor shows, and an edit must be applied to that text, or
//! the write silently reverts work the user can see on screen.
//!
//! Three properties this module keeps:
//!
//! - Capability gating is per direction and per call. No bridge, no advertised
//!   method, or a turn another client drove means local disk exactly as before;
//!   [`super::client_bridge::for_session`] decides all three.
//! - A client request that fails is an error the tool reports. It never falls
//!   back to a disk write: writing the daemon's copy behind the user's buffer is
//!   the one outcome that loses their unsaved text without telling them.
//! - Binary, image and PDF paths stay local. `fs/read_text_file` carries text,
//!   and those need bytes, vision encoding, or PDF extraction.
//!
//! Deletion and moves have no ACP counterpart, so `apply_patch`/`patch` still
//! remove or move the disk file; only text reads and writes are delegated.

// The tests hold the process-wide test-env lock across awaits, the same
// allowance `tool/read.rs` takes for the same pattern: the file tools read the
// active config, and a test that rewrites `HORON_HOME` concurrently would
// otherwise hand these reads another test's configuration.
#![cfg_attr(test, allow(clippy::await_holding_lock))]

use super::ToolContext;
use super::client_bridge;
use anyhow::Result;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

/// Deadline for one `fs/read_text_file` or `fs/write_text_file` round trip.
///
/// Separate from the `ask` deadline on purpose: an editor answers a buffer read
/// at once, and a turn must not park forever when the client never answers.
const FS_TIMEOUT: Duration = Duration::from_secs(60);

/// ACP's "resource not found" code, which the fs methods use for a path the
/// client cannot read. The tools report it exactly like a missing local file.
const CLIENT_NOT_FOUND: i64 = -32002;

/// The error a local read reports for a path that does not exist.
///
/// A client answering [`CLIENT_NOT_FOUND`] has no OS error to hand back, and a
/// tool whose local branch would surface `read_to_string`'s error must not
/// change its text just because the client served the call. `2` is the code
/// `std` itself reports for a missing path (`ENOENT` on unix,
/// `ERROR_FILE_NOT_FOUND` on Windows), so the message matches the local one.
pub(super) fn missing_file_error() -> anyhow::Error {
    std::io::Error::from_raw_os_error(2).into()
}

/// Where a text read came from.
#[derive(Debug)]
pub(crate) enum TextRead {
    /// The client served the text, so the editor's buffer is the source of truth.
    Client(String),
    /// The client answered "no such path" ([`CLIENT_NOT_FOUND`]).
    NotFound,
    /// No client serves this turn: the caller reads local disk.
    Local,
}

/// The absolute path as the client must receive it.
///
/// A non-UTF-8 path cannot be sent without guessing, and guessing would read a
/// different file than the tool was asked about.
fn path_param(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("path is not valid UTF-8: {}", path.display()))
}

/// Read `path` through the client when it serves text reads.
///
/// [`TextRead::Local`] means the caller must use local disk: nothing answers
/// this turn (no ACP bridge, a turn another client drove) or the client did not
/// advertise `fs.readTextFile`.
pub(crate) async fn read_text(path: &Path, ctx: &ToolContext) -> Result<TextRead> {
    read_text_within(path, ctx, FS_TIMEOUT).await
}

/// [`read_text`] with a caller-owned deadline, as [`client_bridge::ClientBridge::call`] takes one.
pub(crate) async fn read_text_within(path: &Path, ctx: &ToolContext, timeout: Duration) -> Result<TextRead> {
    let Some(bridge) = client_bridge::for_session(&ctx.session_id) else {
        return Ok(TextRead::Local);
    };
    if !bridge.caps().fs_read_text_file {
        return Ok(TextRead::Local);
    }
    let params = json!({ "path": path_param(path)? });
    let (_, answer) = bridge
        .call_answer("fs/read_text_file", params, Some(ctx.tool_call_id.clone()), timeout)
        .await?;
    match answer {
        Ok(value) => value
            .get("content")
            .and_then(Value::as_str)
            .map(|content| TextRead::Client(content.to_string()))
            .ok_or_else(|| {
                anyhow::anyhow!("the ACP client answered fs/read_text_file without a string `content` field")
            }),
        Err(error) if error.code == CLIENT_NOT_FOUND => Ok(TextRead::NotFound),
        Err(error) => Err(anyhow::anyhow!(
            "the ACP client answered fs/read_text_file with error {}: {}",
            error.code,
            error.message
        )),
    }
}

/// Write `content` through the client when it serves text writes.
///
/// `Ok(true)` means the client holds the new text; `Ok(false)` means the caller
/// must write local disk. An `Err` means the client was asked and refused or
/// failed: the caller must report it and must not write disk instead.
pub(crate) async fn write_text(path: &Path, content: &str, ctx: &ToolContext) -> Result<bool> {
    write_text_within(path, content, ctx, FS_TIMEOUT).await
}

/// [`write_text`] with a caller-owned deadline.
pub(crate) async fn write_text_within(
    path: &Path,
    content: &str,
    ctx: &ToolContext,
    timeout: Duration,
) -> Result<bool> {
    let Some(bridge) = client_bridge::for_session(&ctx.session_id) else {
        return Ok(false);
    };
    if !bridge.caps().fs_write_text_file {
        return Ok(false);
    }
    let params = json!({ "path": path_param(path)?, "content": content });
    bridge
        .call("fs/write_text_file", params, Some(ctx.tool_call_id.clone()), timeout)
        .await?;
    Ok(true)
}

/// Previous text of `path` for diff and stats, as the client and then the disk see it.
///
/// `None` means unreadable, the same contract as [`super::file_diff::snapshot`],
/// so a caller never claims a diff it could not compute. A client that fails
/// costs the diff its fidelity, never a write: writes go through
/// [`write_text`] and report their own failure.
pub(crate) async fn text_snapshot(path: &Path, ctx: &ToolContext) -> Option<(bool, String)> {
    match read_text(path, ctx).await {
        Ok(TextRead::Client(text)) => Some((true, text)),
        Ok(TextRead::NotFound) => Some((false, String::new())),
        Ok(TextRead::Local) | Err(_) => super::file_diff::snapshot(path).await,
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::client_bridge::{self, ClientAnswer};
    use crate::protocol::{AcpClientCapabilities, ClientRpcError, ServerEvent};
    use crate::tool::acp_session;
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::{Arc, Mutex, PoisonError};
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;

    /// What the fake client answers.
    pub(crate) enum Reply {
        /// An editor with an unsaved buffer: reads come from the buffer, then
        /// from disk, and writes land in the buffer only.
        EditorBuffer,
        /// Every request answers with this fixed JSON result.
        Fixed(Value),
        /// Every request answers with this JSON-RPC error.
        Fail { code: i64, message: String },
        /// No answer at all, so only the caller's deadline ends the wait.
        Silent,
    }

    type Shared<T> = Arc<Mutex<T>>;

    fn shared<T>(value: T) -> Shared<T> {
        Arc::new(Mutex::new(value))
    }

    fn lock<T>(shared: &Shared<T>) -> std::sync::MutexGuard<'_, T> {
        shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A fake ACP client installed for one session through the todo-3 API.
    ///
    /// It answers `fs/*` from an in-memory buffer and records every method it
    /// was asked, which is what a delegation test needs to prove both that the
    /// client served the call and that it was never called when it should not be.
    pub(crate) struct FakeClient {
        pub(crate) session: String,
        buffer: Shared<HashMap<String, String>>,
        calls: Shared<Vec<(String, Value)>>,
        session_token: u64,
        bridge_token: u64,
        turn: Option<acp_session::TurnOwnership>,
        responder: JoinHandle<()>,
    }

    impl FakeClient {
        /// Install a fake client whose own connection drives the running turn.
        pub(crate) fn install(session: &str, caps: AcpClientCapabilities, reply: Reply) -> Self {
            Self::with_turn(session, caps, reply, true)
        }

        /// Install a fake client on a session whose running turn another client
        /// started, the way a desktop turn on an ACP-attached session looks.
        pub(crate) fn install_for_desktop_turn(session: &str, caps: AcpClientCapabilities, reply: Reply) -> Self {
            Self::with_turn(session, caps, reply, false)
        }

        fn with_turn(session: &str, caps: AcpClientCapabilities, reply: Reply, own_turn: bool) -> Self {
            const CONNECTION: &str = "conn-fake-client";
            let (tx, mut rx) = mpsc::unbounded_channel();
            let session_token = acp_session::install(session, CONNECTION, tx.clone());
            let bridge_token = client_bridge::install(session, CONNECTION, caps, tx);
            let turn = Some(if own_turn {
                acp_session::claim_turn(session, Some(CONNECTION))
            } else {
                acp_session::claim_turn(session, Some("conn-desktop"))
            });

            let buffer = shared(HashMap::new());
            let calls = shared(Vec::new());
            let responder = {
                let buffer = Arc::clone(&buffer);
                let calls = Arc::clone(&calls);
                let session = session.to_string();
                tokio::spawn(async move {
                    while let Some(event) = rx.recv().await {
                        let ServerEvent::ClientRequest {
                            request_id,
                            method,
                            params,
                            ..
                        } = event
                        else {
                            continue;
                        };
                        lock(&calls).push((method.clone(), params.clone()));
                        let answer: ClientAnswer = match &reply {
                            Reply::EditorBuffer => serve_buffer(&buffer, &method, &params),
                            Reply::Fixed(value) => Ok(value.clone()),
                            Reply::Fail { code, message } => Err(ClientRpcError {
                                code: *code,
                                message: message.clone(),
                            }),
                            Reply::Silent => continue,
                        };
                        if let Some(bridge) = client_bridge::bridge_for_connection(&session, CONNECTION) {
                            let _ = bridge.complete(&request_id, answer);
                        }
                    }
                })
            };

            Self {
                session: session.to_string(),
                buffer,
                calls,
                session_token,
                bridge_token,
                turn,
                responder,
            }
        }

        /// Put `content` in the editor's unsaved buffer for `path`.
        pub(crate) fn set_buffer(&self, path: &Path, content: &str) {
            lock(&self.buffer).insert(path.to_string_lossy().into_owned(), content.to_string());
        }

        /// The buffer's text for `path`, if the client wrote one.
        pub(crate) fn buffer_of(&self, path: &Path) -> Option<String> {
            lock(&self.buffer).get(&path.to_string_lossy().into_owned()).cloned()
        }

        /// Methods the client was asked to serve, in order.
        pub(crate) fn asked(&self) -> Vec<String> {
            lock(&self.calls).iter().map(|(method, _)| method.clone()).collect()
        }
    }

    fn serve_buffer(buffer: &Shared<HashMap<String, String>>, method: &str, params: &Value) -> ClientAnswer {
        let path = params["path"].as_str().unwrap_or_default().to_string();
        match method {
            "fs/read_text_file" => {
                let buffered = lock(buffer).get(&path).cloned();
                match buffered.or_else(|| std::fs::read_to_string(&path).ok()) {
                    Some(content) => Ok(json!({ "content": content })),
                    None => Err(ClientRpcError {
                        code: -32002,
                        message: format!("no such file: {path}"),
                    }),
                }
            }
            "fs/write_text_file" => {
                let content = params["content"].as_str().unwrap_or_default().to_string();
                lock(buffer).insert(path, content);
                Ok(Value::Null)
            }
            other => Err(ClientRpcError {
                code: -32601,
                message: format!("method not found: {other}"),
            }),
        }
    }

    impl Drop for FakeClient {
        fn drop(&mut self) {
            self.responder.abort();
            drop(self.turn.take());
            client_bridge::clear(&self.session, self.bridge_token);
            acp_session::clear(&self.session, self.session_token);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{FakeClient, Reply};
    use super::*;
    use crate::protocol::AcpClientCapabilities;
    use crate::tool::{
        Tool, ToolContext, ToolExecutionMode, apply_patch::ApplyPatchTool, edit::EditTool, read::ReadTool,
        replace::ReplaceTool, undo::UndoTool, write::WriteTool,
    };
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    /// A per-test environment: the process-wide env lock plus a private
    /// `HORON_HOME`.
    ///
    /// Every mutation is accounted through `edit_stats::record`, which resolves
    /// `horon_agent_dir()`. Without this, running the tests writes the counters
    /// of these sessions into the user's real
    /// `~/.horon/sessions/edit-stats`. The lock is the one the hook and
    /// tool tests take, and the home is restored while it is still held.
    struct TestEnv {
        /// Kept only so the redirected home outlives the test.
        #[allow(dead_code)]
        home: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl TestEnv {
        fn new() -> Self {
            let lock = crate::storage::lock_test_env();
            let home = tempfile::tempdir().expect("temp home");
            let previous = std::env::var_os("HORON_HOME");
            crate::env::set_var("HORON_HOME", home.path());
            Self {
                home,
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for TestEnv {
        fn drop(&mut self) {
            // Runs before the lock guard field is dropped, so no test observes
            // the redirected home after it is gone.
            match self.previous.take() {
                Some(value) => crate::env::set_var("HORON_HOME", value),
                None => crate::env::remove_var("HORON_HOME"),
            }
        }
    }

    fn caps(read: bool, write: bool) -> AcpClientCapabilities {
        AcpClientCapabilities {
            fs_read_text_file: read,
            fs_write_text_file: write,
            terminal: false,
        }
    }

    fn ctx(session: &str, dir: &Path) -> ToolContext {
        ToolContext {
            session_id: session.to_string(),
            message_id: "msg-client-fs".to_string(),
            tool_call_id: "call-client-fs".to_string(),
            working_dir: Some(dir.to_path_buf()),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        }
    }

    fn body(output: &crate::tool::ToolOutput) -> String {
        output.output.clone()
    }

    fn diff_of(output: &crate::tool::ToolOutput) -> Option<String> {
        output
            .metadata
            .as_ref()
            .and_then(|meta| meta.get("diff"))
            .and_then(|diff| diff.as_str())
            .map(str::to_string)
    }

    #[tokio::test]
    async fn client_fs_read_returns_the_editor_buffer() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path: PathBuf = dir.path().join("notes.txt");
        std::fs::write(&path, "on disk\n").unwrap();
        let client = FakeClient::install("client-fs-read-buffer", caps(true, false), Reply::EditorBuffer);
        client.set_buffer(&path, "unsaved buffer\n");

        let text = read_text(&path, &ctx(&client.session, dir.path()))
            .await
            .expect("a served read succeeds");
        assert!(
            matches!(&text, TextRead::Client(text) if text == "unsaved buffer\n"),
            "a served read returns the buffer"
        );
        assert_eq!(client.asked(), ["fs/read_text_file"]);
    }

    #[tokio::test]
    async fn read_uses_disk_without_capability() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "disk text\n").unwrap();
        let client = FakeClient::install("client-fs-no-caps", caps(false, false), Reply::EditorBuffer);

        assert!(
            matches!(
                read_text(&path, &ctx(&client.session, dir.path())).await.unwrap(),
                TextRead::Local
            ),
            "an unadvertised read is left to local disk"
        );
        assert!(
            !write_text(&path, "text\n", &ctx(&client.session, dir.path()))
                .await
                .unwrap()
        );

        let output = ReadTool::new()
            .execute(
                json!({ "file_path": path.to_string_lossy() }),
                ctx(&client.session, dir.path()),
            )
            .await
            .expect("the local read works");
        assert!(body(&output).contains("disk text"), "{}", body(&output));
        assert!(
            client.asked().is_empty(),
            "an unadvertised method must never reach the client"
        );
    }

    #[tokio::test]
    async fn client_fs_write_goes_to_the_client() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "on disk\n").unwrap();
        let client = FakeClient::install("client-fs-write", caps(false, true), Reply::EditorBuffer);

        assert!(
            write_text(&path, "through the client\n", &ctx(&client.session, dir.path()))
                .await
                .unwrap()
        );
        assert_eq!(client.buffer_of(&path).as_deref(), Some("through the client\n"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "on disk\n");
        assert_eq!(client.asked(), ["fs/write_text_file"]);
    }

    #[tokio::test]
    async fn client_fs_read_times_out_when_the_client_never_answers() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "on disk\n").unwrap();
        let client = FakeClient::install("client-fs-timeout", caps(true, false), Reply::Silent);

        let error = read_text_within(&path, &ctx(&client.session, dir.path()), Duration::from_millis(50))
            .await
            .expect_err("an unanswered request must end at the deadline");
        assert_eq!(error.to_string(), "client request timed out");
    }

    #[tokio::test]
    async fn client_fs_rejects_a_read_answer_without_text_content() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "on disk\n").unwrap();
        let client = FakeClient::install(
            "client-fs-malformed",
            caps(true, false),
            Reply::Fixed(json!({ "content": 42 })),
        );

        let error = read_text(&path, &ctx(&client.session, dir.path()))
            .await
            .expect_err("a non-text answer is an error");
        assert!(
            error.to_string().contains("without a string `content` field"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn read_uses_client_overlay_when_capable() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "stale disk line\n").unwrap();
        let client = FakeClient::install("client-fs-read-tool", caps(true, false), Reply::EditorBuffer);
        client.set_buffer(&path, "unsaved buffer line\n");

        let output = ReadTool::new()
            .execute(
                json!({ "file_path": path.to_string_lossy() }),
                ctx(&client.session, dir.path()),
            )
            .await
            .expect("read succeeds");

        let text = body(&output);
        assert!(text.contains("unsaved buffer line"), "{text}");
        assert!(!text.contains("stale disk line"), "{text}");
        assert_eq!(client.asked(), ["fs/read_text_file"]);
    }

    #[tokio::test]
    async fn read_tool_keeps_binary_paths_on_local_disk() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("picture.png");
        std::fs::write(&path, [0x89u8, b'P', b'N', b'G']).unwrap();
        let client = FakeClient::install("client-fs-read-binary", caps(true, false), Reply::EditorBuffer);

        let output = ReadTool::new()
            .execute(
                json!({ "file_path": path.to_string_lossy() }),
                ctx(&client.session, dir.path()),
            )
            .await
            .expect("the local image path still works");

        assert!(body(&output).contains("Image:"), "{}", body(&output));
        assert!(
            client.asked().is_empty(),
            "an image read must never be sent to fs/read_text_file"
        );
    }

    #[tokio::test]
    async fn write_goes_to_client_when_capable() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "on disk\n").unwrap();
        let client = FakeClient::install("client-fs-write-tool", caps(true, true), Reply::EditorBuffer);

        let output = WriteTool::new()
            .execute(
                json!({ "file_path": path.to_string_lossy(), "content": "edited in the buffer\n" }),
                ctx(&client.session, dir.path()),
            )
            .await
            .expect("the client write succeeds");

        assert_eq!(client.buffer_of(&path).as_deref(), Some("edited in the buffer\n"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "on disk\n",
            "a client-served write must leave the disk file untouched"
        );
        assert!(body(&output).contains("file on disk is unchanged"), "{}", body(&output));
        let diff = diff_of(&output).expect("the diff is computed from the buffer");
        assert!(diff.contains("+edited in the buffer"), "{diff}");
    }

    #[tokio::test]
    async fn write_tool_reports_a_client_failure_and_writes_nothing() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        let client = FakeClient::install(
            "client-fs-write-failure",
            caps(true, true),
            Reply::Fail {
                code: -32000,
                message: "read-only buffer".to_string(),
            },
        );

        let error = WriteTool::new()
            .execute(
                json!({ "file_path": path.to_string_lossy(), "content": "half a write\n" }),
                ctx(&client.session, dir.path()),
            )
            .await
            .expect_err("a refused client write is a tool error");

        assert!(error.to_string().contains("read-only buffer"), "{error}");
        assert!(!path.exists(), "a failed client write must not fall back to disk");
    }

    #[tokio::test]
    async fn client_served_write_records_no_undo_unit() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "on disk\n").unwrap();
        let client = FakeClient::install("client-fs-undo", caps(true, true), Reply::EditorBuffer);
        let ctx = ctx(&client.session, dir.path());

        WriteTool::new()
            .execute(
                json!({ "file_path": path.to_string_lossy(), "content": "buffer text\n" }),
                ctx.clone(),
            )
            .await
            .expect("the client write succeeds");

        let error = UndoTool::new()
            .execute(json!({}), ctx)
            .await
            .expect_err("nothing on disk changed, so there is nothing to undo");
        assert!(error.to_string().contains("Nothing to undo"), "{error}");
    }

    #[tokio::test]
    async fn edit_reads_and_writes_through_client() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "stale disk line\n").unwrap();
        let client = FakeClient::install("client-fs-edit-tool", caps(true, true), Reply::EditorBuffer);
        client.set_buffer(&path, "unsaved buffer line\n");

        let output = EditTool::new()
            .execute(
                json!({
                    "file_path": path.to_string_lossy(),
                    "old_string": "unsaved buffer line",
                    "new_string": "edited buffer line"
                }),
                ctx(&client.session, dir.path()),
            )
            .await
            .expect("the edit applies to the buffer");

        assert_eq!(client.buffer_of(&path).as_deref(), Some("edited buffer line\n"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "stale disk line\n");
        assert!(body(&output).contains("edited buffer line"), "{}", body(&output));
    }

    #[tokio::test]
    async fn replace_tool_replaces_in_the_unsaved_buffer() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "name = old\n").unwrap();
        let client = FakeClient::install("client-fs-replace-tool", caps(true, true), Reply::EditorBuffer);
        client.set_buffer(&path, "name = unsaved\n");

        let output = ReplaceTool::new()
            .execute(
                json!({
                    "pattern": "unsaved",
                    "replacement": "replaced",
                    "path": path.to_string_lossy()
                }),
                ctx(&client.session, dir.path()),
            )
            .await
            .expect("the replacement applies to the buffer");

        assert_eq!(client.buffer_of(&path).as_deref(), Some("name = replaced\n"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "name = old\n");
        assert!(body(&output).contains("Replaced 1 match"), "{}", body(&output));
    }

    #[tokio::test]
    async fn apply_patch_tool_updates_the_unsaved_buffer() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "stale disk line\n").unwrap();
        let client = FakeClient::install("client-fs-apply-patch", caps(true, true), Reply::EditorBuffer);
        client.set_buffer(&path, "unsaved buffer line\n");

        let output = ApplyPatchTool::new()
            .execute(
                json!({
                    "patch_text": format!(
                        "*** Begin Patch\n*** Update File: {}\n@@\n-unsaved buffer line\n+patched buffer line\n*** End Patch",
                        path.to_string_lossy()
                    )
                }),
                ctx(&client.session, dir.path()),
            )
            .await
            .expect("the patch applies to the buffer");

        assert_eq!(client.buffer_of(&path).as_deref(), Some("patched buffer line\n"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "stale disk line\n");
        assert!(body(&output).contains("patched buffer line"), "{}", body(&output));
    }

    #[tokio::test]
    async fn apply_patch_tool_routes_unified_diffs_through_the_client() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "stale disk line\n").unwrap();
        let client = FakeClient::install("client-fs-unified-patch", caps(true, true), Reply::EditorBuffer);
        client.set_buffer(&path, "unsaved buffer line\n");

        let patch = format!(
            "--- {0}\n+++ {0}\n@@ -1,1 +1,1 @@\n-unsaved buffer line\n+patched buffer line\n",
            path.to_string_lossy()
        );
        let output = ApplyPatchTool::new()
            .execute(json!({ "patch_text": patch }), ctx(&client.session, dir.path()))
            .await
            .expect("the unified diff applies to the buffer");

        assert_eq!(client.buffer_of(&path).as_deref(), Some("patched buffer line\n"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "stale disk line\n");
        assert!(body(&output).contains("patched buffer line"), "{}", body(&output));
    }

    #[tokio::test]
    async fn apply_patch_delete_stays_local() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "delete me\n").unwrap();
        let client = FakeClient::install("client-fs-delete-local", caps(true, true), Reply::EditorBuffer);

        let output = ApplyPatchTool::new()
            .execute(
                json!({
                    "patch_text": format!(
                        "*** Begin Patch\n*** Delete File: {}\n*** End Patch",
                        path.to_string_lossy()
                    )
                }),
                ctx(&client.session, dir.path()),
            )
            .await
            .expect("the delete runs");

        assert!(!path.exists(), "a delete removes the disk file");
        assert!(body(&output).contains("deleted"), "{}", body(&output));
        assert!(
            !client.asked().iter().any(|method| method == "fs/write_text_file"),
            "a delete has no client counterpart and must not become a client write"
        );
    }

    #[tokio::test]
    async fn client_not_found_maps_to_missing_file_text() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.txt");
        let expected = format!("File not found: {}", path.to_string_lossy());
        let client = FakeClient::install("client-fs-not-found", caps(true, false), Reply::EditorBuffer);

        let read_error = ReadTool::new()
            .execute(
                json!({ "file_path": path.to_string_lossy() }),
                ctx(&client.session, dir.path()),
            )
            .await
            .expect_err("neither the client nor the disk has this file");
        assert_eq!(read_error.to_string(), expected);

        let edit_error = EditTool::new()
            .execute(
                json!({
                    "file_path": path.to_string_lossy(),
                    "old_string": "a",
                    "new_string": "b"
                }),
                ctx(&client.session, dir.path()),
            )
            .await
            .expect_err("editing an absent file fails like it does locally");
        assert_eq!(edit_error.to_string(), expected);
    }

    #[tokio::test]
    async fn a_desktop_turn_on_an_acp_session_reads_and_writes_disk() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "on disk\n").unwrap();
        let client =
            FakeClient::install_for_desktop_turn("client-fs-desktop-turn", caps(true, true), Reply::EditorBuffer);

        let output = ReadTool::new()
            .execute(
                json!({ "file_path": path.to_string_lossy() }),
                ctx(&client.session, dir.path()),
            )
            .await
            .expect("the desktop turn reads disk");
        assert!(body(&output).contains("on disk"), "{}", body(&output));

        WriteTool::new()
            .execute(
                json!({ "file_path": path.to_string_lossy(), "content": "written on disk\n" }),
                ctx(&client.session, dir.path()),
            )
            .await
            .expect("the desktop turn writes disk");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "written on disk\n");
        assert!(
            client.asked().is_empty(),
            "a turn another client started must not reach the editor"
        );
    }

    #[tokio::test]
    async fn apply_patch_update_of_a_missing_file_reports_the_local_error_text() {
        let _env = TestEnv::new();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.txt");
        let patch = format!(
            "*** Begin Patch\n*** Update File: {}\n@@\n-a\n+b\n*** End Patch",
            path.to_string_lossy()
        );

        // Without a client the update fails on the local read; a client that
        // answers -32002 must read the same, so the reported text never depends
        // on which side served the file.
        let local = ApplyPatchTool::new()
            .execute(
                json!({ "patch_text": patch.clone() }),
                ctx("client-fs-update-missing-local", dir.path()),
            )
            .await
            .expect("a missing file is reported in the output, not as a tool error");
        let client = FakeClient::install("client-fs-update-missing-client", caps(true, true), Reply::EditorBuffer);
        let served = ApplyPatchTool::new()
            .execute(json!({ "patch_text": patch.clone() }), ctx(&client.session, dir.path()))
            .await
            .expect("a missing file is reported in the output, not as a tool error");

        assert_eq!(
            body(&served),
            body(&local),
            "a client -32002 update must report the same text as the local read"
        );
        assert!(
            body(&local).starts_with(&format!("\u{2717} {}: ", path.display())),
            "{}",
            body(&local)
        );
        assert!(!body(&local).contains("File not found"), "{}", body(&local));
    }
}
