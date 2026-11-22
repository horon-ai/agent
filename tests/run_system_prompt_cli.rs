//! `horon run --system-prompt` and `--append-system-prompt`, checked on the request the model receives.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

/// Serve one OpenAI-compatible streamed reply, recording the request body.
fn mock() -> (String, Arc<Mutex<Vec<String>>>) {
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
            let frame = |delta: &str, finish: &str| {
                format!(
                    "data: {{\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"model\":\"qa-model\",\"choices\":[{{\"index\":0,\"delta\":{delta},\"finish_reason\":{finish}}}]}}\n\n"
                )
            };
            let sse = format!(
                "{}{}data: [DONE]\n\n",
                frame("{\"role\":\"assistant\",\"content\":\"ok\"}", "null"),
                frame("{}", "\"stop\"")
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

fn run(home: &Path, url: &str, extra: &[&str]) -> std::process::Output {
    let config = format!(
        "[compaction]\nbackground = false\n\n[provider]\ndefault_provider = \"qa\"\ndefault_model = \"qa-model\"\n\n[providers.qa]\ntype = \"openai-compatible\"\nbase_url = \"{url}\"\nauth = \"none\"\ndefault_model = \"qa-model\"\nmodels = [{{ id = \"qa-model\" }}]\n"
    );
    std::fs::write(home.join("config.toml"), config).unwrap();
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
        .env("HORON_AGENT_RUN_MCP", "0")
        .env("NO_COLOR", "1")
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .args(["run", "say ok", "--no-update"])
        .args(extra)
        .output()
        .expect("run horon-agent")
}

#[test]
fn the_request_carries_the_replacement_or_the_suffix_and_a_default_run_carries_neither() {
    let home = tempfile::tempdir().unwrap();
    let (url, bodies) = mock();

    let replaced = run(home.path(), &url, &["--system-prompt", "ONLY-THIS-SYSTEM-TEXT"]);
    assert!(
        replaced.status.success(),
        "{}",
        String::from_utf8_lossy(&replaced.stderr)
    );
    let appended = run(home.path(), &url, &["--append-system-prompt", "SUFFIX-FOR-THIS-RUN"]);
    assert!(
        appended.status.success(),
        "{}",
        String::from_utf8_lossy(&appended.stderr)
    );
    let plain = run(home.path(), &url, &[]);
    assert!(plain.status.success(), "{}", String::from_utf8_lossy(&plain.stderr));

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    assert!(bodies[0].contains("ONLY-THIS-SYSTEM-TEXT") && !bodies[0].contains("SUFFIX-FOR-THIS-RUN"));
    assert!(bodies[1].contains("SUFFIX-FOR-THIS-RUN") && !bodies[1].contains("ONLY-THIS-SYSTEM-TEXT"));
    assert!(
        bodies[1].len() > bodies[0].len(),
        "an append keeps the composed prompt in place"
    );
    assert!(!bodies[2].contains("ONLY-THIS-SYSTEM-TEXT") && !bodies[2].contains("SUFFIX-FOR-THIS-RUN"));
}

#[test]
fn an_oversized_append_fails_before_any_request() {
    let home = tempfile::tempdir().unwrap();
    let (url, bodies) = mock();
    let big = "x".repeat(16 * 1024 + 1);
    let result = run(home.path(), &url, &["--append-system-prompt", &big]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("byte limit"));
    assert!(bodies.lock().unwrap().is_empty());
}
