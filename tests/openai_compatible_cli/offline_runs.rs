use super::*;

fn files_under(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    found.sort();
    found
}

/// Files a run leaves behind that belong to a session: its log, journal,
/// checkpoint, catalog and the recovery data built from them.
fn session_files(root: &Path) -> Vec<String> {
    files_under(root)
        .into_iter()
        .filter(|file| file.starts_with("sessions/") || file.starts_with("durable-runtime/"))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn run_without_session_leaves_no_session_files() {
    let mock = Mock::start().await;
    let home = Home::new();
    home.write_config(&mock.base_url);

    let output = home.run(&["run", "hi", "--no-session", "--no-update"]).await;
    assert!(
        output.status.success(),
        "run failed: status={:?}\nstdout={}\nstderr={}",
        output.status,
        text(&output.stdout),
        text(&output.stderr)
    );
    assert!(text(&output.stdout).contains(FINAL_TEXT));
    assert!(tool_round_seen(&mock.bodies()), "the tool round still runs");
    assert_eq!(session_files(home.path()), Vec::<String>::new());
    assert!(
        !text(&output.stderr).to_lowercase().contains("resume"),
        "no recovery hint: {}",
        text(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn run_without_session_leaves_no_session_files_when_the_provider_fails() {
    let home = Home::new();
    home.write_config("http://127.0.0.1:9/v1");

    let output = home.run(&["run", "hi", "--no-session", "--no-update"]).await;
    assert!(!output.status.success());
    assert_eq!(session_files(home.path()), Vec::<String>::new());
}

#[tokio::test(flavor = "multi_thread")]
async fn run_without_session_refuses_resume() {
    let mock = Mock::start().await;
    let home = Home::new();
    home.write_config(&mock.base_url);
    let first = home.run(&["run", "hi", "--no-update"]).await;
    assert!(first.status.success());
    let session_id = session_files(home.path())
        .iter()
        .find_map(|file| {
            file.strip_suffix("/events.jsonl")
                .and_then(|dir| dir.strip_prefix("sessions/"))
        })
        .expect("the first run stored a session")
        .to_string();

    let output = home
        .run(&["--resume", &session_id, "run", "hi", "--no-session", "--no-update"])
        .await;
    assert!(!output.status.success());
    assert!(text(&output.stderr).contains("--no-session cannot be combined"));
}

#[tokio::test(flavor = "multi_thread")]
async fn default_run_still_stores_its_session() {
    let mock = Mock::start().await;
    let home = Home::new();
    home.write_config(&mock.base_url);

    let output = home.run(&["run", "hi", "--no-update"]).await;
    assert!(output.status.success());
    let files = session_files(home.path());
    assert!(
        files.iter().any(|file| file.ends_with("/events.jsonl")),
        "a normal run keeps its event log: {files:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn provider_diagnose_separates_missing_config_missing_credential_and_unreachable_endpoint() {
    let secret = "sk-diagnose-masked-fixture-0123456789";
    let home = Home::new();

    let none = home.run(&["provider", "diagnose", "--json"]).await;
    assert_eq!(none.status.code(), Some(1));
    assert_eq!(
        serde_json::from_slice::<Value>(&none.stdout).unwrap()["status"],
        "no_providers"
    );

    let table = |extra: &str| {
        format!(
            "{COMPACTION_HEAD}[providers.keyed]\ntype = \"openai-compatible\"\nbase_url = \"https://llm.example.com/v1\"\n\
api_key_env = \"{KEY_ENV}\"\ndefault_model = \"m\"\n{extra}"
        )
    };
    std::fs::write(home.path().join("config.toml"), table("")).unwrap();
    let missing = home.run(&["provider", "diagnose", "--json"]).await;
    assert_eq!(missing.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert_eq!(report["profiles"][0]["outcome"], "missing_credential");

    // Credential present, local endpoint closed: reachability fails without leaking the key.
    let local = format!(
        "{COMPACTION_HEAD}[providers.local]\ntype = \"openai-compatible\"\nbase_url = \"http://127.0.0.1:1/v1\"\n\
api_key_env = \"{KEY_ENV}\"\ndefault_model = \"m\"\n"
    );
    std::fs::write(home.path().join("config.toml"), local).unwrap();
    let unreachable = home
        .run_with_env(&["provider", "diagnose", "--check", "--json"], &[(KEY_ENV, secret)])
        .await;
    assert_eq!(unreachable.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&unreachable.stdout).unwrap();
    assert_eq!(report["profiles"][0]["outcome"], "unreachable");
    assert!(!text(&unreachable.stdout).contains(secret) && !text(&unreachable.stderr).contains(secret));

    // Without --check no request is made, so the same profile is merely configured.
    let configured = home
        .run_with_env(&["provider", "diagnose", "--json"], &[(KEY_ENV, secret)])
        .await;
    assert!(configured.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&configured.stdout).unwrap()["profiles"][0]["outcome"],
        "configured"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn preview_runs_read_only_tools_without_a_session_and_redacts_search_output() {
    let secret = "sk-proj-abcdefghijklmnopqrstuvwxyz012345";
    let home = Home::new();
    std::fs::write(home.path().join("sample.txt"), "alpha\nneedle one\nbeta\nneeedle two\n").unwrap();

    let read = home.run(&["preview", "read", r#"{"file_path":"sample.txt"}"#]).await;
    assert!(read.status.success(), "{}", text(&read.stderr));
    assert!(text(&read.stdout).contains("needle one"));

    let grep = home
        .run(&[
            "preview",
            "agentgrep",
            r#"{"query":"ne+dle","regex":true,"paths_only":false}"#,
            "--json",
        ])
        .await;
    assert!(grep.status.success(), "{}", text(&grep.stderr));
    let grep: Value = serde_json::from_slice(&grep.stdout).unwrap();
    assert_eq!(grep["tool"], "agentgrep");
    assert!(grep["output"].as_str().unwrap().contains("needle"));

    assert!(
        !home.path().join("sessions").exists(),
        "a preview must not create a session"
    );
    let refused = home.run(&["preview", "bash", r#"{"command":"true"}"#]).await;
    assert!(!refused.status.success());

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await.unwrap();
        let body = format!(
            r#"{{"results":[{{"title":"Found","url":"https://example.org/a","content":"OPENAI_API_KEY={secret}"}}]}}"#
        );
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(reply.as_bytes()).await.unwrap();
    });
    std::fs::write(
        home.path().join("config.toml"),
        format!("{COMPACTION_HEAD}[websearch]\nengine = \"searxng\"\nfallback_engines = []\nsearxng_url = \"http://127.0.0.1:{port}\"\n"),
    )
    .unwrap();
    let search = home
        .run(&["preview", "websearch", r#"{"query":"anything"}"#, "--json"])
        .await;
    assert!(search.status.success(), "{}", text(&search.stderr));
    server.await.unwrap();
    let search: Value = serde_json::from_slice(&search.stdout).unwrap();
    assert_eq!(search["backend"], "searxng");
    let output = search["output"].as_str().unwrap();
    assert!(output.contains("Found") && !output.contains(secret), "{output}");
}

#[tokio::test(flavor = "multi_thread")]
async fn diagnostics_lists_unchecked_files_and_refuses_a_bad_delegation_before_any_model_call() {
    let home = Home::new();
    std::fs::write(home.path().join("notes.txt"), "plain text").unwrap();

    let listing = home.run(&["diagnostics", "notes.txt", "missing.rs", "--json"]).await;
    assert!(listing.status.success(), "{}", text(&listing.stderr));
    let reports: Value = serde_json::from_slice(&listing.stdout).unwrap();
    assert_eq!(reports[0]["path"], "notes.txt");
    assert_eq!(reports[0]["note"], "no language server for this file type");
    assert_eq!(reports[1]["note"], "file not found");

    // A number that is not in the listing is refused, so no run (and no model call) starts.
    let mock = Mock::start().await;
    home.write_config(&mock.base_url);
    let bad = home
        .run(&["diagnostics", "notes.txt", "--select", "1", "--delegate"])
        .await;
    assert!(!bad.status.success());
    assert!(text(&bad.stderr).contains("no diagnostic [1]"), "{}", text(&bad.stderr));
    assert_eq!(mock.requests(), 0, "a refused delegation must not reach the model");

    // Delegating needs a selection and a selection needs --delegate.
    assert!(
        !home
            .run(&["diagnostics", "notes.txt", "--delegate"])
            .await
            .status
            .success()
    );
    assert!(
        !home
            .run(&["diagnostics", "notes.txt", "--select", "1"])
            .await
            .status
            .success()
    );
}
