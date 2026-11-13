use super::*;
use crate::cli::provider_init::ProviderChoice;

#[test]
fn native_ssh_attach_arguments_parse_and_do_not_steal_local_socket() {
    let args = Args::try_parse_from([
        "horon-agent",
        "--ssh",
        "dev",
        "--ssh-binary",
        "/opt/horon-agent",
        "--ssh-server-socket",
        "/run/native/horon-agent.sock",
        "--remote-working-dir",
        "/srv/project",
    ])
    .unwrap();
    assert_eq!(args.ssh.as_deref(), Some("dev"));
    assert_eq!(args.ssh_binary.as_deref(), Some("/opt/horon-agent"));
    assert_eq!(args.ssh_server_socket.as_deref(), Some("/run/native/horon-agent.sock"));
    assert_eq!(args.remote_working_dir.as_deref(), Some("/srv/project"));
    assert!(args.socket.is_none());
    for argv in [
        vec!["horon-agent", "--ssh", "dev", "--socket", "/local.sock"],
        vec!["horon-agent", "--ssh-binary", "/opt/horon-agent"],
        vec!["horon-agent", "--ssh-server-socket", "/remote.sock"],
    ] {
        assert!(Args::try_parse_from(argv).is_err());
    }
}

#[cfg(unix)]
#[test]
fn native_server_stdio_preserves_socket_override() {
    let args = Args::try_parse_from(["horon-agent", "--socket", "/run/native.sock", "server", "stdio"]).unwrap();
    assert_eq!(args.socket.as_deref(), Some("/run/native.sock"));
    assert!(matches!(
        args.command,
        Some(Command::Server {
            action: ServerCommand::Stdio
        })
    ));
}

#[test]
fn server_start_and_internal_keepalive_parse() {
    let args = Args::try_parse_from(["horon-agent", "server", "start", "--json"]).expect("server start should parse");
    assert!(matches!(
        args.command,
        Some(Command::Server {
            action: ServerCommand::Start { json: true }
        })
    ));

    let keepalive =
        Args::try_parse_from(["horon-agent", "server", "keepalive"]).expect("internal server keepalive should parse");
    assert!(matches!(
        keepalive.command,
        Some(Command::Server {
            action: ServerCommand::Keepalive
        })
    ));
}

#[test]
fn server_promote_parses_default_and_explicit_version() {
    let current = Args::try_parse_from(["horon-agent", "server", "promote", "--json"])
        .expect("server promote should default to current");
    assert!(matches!(
        current.command,
        Some(Command::Server {
            action: ServerCommand::Promote {
                version: None,
                json: true,
            }
        })
    ));

    let explicit = Args::try_parse_from(["horon-agent", "server", "promote", "abc1234-dirty-deadbeef"])
        .expect("server promote should accept an installed version label");
    assert!(matches!(
        explicit.command,
        Some(Command::Server {
            action: ServerCommand::Promote {
                version: Some(ref version),
                json: false,
            }
        }) if version == "abc1234-dirty-deadbeef"
    ));
}

#[test]
fn test_provider_choice_accepts_auto_compatible_and_profiles() {
    let args = Args::try_parse_from(["horon-agent", "--provider", "auto", "run", "smoke"]).unwrap();
    assert_eq!(args.provider, ProviderChoice::Auto);

    let args = Args::try_parse_from(["horon-agent", "--provider", "compat", "run", "smoke"]).unwrap();
    assert_eq!(args.provider, ProviderChoice::OpenaiCompatible);

    let args = Args::try_parse_from(["horon-agent", "--provider", "deepseek", "run", "smoke"]).unwrap();
    assert_eq!(args.provider, ProviderChoice::Profile("deepseek".to_string()));

    let error = Args::try_parse_from(["horon-agent", "--provider", "claude", "run", "smoke"])
        .expect_err("a removed vendor login is a usage error");
    assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
}

#[test]
fn serve_server_name_option_parses() {
    let args = Args::try_parse_from(["horon-agent", "serve", "--server-name", "mount-cloud/fabian"]).unwrap();
    match args.command {
        Some(Command::Serve { server_name, .. }) => {
            assert_eq!(server_name.as_deref(), Some("mount-cloud/fabian"));
        }
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn remote_working_dir_option_parses() {
    let args = Args::try_parse_from([
        "horon-agent",
        "--socket",
        "/tmp/horon-agent.sock",
        "--remote-working-dir",
        "/home/agent/project",
    ])
    .unwrap();

    assert_eq!(args.remote_working_dir.as_deref(), Some("/home/agent/project"));
}

#[test]
fn model_list_subcommand_parses() {
    let args = Args::try_parse_from(["horon-agent", "model", "list", "--json", "--verbose"]).unwrap();
    match args.command {
        Some(Command::Model(ModelCommand::List { json, verbose })) => {
            assert!(json);
            assert!(verbose);
        }
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn session_rename_subcommand_parses() {
    let args = Args::try_parse_from(["horon-agent", "session", "rename", "fox", "release planning", "--json"]).unwrap();
    match args.command {
        Some(Command::Session(SessionCommand::Rename {
            session,
            name,
            clear,
            json,
        })) => {
            assert_eq!(session, "fox");
            assert_eq!(name.as_deref(), Some("release planning"));
            assert!(!clear);
            assert!(json);
        }
        other => panic!("unexpected command: {:?}", other),
    }

    let args = Args::try_parse_from(["horon-agent", "session", "rename", "fox", "--clear"]).unwrap();
    match args.command {
        Some(Command::Session(SessionCommand::Rename {
            session,
            name,
            clear,
            json,
        })) => {
            assert_eq!(session, "fox");
            assert!(name.is_none());
            assert!(clear);
            assert!(!json);
        }
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn trust_workspace_is_global_and_defaults_off() {
    let plain = Args::try_parse_from(["horon-agent", "run", "hi"]).unwrap();
    assert!(!plain.trust_workspace);

    let trusted = Args::try_parse_from(["horon-agent", "--trust-workspace", "run", "hi"]).unwrap();
    assert!(trusted.trust_workspace);

    let trailing = Args::try_parse_from(["horon-agent", "run", "hi", "--trust-workspace"]).unwrap();
    assert!(trailing.trust_workspace);
}

#[test]
fn quiet_global_flag_parses() {
    let args = Args::try_parse_from(["horon-agent", "--quiet", "model", "list"]).unwrap();
    assert!(args.quiet);
}

#[test]
fn acp_subcommand_parses() {
    let args = Args::try_parse_from(["horon-agent", "acp"]).unwrap();
    match args.command {
        Some(Command::Acp) => {}
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn rpc_subcommand_parses() {
    let args = Args::try_parse_from(["horon-agent", "rpc", "--session-dir", "/tmp/s"]).unwrap();
    match args.command {
        Some(Command::Rpc { session_dir }) => assert_eq!(session_dir.as_deref(), Some("/tmp/s")),
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn rpc_mode_flag_parses() {
    let mut args = Args::try_parse_from(["horon-agent", "--mode", "rpc"]).unwrap();
    assert_eq!(args.mode.as_deref(), Some("rpc"));
    assert!(args.command.is_none());
    args.promote_rpc_mode();
    assert!(matches!(args.command, Some(Command::Rpc { session_dir: None })));
}

#[test]
fn rpc_subcommand_wins_over_the_mode_flag() {
    let mut args = Args::try_parse_from(["horon-agent", "--mode", "rpc", "run", "hello"]).unwrap();
    args.promote_rpc_mode();
    assert!(matches!(args.command, Some(Command::Run { .. })));
}

#[test]
fn run_json_subcommand_parses() {
    let args = Args::try_parse_from(["horon-agent", "run", "--json", "hello"]).unwrap();
    match args.command {
        Some(Command::Run { json, ndjson, message }) => {
            assert!(json);
            assert!(!ndjson);
            assert_eq!(message, "hello");
        }
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn run_ndjson_subcommand_parses() {
    let args = Args::try_parse_from(["horon-agent", "run", "--ndjson", "hello"]).unwrap();
    match args.command {
        Some(Command::Run { json, ndjson, message }) => {
            assert!(!json);
            assert!(ndjson);
            assert_eq!(message, "hello");
        }
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn version_subcommand_parses() {
    let args = Args::try_parse_from(["horon-agent", "version", "--json"]).unwrap();
    match args.command {
        Some(Command::Version { json }) => assert!(json),
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn usage_subcommand_parses() {
    let args = Args::try_parse_from(["horon-agent", "usage", "--json"]).unwrap();
    match args.command {
        Some(Command::Usage { json }) => assert!(json),
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn provider_list_subcommand_parses() {
    let args = Args::try_parse_from(["horon-agent", "provider", "list", "--json"]).unwrap();
    match args.command {
        Some(Command::Provider(ProviderCommand::List { json })) => assert!(json),
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn provider_current_subcommand_parses() {
    let args = Args::try_parse_from(["horon-agent", "provider", "current", "--json"]).unwrap();
    match args.command {
        Some(Command::Provider(ProviderCommand::Current { json })) => assert!(json),
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn provider_add_subcommand_parses_agent_friendly_flags() {
    let args = Args::try_parse_from([
        "horon-agent",
        "provider",
        "add",
        "my-api",
        "--base-url",
        "https://llm.example.com/v1",
        "--model",
        "model-a",
        "--context-window",
        "128000",
        "--api-key-stdin",
        "--auth",
        "bearer",
        "--set-default",
        "--json",
    ])
    .unwrap();

    match args.command {
        Some(Command::Provider(ProviderCommand::Add {
            name,
            base_url,
            model,
            context_window,
            api_key_stdin,
            auth,
            set_default,
            json,
            ..
        })) => {
            assert_eq!(name, "my-api");
            assert_eq!(base_url, "https://llm.example.com/v1");
            assert_eq!(model, "model-a");
            assert_eq!(context_window, Some(128000));
            assert!(api_key_stdin);
            assert_eq!(auth, Some(ProviderAuthArg::Bearer));
            assert!(set_default);
            assert!(json);
        }
        other => panic!("unexpected command: {:?}", other),
    }
}

/// The bridge has two sockets and the global `--socket` already owns one of
/// them. When the subcommand flag was also `--socket`, clap bound the global
/// and the bridge listened on, and dialled, the same path: it came up looking
/// healthy and every SDK client got ECONNREFUSED. Keep the names distinct.
#[cfg(unix)]
#[test]
fn api_bridge_socket_flags_do_not_collide() {
    let args = Args::try_parse_from([
        "horon-agent",
        "--socket",
        "/tmp/daemon.sock",
        "api-bridge",
        "--api-socket",
        "/tmp/api.sock",
    ])
    .expect("api-bridge should accept both socket flags");
    assert_eq!(args.socket.as_deref(), Some("/tmp/daemon.sock"));
    assert!(matches!(
        args.command,
        Some(Command::ApiBridge { api_socket: Some(ref path), stdio: false }) if path == "/tmp/api.sock"
    ));

    // The bare form must resolve both paths from the environment.
    let bare = Args::try_parse_from(["horon-agent", "api-bridge"]).expect("bare api-bridge should parse");
    assert!(matches!(
        bare.command,
        Some(Command::ApiBridge {
            api_socket: None,
            stdio: false
        })
    ));

    // `--socket` after the subcommand must not be silently accepted as the
    // API socket, which is the exact confusion this test exists to prevent.
    let ambiguous = Args::try_parse_from(["horon-agent", "api-bridge", "--socket", "/tmp/x.sock"]).ok();
    assert!(
        matches!(
            ambiguous.map(|args| (args.socket, args.command)),
            None | Some((
                Some(_),
                Some(Command::ApiBridge {
                    api_socket: None,
                    stdio: false
                })
            ))
        ),
        "`--socket` after api-bridge must bind the daemon socket, never the API socket"
    );
}

#[cfg(unix)]
#[test]
fn api_stdio_accepts_alias_and_daemon_socket_but_not_api_socket() {
    for command in ["api", "api-bridge"] {
        let args = Args::try_parse_from([
            "horon-agent",
            "--no-update",
            "--socket",
            "/isolated/daemon.sock",
            command,
            "--stdio",
        ])
        .expect("stdio bridge must accept the API alias and daemon override");
        assert_eq!(args.socket.as_deref(), Some("/isolated/daemon.sock"));
        assert!(args.no_update);
        assert!(matches!(
            args.command,
            Some(Command::ApiBridge {
                api_socket: None,
                stdio: true
            })
        ));
        assert!(
            Args::try_parse_from(["horon-agent", command, "--stdio", "--api-socket", "/isolated/api.sock",]).is_err(),
            "stdio must not silently ignore an API socket override"
        );
    }
}
