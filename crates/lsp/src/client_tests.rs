use super::*;

#[test]
fn paths_and_uris_round_trip() {
    let dir = tempfile::tempdir().expect("temp dir");
    let file = dir.path().join("a b.rs");
    std::fs::write(&file, "fn main() {}\n").expect("write file");
    let canonical = std::fs::canonicalize(&file).expect("canonical file");

    let uri = path_to_uri(&canonical);
    assert!(uri.starts_with("file:///"), "{uri}");
    assert!(uri.contains("a%20b.rs"), "a space must be percent-encoded: {uri}");
    assert_eq!(uri_to_path(&uri).as_deref(), Some(canonical.as_path()));
}

#[test]
fn non_file_uris_do_not_resolve_to_paths() {
    assert!(uri_to_path("untitled:Untitled-1").is_none());
    assert!(uri_to_path("not a uri").is_none());
}

#[test]
fn positions_are_one_based_in_public_arguments() {
    let params = position_params(Path::new("/tmp/a.rs"), 4, 7);
    assert_eq!(params["position"]["line"], 3);
    assert_eq!(params["position"]["character"], 6);
}

#[test]
fn diagnostic_severities_map_to_words() {
    use lsp_types::DiagnosticSeverity as LspSeverity;
    assert_eq!(severity_of(Some(LspSeverity::ERROR)), DiagnosticSeverity::Error);
    assert_eq!(severity_of(Some(LspSeverity::WARNING)), DiagnosticSeverity::Warning);
    assert_eq!(
        severity_of(Some(LspSeverity::INFORMATION)),
        DiagnosticSeverity::Information
    );
    assert_eq!(severity_of(Some(LspSeverity::HINT)), DiagnosticSeverity::Hint);
    assert_eq!(
        severity_of(None),
        DiagnosticSeverity::Error,
        "a diagnostic with no severity is reported as an error"
    );
}

#[test]
fn symbol_kinds_render_as_lowercase_words() {
    use lsp_types::SymbolKind;
    assert_eq!(symbol_kind_name(SymbolKind::FUNCTION), "function");
    assert_eq!(symbol_kind_name(SymbolKind::STRUCT), "struct");
    assert_eq!(symbol_kind_name(SymbolKind::TYPE_PARAMETER), "type parameter");
    assert_eq!(symbol_kind_name(SymbolKind::ENUM_MEMBER), "enum member");
}

#[test]
fn hover_markup_and_code_blocks_are_rendered() {
    let markup = lsp_types::HoverContents::Markup(lsp_types::MarkupContent {
        kind: lsp_types::MarkupKind::Markdown,
        value: "**fn** `parse`".to_string(),
    });
    assert_eq!(hover_text(markup), "**fn** `parse`");

    let code = lsp_types::HoverContents::Scalar(lsp_types::MarkedString::LanguageString(lsp_types::LanguageString {
        language: "rust".to_string(),
        value: "fn parse() -> u32".to_string(),
    }));
    assert_eq!(hover_text(code), "```rust\nfn parse() -> u32\n```");
}

#[test]
fn language_ids_follow_the_file_extension() {
    assert_eq!(language_id(Path::new("a.rs")), "rust");
    assert_eq!(language_id(Path::new("a.tsx")), "typescriptreact");
    assert_eq!(language_id(Path::new("a.mjs")), "javascript");
    assert_eq!(language_id(Path::new("a.go")), "go");
    assert_eq!(language_id(Path::new("a.py")), "python");
    assert_eq!(language_id(Path::new("a.txt")), "plaintext");
}

#[test]
fn workspace_configuration_returns_defaults_per_item() {
    let params = serde_json::json!({ "items": [{ "section": "rust-analyzer" }, { "section": "other" }] });
    let rust = server_request_result(
        "workspace/configuration",
        Some(&params),
        "rust-analyzer",
        Path::new("/w"),
    );
    assert_eq!(rust[0]["checkOnSave"], false);
    assert_eq!(rust[1], serde_json::json!({}));

    let gopls = server_request_result("workspace/configuration", Some(&params), "gopls", Path::new("/w"));
    assert_eq!(gopls[0], serde_json::json!({}));
}

#[test]
fn apply_edit_is_never_applied() {
    let result = server_request_result("workspace/applyEdit", None, "rust-analyzer", Path::new("/w"));
    assert_eq!(result, serde_json::json!({ "applied": false }));
}
