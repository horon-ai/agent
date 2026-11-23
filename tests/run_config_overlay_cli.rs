//! `horon run --config <file>`: per-run configuration overlays, driven through the real binary.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

fn mock_chat() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/v1", listener.local_addr().unwrap().port());
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&bodies);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut socket) = stream else { return };
            let mut request = Vec::new();
            let mut chunk = [0u8; 4096];
            let (header_end, length) = loop {
                let read = socket.read(&mut chunk).unwrap();
                if read == 0 {
                    break (0, 0);
                }
                request.extend_from_slice(&chunk[..read]);
                if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_ascii_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.strip_prefix("content-length:")
                                .map(|value| value.trim().parse().unwrap_or(0))
                        })
                        .unwrap_or(0usize);
                    break (end + 4, length);
                }
            };
            while header_end > 0 && request.len() < header_end + length {
                let read = socket.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            recorded
                .lock()
                .unwrap()
                .push(String::from_utf8_lossy(&request[header_end..]).into_owned());
            let sse = concat!(
                "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"model\":\"overlay-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"model\":\"overlay-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n"
            );
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
                sse.len()
            );
            socket.write_all(reply.as_bytes()).unwrap();
        }
    });
    (url, bodies)
}

fn horon(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_horon-agent"))
        .current_dir(home)
        .env_clear()
        .env("HOME", home)
        .env("HORON_HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("HORON_AGENT_NON_INTERACTIVE", "1")
        .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
        .env("HORON_AGENT_MEMORY_ENABLED", "0")
        .env("NO_COLOR", "1")
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args(args)
        .output()
        .expect("run horon-agent")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn an_invalid_overlay_names_its_file_before_any_provider_work() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("broken-overlay.toml"), "[tools\nirc = true\n").unwrap();
    let result = horon(
        home.path(),
        &["run", "hi", "--no-update", "--config", "broken-overlay.toml"],
    );
    assert!(!result.status.success());
    assert!(stderr(&result).contains("broken-overlay.toml"), "{}", stderr(&result));
}

#[test]
fn an_overlay_is_applied_and_validated_for_that_run_only() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), "[tools]\nirc = false\n").unwrap();
    // `[prewalk] enabled` without `into` is invalid, so the run can only fail on it
    // if the overlay was merged over the file and checked.
    std::fs::write(home.path().join("overlay.toml"), "[prewalk]\nenabled = true\n").unwrap();

    let with = horon(home.path(), &["run", "hi", "--no-update", "--config", "overlay.toml"]);
    assert!(!with.status.success());
    assert!(stderr(&with).contains("[prewalk]"), "{}", stderr(&with));

    let without = horon(home.path(), &["run", "hi", "--no-update"]);
    assert!(!stderr(&without).contains("[prewalk]"), "{}", stderr(&without));
}

#[test]
fn overlay_applies_before_provider_profile_role_checks_and_default_tool_profile() {
    let home = tempfile::tempdir().unwrap();
    let (url, bodies) = mock_chat();
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "[compaction]\nbackground = false\n\n[provider]\ndefault_provider = \"base\"\ndefault_model = \"base-model\"\n\n[providers.base]\ntype = \"openai-compatible\"\nbase_url = \"{url}\"\nauth = \"none\"\ndefault_model = \"base-model\"\nmodels = [{{ id = \"base-model\" }}]\n"
        ),
    )
    .unwrap();
    std::fs::write(
        home.path().join("overlay.toml"),
        format!(
            "[tools]\nprofile = \"minimal\"\n\n[providers.overlay_qa]\ntype = \"openai-compatible\"\nbase_url = \"{url}\"\nauth = \"none\"\ndefault_model = \"overlay-model\"\nmodels = [{{ id = \"overlay-model\" }}, {{ id = \"plan-model\" }}]\n"
        ),
    )
    .unwrap();

    let ok = Command::new(env!("CARGO_BIN_EXE_horon-agent"))
        .current_dir(home.path())
        .env_clear()
        .env("HOME", home.path())
        .env("HORON_HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .env("XDG_CACHE_HOME", home.path().join("cache"))
        .env("HORON_AGENT_NON_INTERACTIVE", "1")
        .env("HORON_AGENT_NO_AUTO_UPDATE", "1")
        .env("HORON_AGENT_MEMORY_ENABLED", "0")
        .env("HORON_AGENT_RUN_MCP", "0")
        .env("NO_COLOR", "1")
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args([
            "--provider-profile",
            "overlay_qa",
            "run",
            "say ok",
            "--no-update",
            "--config",
            "overlay.toml",
            "--role",
            "plan=plan-model",
            "--prewalk",
            "role:plan",
        ])
        .output()
        .expect("run horon-agent");
    assert!(ok.status.success(), "stderr: {}", stderr(&ok));
    let recorded = bodies.lock().unwrap();
    assert_eq!(recorded.len(), 1);
    let body: serde_json::Value = serde_json::from_str(&recorded[0]).unwrap();
    let tool_names: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|tool| tool["function"]["name"].as_str())
        .collect();
    assert!(
        !tool_names.contains(&"toolbox"),
        "overlay `[tools] profile = \"minimal\"` must not be overridden by default core profile: {tool_names:?}"
    );
}
