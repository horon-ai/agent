//! `horon run --approval`: a stricter approval mode for one run, checked against a real write call.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;

/// Serve OpenAI-compatible streamed replies: a request with no tool result asks for a `write` call, one with a result answers in text.
fn mock() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}/v1", listener.local_addr().unwrap().port());
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
            let body = String::from_utf8_lossy(&request[header_end..]).into_owned();
            // A run starts with the user's message; every later request follows a tool result.
            let after_tool = body.contains("\"role\":\"tool\"");
            let frame = |delta: &str, finish: &str| {
                format!(
                    "data: {{\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"model\":\"qa-model\",\"choices\":[{{\"index\":0,\"delta\":{delta},\"finish_reason\":{finish}}}]}}\n\n"
                )
            };
            let sse = if after_tool {
                format!(
                    "{}{}data: [DONE]\n\n",
                    frame("{\"role\":\"assistant\",\"content\":\"done\"}", "null"),
                    frame("{}", "\"stop\"")
                )
            } else {
                let call = "{\"role\":\"assistant\",\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"write\",\"arguments\":\"{\\\"file_path\\\":\\\"approval-probe.txt\\\",\\\"content\\\":\\\"written\\\"}\"}}]}";
                format!(
                    "{}{}data: [DONE]\n\n",
                    frame(call, "null"),
                    frame("{}", "\"tool_calls\"")
                )
            };
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{sse}",
                sse.len()
            );
            socket.write_all(reply.as_bytes()).unwrap();
        }
    });
    url
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
        .args(["run", "write the file", "--no-update"])
        .args(extra)
        .output()
        .expect("run horon-agent")
}

#[test]
fn approval_write_blocks_an_ordinary_write_for_that_run_only() {
    let home = tempfile::tempdir().unwrap();
    let probe = home.path().join("approval-probe.txt");
    let url = mock();

    let held = run(home.path(), &url, &["--approval", "write"]);
    assert!(held.status.success(), "{}", String::from_utf8_lossy(&held.stderr));
    assert!(
        !probe.exists(),
        "an unattended run must not write under --approval write"
    );

    let open = run(home.path(), &url, &[]);
    assert!(open.status.success(), "{}", String::from_utf8_lossy(&open.stderr));
    assert_eq!(
        std::fs::read_to_string(&probe).unwrap(),
        "written",
        "a later run keeps the configured mode"
    );
}
