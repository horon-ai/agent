use super::*;

#[test]
fn legacy_validation_report_is_only_accepted_after_authenticated_completion() {
    let success = br#"{"status":"authenticated","provider":"openai"}"#;
    let mut legacy = success.to_vec();
    legacy.extend_from_slice(b"\n=== auth-test: OpenAI ===\nresult: PASS\n");
    for operation in [Operation::Callback, Operation::Code, Operation::Complete] {
        assert_eq!(
            parse_login_response(&legacy, operation).unwrap()["status"],
            "authenticated"
        );
    }
    for operation in [Operation::Begin, Operation::Cancel] {
        assert!(parse_login_response(&legacy, operation).is_err());
    }
    for invalid in [
        b"banner\n{\"status\":\"authenticated\"}".as_slice(),
        b"{\"status\":\"pending\"}\n=== auth-test: OpenAI ===\n",
        b"{\"status\":\"authenticated\"}\n{\"status\":\"failed\"}",
        b"{\"status\":\"authenticated\"}\nunrecognized private output",
    ] {
        let error = parse_login_response(invalid, Operation::Callback).unwrap_err();
        assert_eq!(error.message, "Invalid login response. Update HoronAgent and retry.");
    }
}

#[cfg(unix)]
mod processes {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::thread;

    fn fixture(mode: &str) -> (tempfile::TempDir, AuthClient) {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("fixture.py");
        std::fs::write(
            &binary,
            r#"#!/usr/bin/env python3
import json, os, sys, time
from pathlib import Path
home = Path(os.environ['HORON_AGENT_HOME'])
args = sys.argv[1:]
provider = args[args.index('--provider') + 1]
flow = args[args.index('--flow-id') + 1]
mode = (home / 'mode').read_text()
(home / 'argv').write_text(json.dumps(args))
if '--cancel' in args:
    (home / ('cancel-' + flow)).write_text('cancelled')
    print(json.dumps(dict(status='cancelled', provider=provider)))
    sys.exit(0)
if '--print-auth-url' in args:
    if mode == 'oversized':
        print('X' * 70000)
        sys.exit(0)
    if mode == 'bad-json':
        print('private-fixture-secret')
        sys.exit(1)
    print(json.dumps(dict(status='pending', provider=provider,
        auth_url=(home / 'auth-url').read_text() if (home / 'auth-url').exists() else 'https://example.com/oauth?state=private-fixture-secret',
        input_kind='complete' if provider == 'copilot' else 'callback_url',
        user_code='ABCD-1234', expires_at_ms=9999999999999)))
    sys.exit(0)
if mode == 'hang':
    (home / 'pid').write_text(str(os.getpid()))
    time.sleep(60)
    sys.exit(1)
if mode == 'exchange-error':
    print('Error: Token exchange failed: private-fixture-secret', file=sys.stderr)
    sys.exit(1)
payload = sys.stdin.read()
(home / 'callback-input').write_text(payload)
(home / 'stdin-ok').write_text(str(payload == 'private-fixture-secret'))
print('private-fixture-secret', file=sys.stderr)
print(json.dumps(dict(status='authenticated', provider=provider)))
if mode.startswith('legacy-'):
    print('=== auth-test: Fixture ===')
    print('result: FAIL' if mode == 'legacy-warning' else 'result: PASS')
sys.exit(1 if mode in ('warning', 'legacy-warning') else 0)
"#,
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(dir.path().join("mode"), mode).unwrap();
        let client = AuthClient::new(AuthOptions {
            binary,
            horon_agent_home: Some(dir.path().to_owned()),
            socket: Some(dir.path().join("daemon.sock")),
            timeout: Duration::from_secs(3),
        });
        (dir, client)
    }

    #[test]
    fn oauth_round_trip_uses_stdin_and_scoped_flow_not_argv() {
        let (dir, client) = fixture("success");
        let flow = client.begin("OpenAI", Some("work")).unwrap();
        let prompt = flow.start().unwrap();
        assert_eq!(prompt.input_kind, AuthInputKind::CallbackUrl);
        assert!(flow.submit_code("private-fixture-secret").is_err());
        assert!(
            !flow
                .submit_callback("private-fixture-secret")
                .unwrap()
                .validation_warning
        );
        assert_eq!(std::fs::read_to_string(dir.path().join("stdin-ok")).unwrap(), "True");
        let args = std::fs::read_to_string(dir.path().join("argv")).unwrap();
        assert!(!args.contains("private-fixture-secret"));
        assert!(args.contains("--callback-url"));
        assert!(args.contains("--flow-id"));
        assert!(args.contains("daemon.sock"));
        assert!(!args.contains("--account"));
        assert!(flow.start().is_err());
    }

    #[test]
    fn saved_credentials_and_validation_failure_are_distinct() {
        let (dir, client) = fixture("warning");
        let listener = std::os::unix::net::UnixListener::bind(dir.path().join("daemon.sock")).unwrap();
        let notified = thread::spawn(move || {
            use std::io::BufRead;
            let (stream, _) = listener.accept().unwrap();
            // Best effort: Darwin rejects SO_RCVTIMEO with EINVAL on an accepted AF_UNIX stream
            // once the peer has closed, and the notifier drops its stream right after writing,
            // so that outcome is a normal race here. The timeout only guards a peer that never
            // closes; a payload that was already written is readable either way.
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut line = String::new();
            std::io::BufReader::new(stream).read_line(&mut line).unwrap();
            serde_json::from_str::<serde_json::Value>(&line).unwrap()
        });
        let flow = client.begin("openai", None).unwrap();
        flow.start().unwrap();
        assert!(
            flow.submit_callback("private-fixture-secret")
                .unwrap()
                .validation_warning
        );
        assert!(flow.submit_callback("private-fixture-secret").is_err());
        let request = notified.join().unwrap();
        assert_eq!(request["type"], "notify_auth_changed");
        assert_eq!(request["provider"], "openai");
        assert!(!request.to_string().contains("private-fixture-secret"));
    }

    #[test]
    fn legacy_cli_reports_preserve_completion_and_validation_warning() {
        for (mode, warning) in [("legacy-success", false), ("legacy-warning", true)] {
            let (_dir, client) = fixture(mode);
            let flow = client.begin("openai", None).unwrap();
            flow.start().unwrap();
            assert_eq!(
                flow.submit_callback("private-fixture-secret")
                    .unwrap()
                    .validation_warning,
                warning
            );
            assert!(flow.submit_callback("private-fixture-secret").is_err());
        }
    }

    #[test]
    fn errors_never_include_untrusted_output_or_callback_input() {
        for mode in ["bad-json", "oversized"] {
            let (_dir, client) = fixture(mode);
            let flow = client.begin("openai", None).unwrap();
            let error = flow.start().err().unwrap();
            assert!(!error.to_string().contains("private-fixture-secret"));
            flow.cancel().unwrap();
        }
    }

    #[test]
    fn last_drop_cleans_pending_flow_without_blocking_ui() {
        let (dir, client) = fixture("success");
        let flow = client.begin("openai", None).unwrap();
        flow.start().unwrap();
        let marker = dir.path().join(format!("cancel-{}", flow.0.flow_id));
        let clone = flow.clone();
        drop(flow);
        assert!(!marker.exists());
        drop(clone);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !marker.exists() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(marker.exists());
    }

    /// Serializes the callback-port lifecycle across the tests in this binary.
    ///
    /// Each of these tests reserves a loopback port, releases it so the product can bind it, keeps
    /// using that same fixed port for follow-up logins, and finally checks that the port was
    /// released. Every reservation in this binary draws from the same ephemeral pool, so a sibling
    /// reserving a port while one of those windows is open can take the port out from under the
    /// product, after which the product's bind and this test's checks fail on a port this test
    /// never lost. Holding this lock for the test's lifetime keeps every window this test's.
    static CALLBACK_PORT_WINDOW: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Bind `port` the way the product's own callback listener does, retrying briefly.
    ///
    /// A released callback port cannot be checked with a single plain bind. The test deliberately
    /// stalls an HTTP peer, which leaves the closed server side of that connection in
    /// FIN_WAIT_2, and Darwin reports EADDRINUSE for its port in that state even for a
    /// SO_REUSEADDR bind (SO_REUSEADDR only bypasses TIME_WAIT, which is exactly why the product
    /// sets it so its fixed OAuth port survives the next login). Once the stalled peer closes, the
    /// connection finishes tearing down and the port becomes bindable, so wait for that state
    /// instead of sampling once. A listener the product failed to release never becomes bindable.
    fn wait_for_released_port(port: u16) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let probe = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)
                .expect("create release probe socket");
            probe
                .set_reuse_address(true)
                .expect("set SO_REUSEADDR on release probe socket");
            match probe.bind(&std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)).into()) {
                Ok(()) => return,
                Err(error) => {
                    if Instant::now() >= deadline {
                        panic!("callback port {port} was not released: {error}");
                    }
                }
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn loopback_fixture() -> (
        tempfile::TempDir,
        AuthClient,
        std::net::TcpListener,
        std::sync::MutexGuard<'static, ()>,
    ) {
        let window = CALLBACK_PORT_WINDOW
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (dir, client) = fixture("success");
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = reserved.local_addr().unwrap().port();
        let mut url = url::Url::parse("https://example.com/oauth").unwrap();
        url.query_pairs_mut()
            .append_pair("redirect_uri", &format!("http://localhost:{port}/auth/callback"))
            .append_pair("state", "fixture-state");
        std::fs::write(dir.path().join("auth-url"), url.as_str()).unwrap();
        (dir, client, reserved, window)
    }

    fn send_callback(port: u16, target: &str) -> String {
        use std::net::TcpStream;
        let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        write!(stream, "GET {target} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n").unwrap();
        let mut response = String::new();
        if let Err(error) = stream.read_to_string(&mut response) {
            // Rejecting an oversized request can reset its unread remainder.
            assert_eq!(error.kind(), std::io::ErrorKind::ConnectionReset);
        }
        response
    }

    #[test]
    fn browser_callback_rejects_unrelated_requests_then_completes_over_stdin() {
        let (dir, client, reserved, _window) = loopback_fixture();
        let port = reserved.local_addr().unwrap().port();
        drop(reserved);
        let flow = client.begin("openai", None).unwrap();
        flow.start().unwrap();
        assert!(flow.has_callback_listener());
        let worker = flow.clone();
        let task = thread::spawn(move || worker.wait_for_callback());
        for target in [
            "/favicon.ico",
            "/wrong?state=fixture-state&code=secret",
            "/auth/callback?state=wrong&code=secret",
            "/auth/callback?state=fixture-state&state=wrong&code=secret",
            "/auth/callback?state=fixture-state&code=secret&code=duplicate",
            "/auth/callback?state=fixture-state",
            "/\\evil.invalid/auth/callback?state=fixture-state&code=secret",
        ] {
            let response = send_callback(port, target);
            assert!(response.starts_with("HTTP/1.1 400"));
            assert!(!response.contains("secret"));
            assert!(!dir.path().join("callback-input").exists());
        }
        let oversized = format!("/auth/callback?state=fixture-state&code={}", "x".repeat(INPUT_LIMIT));
        assert!(send_callback(port, &oversized).starts_with("HTTP/1.1 400"));
        assert!(!dir.path().join("callback-input").exists());
        let target = "/auth/callback?state=fixture-state&code=fixture-code";
        let response = send_callback(port, target);
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(!response.contains("fixture-code"));
        assert!(!task.join().unwrap().unwrap().validation_warning);
        assert!(!flow.has_callback_listener());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("callback-input")).unwrap(),
            format!("http://localhost:{port}{target}")
        );
        assert!(
            !std::fs::read_to_string(dir.path().join("argv"))
                .unwrap()
                .contains("fixture-code")
        );
        // Repeated logins on a provider's fixed port must work immediately,
        // even while the prior HTTP connection is in TIME_WAIT.
        let next = client.begin("openai", None).unwrap();
        next.start().unwrap();
        assert!(next.has_callback_listener());
        next.cancel().unwrap();
    }

    #[test]
    fn callback_denial_is_actionable_private_and_releases_listener() {
        let (dir, client, reserved, _window) = loopback_fixture();
        let port = reserved.local_addr().unwrap().port();
        drop(reserved);
        let flow = client.begin("openai", None).unwrap();
        flow.start().unwrap();
        let worker = flow.clone();
        let task = thread::spawn(move || {
            worker.wait_for_callback_with_progress(|| panic!("denied authorization must not enter token exchange"))
        });
        send_callback(
            port,
            "/auth/callback?state=fixture-state&error=access_denied&error_description=private-fixture-secret",
        );
        let error = task.join().unwrap().unwrap_err();
        assert!(error.message.contains("authorization was denied"));
        assert!(!error.message.contains("private-fixture-secret"));
        assert!(!dir.path().join("callback-input").exists());
        assert!(!flow.has_callback_listener());
        let next = client.begin("openai", None).unwrap();
        next.start().unwrap();
        assert!(next.has_callback_listener());
        next.cancel().unwrap();
        flow.cancel().unwrap();
    }

    #[test]
    fn exchange_failure_reports_stage_and_releases_callback_for_retry() {
        let (dir, client, reserved, _window) = loopback_fixture();
        let port = reserved.local_addr().unwrap().port();
        drop(reserved);
        std::fs::write(dir.path().join("mode"), "exchange-error").unwrap();
        let flow = client.begin("openai", None).unwrap();
        flow.start().unwrap();
        let worker = flow.clone();
        let (tx, rx) = mpsc::channel();
        let task = thread::spawn(move || {
            worker.wait_for_callback_with_progress(|| {
                tx.send(()).unwrap();
            })
        });
        send_callback(port, "/auth/callback?state=fixture-state&code=fixture-code");
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let error = task.join().unwrap().unwrap_err();
        assert!(error.message.contains("token exchange"));
        assert!(!error.message.contains("private-fixture-secret"));
        assert!(!flow.has_callback_listener());
        let next = client.begin("openai", None).unwrap();
        next.start().unwrap();
        assert!(next.has_callback_listener());
        next.cancel().unwrap();
        flow.cancel().unwrap();
    }

    #[test]
    fn cancel_releases_port_even_while_another_owner_retains_listener() {
        let (_dir, client, reserved, _window) = loopback_fixture();
        drop(reserved);
        let flow = client.begin("openai", None).unwrap();
        flow.start().unwrap();
        // Deterministically model a waiter still holding its Arc after cancel.
        let retained_listener = flow.0.callback.lock().unwrap().clone().unwrap();
        flow.cancel().unwrap();
        let next = client.begin("openai", None).unwrap();
        next.start().unwrap();
        assert!(next.has_callback_listener());
        drop(retained_listener);
        next.cancel().unwrap();
    }

    #[test]
    fn busy_callback_port_keeps_manual_completion_available() {
        let (_dir, client, _reserved, _window) = loopback_fixture();
        let flow = client.begin("openai", None).unwrap();
        flow.start().unwrap();
        assert!(!flow.has_callback_listener());
        assert!(flow.wait_for_callback().is_err());
        assert!(flow.submit_callback("private-fixture-secret").is_ok());
    }

    #[test]
    fn hosted_redirects_never_bind_local_listener() {
        let (dir, client) = fixture("success");
        for redirect in [
            "https://example.com/callback",
            "http://192.0.2.1:1234/callback",
            "http://user@localhost:1234/callback",
        ] {
            let mut url = url::Url::parse("https://example.com/oauth").unwrap();
            url.query_pairs_mut()
                .append_pair("redirect_uri", redirect)
                .append_pair("state", "fixture-state");
            std::fs::write(dir.path().join("auth-url"), url.as_str()).unwrap();
            let flow = client.begin("openai", None).unwrap();
            flow.start().unwrap();
            assert!(!flow.has_callback_listener());
            flow.cancel().unwrap();
        }
    }

    #[test]
    fn callback_wait_is_interrupted_by_cancel_and_manual_completion() {
        for cancel in [true, false] {
            let (_dir, client, reserved, _window) = loopback_fixture();
            let port = reserved.local_addr().unwrap().port();
            drop(reserved);
            let flow = client.begin("openai", None).unwrap();
            flow.start().unwrap();
            let worker = flow.clone();
            let task = thread::spawn(move || worker.wait_for_callback());
            // A stalled HTTP peer cannot prevent cancellation or manual input.
            let peer = std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
            let started = Instant::now();
            if cancel {
                flow.cancel().unwrap();
            } else {
                flow.submit_callback("private-fixture-secret").unwrap();
            }
            assert!(task.join().unwrap().is_err());
            assert!(started.elapsed() < Duration::from_secs(2));
            assert!(!flow.has_callback_listener());
            // The stalled peer is what keeps the port unusable, so close it and wait, bounded, for
            // the port to come back. A listener the product failed to release never comes back.
            drop(peer);
            wait_for_released_port(port);
        }
    }

    #[test]
    fn callback_timeout_keeps_manual_completion_available() {
        let (_dir, mut client, reserved, _window) = loopback_fixture();
        let port = reserved.local_addr().unwrap().port();
        drop(reserved);
        client.options.timeout = Duration::from_millis(300);
        let flow = client.begin("openai", None).unwrap();
        flow.start().unwrap();
        let _peer = std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
        let started = Instant::now();
        assert_eq!(flow.wait_for_callback().unwrap_err().kind, ErrorKind::Timeout);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(!flow.has_callback_listener());
        assert!(flow.submit_callback("private-fixture-secret").is_ok());
    }
}

#[test]
fn process_failure_diagnostics_never_echo_private_output() {
    for (stderr, expected) in [
        ("OAuth state mismatch: private-fixture-secret", "state mismatch"),
        ("Pending openai login state expired private-fixture-secret", "expired"),
        (
            "No pending openai login state found private-fixture-secret",
            "could not be found",
        ),
        (
            "unexpected argument '--flow-id' private-fixture-secret",
            "does not support",
        ),
        ("Permission denied private-fixture-secret", "permissions"),
        ("error sending request private-fixture-secret", "connection"),
        ("private-fixture-secret", "process failed"),
    ] {
        let error = login_process_failure(stderr.as_bytes());
        assert!(error.message.contains(expected), "{}", error.message);
        assert!(!error.message.contains("private-fixture-secret"));
    }
}
