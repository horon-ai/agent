use super::*;
use crate::tool::{ToolContext, ToolExecutionMode};
use serde_json::json;

fn make_ctx(working_dir: PathBuf) -> ToolContext {
    ToolContext {
        session_id: "lsp-test-session".to_string(),
        message_id: "lsp-test-message".to_string(),
        tool_call_id: "lsp-test-call".to_string(),
        working_dir: Some(working_dir),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

fn fixture(contents: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("main.rs");
    std::fs::write(&path, contents).expect("write fixture");
    (dir, path)
}

#[test]
fn the_tool_is_named_lsp_and_declares_every_action() {
    let tool = LspTool::new();
    assert_eq!(tool.name(), "lsp");
    assert!(tool.effects().reads());
    assert!(tool.effects().processes(), "it starts a language server");
    assert!(!tool.effects().writes());

    let schema = tool.parameters_schema();
    let actions = schema["properties"]["action"]["enum"].as_array().expect("action enum");
    for action in [
        "diagnostics",
        "definition",
        "references",
        "hover",
        "symbols",
        "status",
        "rename",
        "code_actions",
    ] {
        assert!(
            actions.iter().any(|value| value == action),
            "the schema must advertise {action}"
        );
    }
    assert_eq!(schema["required"], json!(["action"]));
}

#[test]
fn a_symbol_on_a_line_resolves_to_its_column() {
    let (_dir, path) = fixture("fn main() {\n    let value = helper(1);\n}\n");
    let (line, column) = resolve_position(&path, Some(2), None, Some("helper")).expect("position");

    assert_eq!(line, 2);
    assert_eq!(column, 17, "`    let value = ` is sixteen characters");
}

#[test]
fn columns_are_counted_in_utf16_units() {
    let (_dir, path) = fixture("fn main() {}\n// 🦀 helper()\n");
    let (_, column) = resolve_position(&path, Some(2), None, Some("helper")).expect("position");

    assert_eq!(column, 7, "the crab is one character but two UTF-16 units");
}

#[test]
fn a_missing_symbol_names_the_symbol_and_line() {
    let (_dir, path) = fixture("fn main() {}\nlet x = 1;\n");
    let error = resolve_position(&path, Some(2), None, Some("nope")).expect_err("no such symbol");

    assert_eq!(error.to_string(), "Symbol 'nope' not found on line 2");
}

#[test]
fn positions_are_one_based() {
    let (_dir, path) = fixture("fn main() {}\n");

    let error = resolve_position(&path, Some(0), Some(1), None).expect_err("line 0");
    assert_eq!(error.to_string(), "line is 1-based; line 0 does not exist.");

    let error = resolve_position(&path, Some(1), Some(0), None).expect_err("character 0");
    assert_eq!(error.to_string(), "character is 1-based; character 0 does not exist.");

    assert_eq!(
        resolve_position(&path, Some(1), Some(4), None).expect("explicit position"),
        (1, 4)
    );
}

#[test]
fn a_position_without_a_symbol_or_character_is_refused() {
    let (_dir, path) = fixture("fn main() {}\n");
    let error = resolve_position(&path, Some(1), None, None).expect_err("no position");

    assert_eq!(
        error.to_string(),
        "Provide `character` or `symbol` to locate a position on line 1."
    );
}

#[test]
fn timeouts_default_and_clamp() {
    assert_eq!(effective_timeout(None), Duration::from_secs(20));
    assert_eq!(effective_timeout(Some(1.0)), Duration::from_secs(5));
    assert_eq!(effective_timeout(Some(9999.0)), Duration::from_secs(300));
    assert_eq!(effective_timeout(Some(45.0)), Duration::from_secs(45));
}

#[test]
fn document_symbols_render_as_an_indented_outline() {
    let symbols = vec![horon_agent_lsp::Symbol {
        name: "main".to_string(),
        kind: "function",
        path: None,
        line: 3,
        column: 4,
        children: vec![horon_agent_lsp::Symbol {
            name: "value".to_string(),
            kind: "variable",
            path: None,
            line: 4,
            column: 9,
            children: Vec::new(),
        }],
    }];
    let mut output = String::new();
    render_symbols(&symbols, 0, &mut output);

    assert_eq!(output, "3: main (function)\n  4: value (variable)\n");
}

#[test]
fn paths_under_the_working_directory_print_relative() {
    let base = Path::new("/work/project");
    assert_eq!(display_path(&base.join("src").join("main.rs"), base), "src/main.rs");
    assert_eq!(display_path(Path::new("/elsewhere/x.rs"), base), "/elsewhere/x.rs");
}

#[test]
fn diagnostic_messages_collapse_to_one_line() {
    assert_eq!(one_line("expected  u32,\n   found &str"), "expected u32, found &str");
}

#[tokio::test]
async fn a_missing_file_is_refused_before_a_server_starts() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ctx = make_ctx(dir.path().to_path_buf());

    let error = LspTool::new()
        .execute(json!({ "action": "diagnostics", "file": "nope.rs" }), ctx)
        .await
        .expect_err("missing file");
    assert_eq!(error.to_string(), "File not found: nope.rs");
}

#[tokio::test]
async fn a_file_without_a_configured_server_is_refused() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("notes.md"), "# notes\n").expect("write notes");
    let ctx = make_ctx(dir.path().to_path_buf());

    let error = LspTool::new()
        .execute(json!({ "action": "diagnostics", "file": "notes.md" }), ctx)
        .await
        .expect_err("no server");
    let message = error.to_string();
    assert!(
        message.starts_with("No language server is configured for notes.md;"),
        "{message}"
    );
}

#[tokio::test]
async fn status_needs_no_file_and_never_starts_a_server() {
    let dir = tempfile::tempdir().expect("temp dir");
    let ctx = make_ctx(dir.path().to_path_buf());

    let output = LspTool::new()
        .execute(json!({ "action": "status" }), ctx)
        .await
        .expect("status")
        .output;
    assert!(output.contains("language server"), "{output}");
}

#[tokio::test]
async fn a_missing_binary_reports_the_install_hint() {
    // `pyright` is the only server this test can count on being absent at least
    // somewhere; when it is installed the hint is unreachable and the call would
    // start a real server, so the case is skipped instead of becoming flaky.
    let spec = horon_agent_lsp::server_for_name("pyright").expect("pyright spec");
    if horon_agent_lsp::is_installed(spec) {
        return;
    }

    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("tool.py"), "print('hi')\n").expect("write script");
    let ctx = make_ctx(dir.path().to_path_buf());

    let error = LspTool::new()
        .execute(json!({ "action": "diagnostics", "file": "tool.py" }), ctx)
        .await
        .expect_err("missing binary");
    assert_eq!(
        error.to_string(),
        "pyright-langserver is not installed or not on PATH; install it or use bash to run the compiler."
    );
}

/// These two cases start rust-analyzer on a temporary crate and therefore only
/// run when asked for explicitly (`cargo test -p horon-agent-app-core lsp --
/// --ignored`). They also return early when rust-analyzer is not installed.
fn rust_analyzer_available() -> bool {
    horon_agent_lsp::server_for_name("rust-analyzer").is_some_and(horon_agent_lsp::is_installed)
}

fn rust_fixture(source: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(dir.path().join("src")).expect("create src");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"lsp-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write manifest");
    std::fs::write(dir.path().join("src").join("main.rs"), source).expect("write source");
    dir
}

#[tokio::test]
#[ignore = "requires rust-analyzer on PATH; run with --ignored"]
async fn lsp_diagnostics_report_a_compile_error() {
    if !rust_analyzer_available() {
        eprintln!("rust-analyzer is not installed; skipping");
        return;
    }
    let dir = rust_fixture(
        "use missing_dependency::Thing;\n\nfn main() {\n    let _: u32 = \"text\";\n    let broken = ;\n}\n",
    );
    let ctx = make_ctx(dir.path().to_path_buf());

    let output = LspTool::new()
        .execute(
            json!({ "action": "diagnostics", "file": "src/main.rs", "timeout_secs": 120 }),
            ctx,
        )
        .await
        .expect("diagnostics")
        .output;
    assert!(output.contains("error"), "diagnostics must report the error: {output}");
    assert!(
        output.starts_with("src/main.rs:"),
        "each diagnostic is prefixed with the file path: {output}"
    );
}

#[tokio::test]
#[ignore = "requires rust-analyzer on PATH; run with --ignored"]
async fn lsp_definition_resolves_a_function_call() {
    if !rust_analyzer_available() {
        eprintln!("rust-analyzer is not installed; skipping");
        return;
    }
    let dir = rust_fixture("fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n\nfn main() {\n    let _ = add(1, 2);\n}\n");
    let ctx = make_ctx(dir.path().to_path_buf());

    // Wait for the workspace to finish loading before asking for a definition:
    // the diagnostics call returns once rust-analyzer has analysed the crate.
    LspTool::new()
        .execute(
            json!({ "action": "diagnostics", "file": "src/main.rs", "timeout_secs": 120 }),
            make_ctx(dir.path().to_path_buf()),
        )
        .await
        .expect("diagnostics");

    let output = LspTool::new()
        .execute(
            json!({
                "action": "definition",
                "file": "src/main.rs",
                "line": 6,
                "symbol": "add",
                "timeout_secs": 120
            }),
            ctx,
        )
        .await
        .expect("definition")
        .output;
    assert!(
        output.contains("src/main.rs:1:4:"),
        "the definition of `add` is on line 1: {output}"
    );
    assert!(output.contains("fn add"), "{output}");
}

#[test]
fn only_applying_actions_declare_a_write_effect() {
    let tool = LspTool::new();
    assert!(
        !tool
            .effects_for_input(&json!({"action": "rename", "apply": false}))
            .writes(),
        "a rename dry run writes nothing"
    );
    assert!(
        tool.effects_for_input(&json!({"action": "rename", "apply": true}))
            .writes()
    );
    assert!(
        !tool.effects_for_input(&json!({"action": "code_actions"})).writes(),
        "listing code actions writes nothing"
    );
    assert!(
        tool.effects_for_input(&json!({"action": "code_actions", "action_id": "1"}))
            .writes()
    );
    assert!(!tool.effects_for_input(&json!({"action": "references"})).writes());
}

#[tokio::test]
async fn rename_without_a_new_name_is_refused_before_the_server_runs() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("main.rs");
    std::fs::write(&path, "fn alpha() {}\n").expect("fixture");
    let error = LspTool::new()
        .execute(
            json!({"action": "rename", "file": "main.rs", "line": 1, "character": 4}),
            make_ctx(dir.path().to_path_buf()),
        )
        .await
        .expect_err("a rename needs a new name");
    assert!(format!("{error:#}").contains("new_name"), "{error:#}");
}
