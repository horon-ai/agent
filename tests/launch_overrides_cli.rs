//! `--role`, `--prewalk` and `--no-prewalk` against a loopback mock.
//!
//! The mock scripts one `todo` write and then the final text, so the second
//! request proves which model the session chose for execution after the
//! planning handoff. Every case runs the real binary in an owned HORON_HOME.

use serde_json::{Value, json};
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const RUN_DEADLINE: Duration = Duration::from_secs(90);
const FINAL_TEXT: &str = "pong";
/// One todo write is the planning handoff point.
const TODO_ARGS: &str =
    r#"{"todos":[{"content":"plan","status":"pending","priority":"normal","id":"1","confidence":"plausible"}]}"#;
/// Compaction work stays off so each run makes exactly the scripted requests.
const COMPACTION_HEAD: &str = "[compaction]\nbackground = false\nmax_context_tokens = 20000\nrecent_tokens = 2048\n\n";

struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("a temporary home"),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    async fn run(&self, args: &[&str]) -> Output {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_horon-agent"));
        command
            .current_dir(self.path())
            .env_clear()
            .env("HOME", self.path())
            .env("HORON_HOME", self.path())
            .env("XDG_CONFIG_HOME", self.path().join("config"))
            .env("XDG_CACHE_HOME", self.path().join("cache"))
            .env("HORON_AGENT_NON_INTERACTIVE", "1")
            .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
            .env("HORON_AGENT_MEMORY_ENABLED", "0")
            .env("HORON_AGENT_MEMORY_SIDECAR_ENABLED", "0")
            .env("HORON_AGENT_RUN_MCP", "0")
            // The scripted todo stays incomplete, so the run must not re-prompt for it.
            .env("HORON_AGENT_RUN_AUTO_POKE", "0")
            .env("NO_COLOR", "1")
            .env("PATH", std::env::var_os("PATH").unwrap_or_default());
        let mut child = command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("the CLI spawns");
        let stdout = child.stdout.take().expect("a stdout pipe");
        let stderr = child.stderr.take().expect("a stderr pipe");
        let deadline = tokio::time::Instant::now() + RUN_DEADLINE;
        let outcome = tokio::time::timeout_at(deadline, async {
            tokio::try_join!(child.wait(), drain(stdout), drain(stderr))
        })
        .await;
        match outcome {
            Ok(Ok((status, stdout, stderr))) => Output { status, stdout, stderr },
            Ok(Err(error)) => panic!("the CLI IO failed: {error}"),
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                panic!("the CLI did not finish within {RUN_DEADLINE:?}");
            }
        }
    }
}

async fn drain(mut reader: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf).await?;
    Ok(buf)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// A loopback `/v1/chat/completions` mock. With a tool, the first request gets
/// that tool call and the next request gets the final text; without one, every
/// request gets the final text.
struct Mock {
    base_url: String,
    requests: Arc<AtomicUsize>,
    bodies: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Mock {
    async fn start(tool: Option<&'static str>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind the mock");
        let port = listener.local_addr().expect("the mock address").port();
        let requests = Arc::new(AtomicUsize::new(0));
        let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let requests_thread = Arc::clone(&requests);
        let bodies_thread = Arc::clone(&bodies);
        let tool_name = tool;
        tokio::spawn(async move {
            loop {
                let Ok((socket, _)) = listener.accept().await else {
                    return;
                };
                let requests = Arc::clone(&requests_thread);
                let bodies = Arc::clone(&bodies_thread);
                tokio::spawn(async move {
                    let _ = serve(socket, &requests, &bodies, tool_name).await;
                });
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}/v1"),
            requests,
            bodies,
        }
    }

    fn requests(&self) -> usize {
        self.requests.load(Ordering::SeqCst)
    }

    fn request_text(&self) -> String {
        self.bodies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .join("\n")
    }

    fn models(&self) -> Vec<String> {
        self.bodies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .map(|body| {
                let value: Value = serde_json::from_str(body).expect("a recorded request body");
                value["model"].as_str().expect("the request names a model").to_string()
            })
            .collect()
    }
}

async fn serve(
    mut socket: tokio::net::TcpStream,
    requests: &AtomicUsize,
    bodies: &std::sync::Mutex<Vec<String>>,
    tool: Option<&'static str>,
) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = socket.read(&mut chunk).await?;
        assert!(read > 0, "the peer closed before sending a request");
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut length = 0usize;
    for header in headers.lines() {
        if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    while buffer.len() < header_end + length {
        let read = socket.read(&mut chunk).await?;
        assert!(read > 0, "the peer closed mid-body");
        buffer.extend_from_slice(&chunk[..read]);
    }
    bodies
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(String::from_utf8_lossy(&buffer[header_end..header_end + length]).into_owned());
    let round = requests.fetch_add(1, Ordering::SeqCst) + 1;

    let frame_of = |delta: Value, finish: Value| {
        json!({
            "id": "chatcmpl-launch",
            "object": "chat.completion.chunk",
            "model": "qa-model",
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
        })
    };
    let frames = match tool {
        Some(tool) if round == 1 => vec![
            frame_of(
                json!({"role": "assistant", "tool_calls": [{
                    "index": 0,
                    "id": "call_launch_1",
                    "type": "function",
                    "function": {"name": tool, "arguments": TODO_ARGS}
                }]}),
                Value::Null,
            ),
            frame_of(json!({}), json!("tool_calls")),
        ],
        _ => vec![
            frame_of(json!({"role": "assistant", "content": FINAL_TEXT}), Value::Null),
            frame_of(json!({}), json!("stop")),
        ],
    };
    let mut sse = String::new();
    for frame in frames {
        sse.push_str(&format!("data: {frame}\n\n"));
    }
    sse.push_str("data: [DONE]\n\n");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        sse.len(),
        sse
    );
    socket.write_all(response.as_bytes()).await?;
    socket.flush().await
}

/// A profile with three models and a prewalk section whose `into` is a role, so
/// a `--role plan=` override and a `--prewalk` override are both observable on
/// the wire.
fn write_launch_config(home: &Home, base_url: &str, prewalk_into: &str) {
    let config = format!(
        "{COMPACTION_HEAD}[provider]\ndefault_provider = \"qa\"\ndefault_model = \"qa-model\"\n\n\
[providers.qa]\ntype = \"openai-compatible\"\nbase_url = \"{base_url}\"\nauth = \"none\"\n\
default_model = \"qa-model\"\n\
models = [{{ id = \"qa-model\" }}, {{ id = \"plan-model\" }}, {{ id = \"exec-model\" }}]\n\n\
[prewalk]\nenabled = true\ninto = \"{prewalk_into}\"\n"
    );
    std::fs::write(home.path().join("config.toml"), config).expect("write config.toml");
}

#[tokio::test(flavor = "multi_thread")]
async fn launch_role_override_drives_the_prewalk_target_for_this_run_only() {
    let mock = Mock::start(Some("todo")).await;
    let home = Home::new();
    write_launch_config(&home, &mock.base_url, "role:plan");

    let output = home
        .run(&[
            "run",
            "--provider-profile",
            "qa",
            "--tool-profile",
            "full",
            "--role",
            "plan=plan-model",
            "hello",
        ])
        .await;
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stdout).contains(FINAL_TEXT));
    assert_eq!(mock.models(), vec!["qa-model", "plan-model"]);
    assert!(
        text(&output.stderr).contains("launch override: role plan -> plan-model"),
        "the run reports the override: {}",
        text(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_prewalk_wins_over_config_and_an_explicit_into() {
    let mock = Mock::start(Some("todo")).await;
    let home = Home::new();
    write_launch_config(&home, &mock.base_url, "exec-model");

    let output = home
        .run(&[
            "run",
            "--provider-profile",
            "qa",
            "--tool-profile",
            "full",
            "--no-prewalk",
            "--prewalk",
            "exec-model",
            "hello",
        ])
        .await;
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(mock.models(), vec!["qa-model", "qa-model"]);
    let stderr = text(&output.stderr);
    assert!(stderr.contains("launch override: prewalk off"), "{stderr}");
    assert!(stderr.contains("--prewalk exec-model ignored"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread")]
async fn prewalk_flag_overrides_the_configured_into() {
    let mock = Mock::start(Some("todo")).await;
    let home = Home::new();
    write_launch_config(&home, &mock.base_url, "role:plan");

    let output = home
        .run(&[
            "run",
            "--provider-profile",
            "qa",
            "--tool-profile",
            "full",
            "--prewalk",
            "exec-model",
            "hello",
        ])
        .await;
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(mock.models(), vec!["qa-model", "exec-model"]);
    assert!(
        text(&output.stderr).contains("launch override: prewalk into exec-model -> exec-model"),
        "{}",
        text(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn role_default_names_the_session_model_like_model_does() {
    let mock = Mock::start(None).await;
    let home = Home::new();
    write_launch_config(&home, &mock.base_url, "role:plan");

    let output = home
        .run(&[
            "run",
            "--provider-profile",
            "qa",
            "--role",
            "default=plan-model",
            "hello",
        ])
        .await;
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(mock.models()[0], "plan-model");
    assert!(
        text(&output.stderr).contains("launch override: role default -> plan-model"),
        "{}",
        text(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unusable_role_or_prewalk_values_fail_before_any_request() {
    let mock = Mock::start(None).await;
    let home = Home::new();
    write_launch_config(&home, &mock.base_url, "role:plan");

    for (args, expected) in [
        (vec!["run", "--role", "bogus=plan-model", "hello"], "unknown model role"),
        (vec!["run", "--role", "plan=", "hello"], "needs a model id"),
        (
            vec!["run", "--role", "plan=ghost-model", "hello"],
            "is not offered by provider profile",
        ),
        (vec!["run", "--prewalk", "role:judge", "hello"], "is not supported"),
        (vec!["run", "--skills", "bad pattern", "hello"], "contains"),
    ] {
        let output = home.run(&args).await;
        assert!(!output.status.success(), "{args:?} must fail");
        assert!(
            text(&output.stderr).contains(expected),
            "{args:?}: {}",
            text(&output.stderr)
        );
    }
    assert_eq!(mock.requests(), 0, "a rejected override sends no request");
}

const CONTEXT_MARKER: &str = "ZEBRA-CONTEXT-MARKER-7731";
const OVERLAY_MARKER: &str = "OTTER-OVERLAY-MARKER-4410";

#[tokio::test(flavor = "multi_thread")]
async fn no_context_files_drops_agents_md_but_keeps_the_base_prompt() {
    let mock = Mock::start(None).await;
    let home = Home::new();
    write_launch_config(&home, &mock.base_url, "role:plan");
    std::fs::write(home.path().join("AGENTS.md"), CONTEXT_MARKER).expect("write AGENTS.md");

    let default_run = home.run(&["run", "--provider-profile", "qa", "hello"]).await;
    assert!(default_run.status.success(), "{}", text(&default_run.stderr));
    assert!(
        mock.request_text().contains(CONTEXT_MARKER),
        "the default run reads AGENTS.md"
    );

    let skipped = Mock::start(None).await;
    write_launch_config(&home, &skipped.base_url, "role:plan");
    let output = home
        .run(&["run", "--provider-profile", "qa", "--no-context-files", "hello"])
        .await;
    assert!(output.status.success(), "{}", text(&output.stderr));
    let request = skipped.request_text();
    assert!(!request.contains(CONTEXT_MARKER), "AGENTS.md is skipped");
    assert!(request.contains("You are Horon,"), "the base prompt still applies");
    assert!(
        text(&output.stderr).contains("launch override: --no-context-files skips project AGENTS.md"),
        "{}",
        text(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_rules_drops_the_prompt_overlay_and_leaves_agents_md() {
    let mock = Mock::start(None).await;
    let home = Home::new();
    write_launch_config(&home, &mock.base_url, "role:plan");
    std::fs::write(home.path().join("AGENTS.md"), CONTEXT_MARKER).expect("write AGENTS.md");
    std::fs::create_dir_all(home.path().join(".horon")).expect("create .horon");
    std::fs::write(home.path().join(".horon").join("prompt-overlay.md"), OVERLAY_MARKER).expect("write overlay");

    let output = home
        .run(&["run", "--provider-profile", "qa", "--no-rules", "hello"])
        .await;
    assert!(output.status.success(), "{}", text(&output.stderr));
    let request = mock.request_text();
    assert!(!request.contains(OVERLAY_MARKER), "the overlay is skipped");
    assert!(
        request.contains(CONTEXT_MARKER),
        "AGENTS.md is a context file, not a rule"
    );
    assert!(
        text(&output.stderr).contains("launch override: --no-rules skips .horon/prompt-overlay.md"),
        "{}",
        text(&output.stderr)
    );
}

fn write_skill(home: &Home, name: &str) {
    let directory = home.path().join("skills").join(name);
    std::fs::create_dir_all(&directory).expect("create the skill directory");
    std::fs::write(
        directory.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: fixture skill {name}\n---\n\nbody of {name}\n"),
    )
    .expect("write SKILL.md");
}

#[tokio::test(flavor = "multi_thread")]
async fn skills_patterns_keep_only_matching_skills_in_the_prompt() {
    let home = Home::new();
    let default_mock = Mock::start(None).await;
    write_launch_config(&home, &default_mock.base_url, "role:plan");
    write_skill(&home, "alpha-review");
    write_skill(&home, "beta-deploy");

    let default_run = home.run(&["run", "--provider-profile", "qa", "hello"]).await;
    assert!(default_run.status.success(), "{}", text(&default_run.stderr));
    let default_request = default_mock.request_text();
    assert!(default_request.contains("alpha-review") && default_request.contains("beta-deploy"));

    let limited = Mock::start(None).await;
    write_launch_config(&home, &limited.base_url, "role:plan");
    let output = home
        .run(&["run", "--provider-profile", "qa", "--skills", "alpha-*", "hello"])
        .await;
    assert!(output.status.success(), "{}", text(&output.stderr));
    let request = limited.request_text();
    assert!(request.contains("alpha-review"), "an allowed skill stays");
    assert!(!request.contains("beta-deploy"), "a filtered skill is hidden");
    assert!(
        text(&output.stderr).contains("launch override: skills limited to alpha-*"),
        "{}",
        text(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_skills_wins_over_skills_and_hides_every_skill() {
    let home = Home::new();
    let mock = Mock::start(None).await;
    write_launch_config(&home, &mock.base_url, "role:plan");
    write_skill(&home, "alpha-review");

    let output = home
        .run(&[
            "run",
            "--provider-profile",
            "qa",
            "--no-skills",
            "--skills",
            "alpha-*",
            "hello",
        ])
        .await;
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(!mock.request_text().contains("alpha-review"));
    let stderr = text(&output.stderr);
    assert!(stderr.contains("launch override: skills off"), "{stderr}");
    assert!(stderr.contains("--skills alpha-* ignored"), "{stderr}");
}
