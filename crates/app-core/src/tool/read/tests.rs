use super::*;
use crate::tool::{ToolContext, ToolExecutionMode};
use serde_json::json;

fn make_ctx(working_dir: std::path::PathBuf) -> ToolContext {
    ToolContext {
        session_id: "test-session".to_string(),
        message_id: "test-message".to_string(),
        tool_call_id: "test-call".to_string(),
        working_dir: Some(working_dir),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

/// Sets an environment variable for the rest of the test and restores whatever
/// was there before on drop. A panic between the set and the restore would
/// otherwise leave the variable set for every later test in the same binary.
struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        crate::env::set_var(key, value);
        crate::config::invalidate_config_cache();
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var(self.key, value),
            None => crate::env::remove_var(self.key),
        }
        crate::config::invalidate_config_cache();
    }
}

#[test]
fn normalize_read_range_supports_start_and_end_lines() {
    let params: ReadInput = serde_json::from_value(json!({
        "file_path": "src/lib.rs",
        "start_line": 10,
        "end_line": 20
    }))
    .expect("deserialize params");

    let range = normalize_read_range(&params).expect("normalize range");
    assert_eq!(
        range,
        NormalizedReadRange {
            offset: 9,
            limit: 11,
            style: ReadRangeStyle::StartEnd,
        }
    );
}

#[test]
fn normalize_read_range_supports_start_line_and_limit() {
    let params: ReadInput = serde_json::from_value(json!({
        "file_path": "src/lib.rs",
        "start_line": 10,
        "limit": 20
    }))
    .expect("deserialize params");

    let range = normalize_read_range(&params).expect("start_line + limit should work");
    assert_eq!(
        range,
        NormalizedReadRange {
            offset: 9,
            limit: 20,
            style: ReadRangeStyle::StartEnd,
        }
    );
}

#[test]
fn normalize_read_range_prefers_end_line_over_limit() {
    let params: ReadInput = serde_json::from_value(json!({
        "file_path": "src/lib.rs",
        "start_line": 10,
        "end_line": 20,
        "limit": 999
    }))
    .expect("deserialize params");

    let range = normalize_read_range(&params).expect("end_line should take precedence");
    assert_eq!(
        range,
        NormalizedReadRange {
            offset: 9,
            limit: 11,
            style: ReadRangeStyle::StartEnd,
        }
    );
}

#[test]
fn normalize_read_range_rejects_start_line_and_offset() {
    let params: ReadInput = serde_json::from_value(json!({
        "file_path": "src/lib.rs",
        "start_line": 10,
        "offset": 20
    }))
    .expect("deserialize params");

    let err = normalize_read_range(&params).expect_err("mixed range styles should fail");
    assert!(
        err.to_string().contains("Use either start_line/end_line") || err.to_string().contains("not both"),
        "unexpected error: {err}"
    );
}

#[test]
fn normalize_read_range_accepts_matching_start_line_and_offset() {
    let params: ReadInput = serde_json::from_value(json!({
        "file_path": "src/lib.rs",
        "start_line": 10,
        "offset": 9,
        "limit": 20
    }))
    .expect("deserialize params");

    let range = normalize_read_range(&params).expect("matching range styles should work");
    assert_eq!(
        range,
        NormalizedReadRange {
            offset: 9,
            limit: 20,
            style: ReadRangeStyle::StartEnd,
        }
    );
}

#[test]
fn normalize_read_range_accepts_end_line_with_zero_offset() {
    let params: ReadInput = serde_json::from_value(json!({
        "file_path": "src/lib.rs",
        "end_line": 20,
        "offset": 0
    }))
    .expect("deserialize params");

    let range = normalize_read_range(&params).expect("redundant zero offset should work");
    assert_eq!(
        range,
        NormalizedReadRange {
            offset: 0,
            limit: 20,
            style: ReadRangeStyle::StartEnd,
        }
    );
}

#[test]
fn normalize_read_range_rejects_invalid_end_before_start() {
    let params: ReadInput = serde_json::from_value(json!({
        "file_path": "src/lib.rs",
        "start_line": 20,
        "end_line": 10
    }))
    .expect("deserialize params");

    let err = normalize_read_range(&params).expect_err("invalid range should fail");
    assert!(
        err.to_string().contains("greater than or equal to start_line"),
        "unexpected error: {err}"
    );
}

#[test]
fn read_tool_schema_avoids_openai_incompatible_combinators() {
    let schema = ReadTool::new().parameters_schema();

    assert_eq!(schema.get("type"), Some(&json!("object")));
    assert!(schema.get("allOf").is_none());
    assert!(schema.get("not").is_none());
}

#[test]
fn read_tool_schema_advertises_only_canonical_public_fields() {
    let schema = ReadTool::new().parameters_schema();
    let properties = schema["properties"]
        .as_object()
        .expect("read schema properties should be an object");

    assert!(properties.contains_key("file_path"));
    assert!(properties.contains_key("start_line"));
    assert!(properties.contains_key("limit"));
    assert!(!properties.contains_key("end_line"));
    assert!(!properties.contains_key("offset"));
}

#[test]
fn read_tool_description_advertises_supported_file_types() {
    let tool = ReadTool::new();
    let description = tool.description().to_lowercase();
    assert!(description.contains("text"), "description={description}");
    assert!(description.contains("image"), "description={description}");
    assert!(description.contains("pdf"), "description={description}");

    let schema = tool.parameters_schema();
    let file_path_description = schema["properties"]["file_path"]["description"]
        .as_str()
        .expect("file_path should have a description");
    assert!(file_path_description.contains("artifact://"), "{file_path_description}");
    assert!(
        file_path_description.contains("line selector") && file_path_description.contains(":10-40"),
        "file_path should advertise the selector grammar: {file_path_description}"
    );
}

#[tokio::test]
async fn read_tool_supports_start_line_and_end_line() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("sample.txt");
    std::fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").expect("write sample file");

    let tool = ReadTool::new();
    let output = tool
        .execute(
            json!({
                "file_path": "sample.txt",
                "start_line": 2,
                "end_line": 4
            }),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect("read execution should succeed");

    assert!(output.output.contains("2\ttwo"), "output={:?}", output.output);
    assert!(output.output.contains("3\tthree"), "output={:?}", output.output);
    assert!(output.output.contains("4\tfour"), "output={:?}", output.output);
    assert!(!output.output.contains("1\tone"), "output={:?}", output.output);
    assert!(!output.output.contains("5\tfive"), "output={:?}", output.output);
}

#[tokio::test]
async fn read_tool_continuation_hint_matches_start_line_style() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("sample.txt");
    std::fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").expect("write sample file");

    let tool = ReadTool::new();
    let output = tool
        .execute(
            json!({
                "file_path": "sample.txt",
                "start_line": 2,
                "end_line": 3
            }),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect("read execution should succeed");

    assert!(
        output.output.contains("use start_line=4 to continue"),
        "output={:?}",
        output.output
    );
}

#[tokio::test]
async fn read_tool_supports_start_line_with_limit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("sample.txt");
    std::fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").expect("write sample file");

    let tool = ReadTool::new();
    let output = tool
        .execute(
            json!({
                "file_path": "sample.txt",
                "start_line": 2,
                "limit": 2
            }),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect("read execution should succeed");

    assert!(output.output.contains("2\ttwo"), "output={:?}", output.output);
    assert!(output.output.contains("3\tthree"), "output={:?}", output.output);
    assert!(!output.output.contains("4\tfour"), "output={:?}", output.output);
    assert!(
        output.output.contains("use start_line=4 to continue"),
        "output={:?}",
        output.output
    );
}

#[tokio::test]
async fn read_tool_prefers_end_line_over_limit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("sample.txt");
    std::fs::write(&path, "one\ntwo\nthree\nfour\nfive\n").expect("write sample file");

    let tool = ReadTool::new();
    let output = tool
        .execute(
            json!({
                "file_path": "sample.txt",
                "start_line": 2,
                "end_line": 3,
                "limit": 50
            }),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect("read execution should succeed");

    assert!(output.output.contains("2\ttwo"), "output={:?}", output.output);
    assert!(output.output.contains("3\tthree"), "output={:?}", output.output);
    assert!(!output.output.contains("4\tfour"), "output={:?}", output.output);
    assert!(
        output.output.contains("use start_line=4 to continue"),
        "output={:?}",
        output.output
    );
}

fn tiny_bmp_bytes() -> Vec<u8> {
    // 2x2 24-bit BMP, rows padded to 4 bytes.
    let row: [u8; 8] = [0, 0, 255, 0, 255, 0, 0, 0];
    let pixels: Vec<u8> = row.iter().chain(row.iter()).copied().collect();
    let mut out = Vec::new();
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + pixels.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&2i32.to_le_bytes());
    out.extend_from_slice(&2i32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(pixels.len() as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&pixels);
    out
}

/// #1712: reading a BMP must attach a PNG, never `image/bmp`, which every
/// provider rejects and which used to brick the session.
#[tokio::test]
async fn read_tool_converts_bmp_to_png_image_block() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("pic.bmp"), tiny_bmp_bytes()).expect("write bmp");

    let output = ReadTool::new()
        .execute(json!({"file_path": "pic.bmp"}), make_ctx(temp.path().to_path_buf()))
        .await
        .expect("read execution should succeed");

    assert_eq!(output.images.len(), 1, "output={:?}", output.output);
    assert_eq!(output.images[0].media_type, "image/png");
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &output.images[0].data)
        .expect("valid base64");
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(output.output.contains("Converted from BMP"), "{}", output.output);
}

/// Garbage behind an image extension produces text only, with a reason.
#[tokio::test]
async fn read_tool_refuses_undecodable_image_without_image_block() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("fake.png"), b"not really a png").expect("write");

    let output = ReadTool::new()
        .execute(json!({"file_path": "fake.png"}), make_ctx(temp.path().to_path_buf()))
        .await
        .expect("read execution should succeed");

    assert!(output.images.is_empty());
    assert!(output.output.contains("Not sent to the model"), "{}", output.output);
}

async fn read_fixture(contents: &str, extra: serde_json::Value) -> String {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("sample.txt"), contents).expect("write sample file");
    let mut input = json!({"file_path": "sample.txt"});
    for (key, value) in extra.as_object().expect("object extras") {
        input[key] = value.clone();
    }
    ReadTool::new()
        .execute(input, make_ctx(temp.path().to_path_buf()))
        .await
        .expect("read execution should succeed")
        .output
}

/// Tags below are the first two bytes of each line's SHA-256, computed outside
/// the crate (`shasum -a 256`), so the wiring is checked against the codec and
/// not against itself.
#[tokio::test]
async fn read_hashline_format() {
    let output = read_fixture("alpha\nbeta\ngamma\n", json!({"hashline": true})).await;

    assert_eq!(
        output, "    1#8ed3:alpha\n    2#f44e:beta\n    3#be9d:gamma\n",
        "hashline output={output:?}"
    );
}

#[tokio::test]
async fn read_default_output_is_byte_identical_to_legacy() {
    let expected = "    1\talpha\n    2\tbeta\n    3\tgamma\n";

    assert_eq!(read_fixture("alpha\nbeta\ngamma\n", json!({})).await, expected);
    assert_eq!(
        read_fixture("alpha\nbeta\ngamma\n", json!({"hashline": false})).await,
        expected
    );
}

/// With `[tools] nested_agents_md` off, an ancestor `AGENTS.md` must not reach
/// the result: the output stays the plain numbered listing.
#[tokio::test]
async fn read_leaves_output_plain_when_nested_agents_md_is_off() {
    let _guard = crate::storage::lock_test_env();
    let _env = EnvGuard::set("HORON_AGENT_NESTED_AGENTS_MD", "0");

    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(temp.path().join("sub")).expect("create subdir");
    std::fs::write(temp.path().join("sub/AGENTS.md"), "nested rules").expect("write AGENTS.md");
    std::fs::write(temp.path().join("sub/sample.txt"), "alpha\nbeta\n").expect("write sample file");

    let output = ReadTool::new()
        .execute(
            json!({"file_path": "sub/sample.txt"}),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect("read execution should succeed")
        .output;

    assert!(!output.contains("[Directory Context:"), "output={output:?}");
    assert_eq!(output, "    1\talpha\n    2\tbeta\n", "output={output:?}");
}

/// The flag is on by default, so the same read in a tree that holds no
/// `AGENTS.md` at any level must still come back as the plain listing: nothing
/// to inject is not the same as injecting something extra.
#[tokio::test]
async fn read_leaves_output_alone_without_agents_md() {
    let _guard = crate::storage::lock_test_env();
    let _env = EnvGuard::set("HORON_AGENT_NESTED_AGENTS_MD", "1");
    crate::tool::nested_agents::clear_session_cache_for_test();
    assert!(
        crate::config::config().tools.nested_agents_md,
        "this case only means something with injection enabled"
    );

    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(temp.path().join("sub/deeper")).expect("create subdir");
    std::fs::write(temp.path().join("sub/deeper/sample.txt"), "alpha\nbeta\n").expect("write sample file");

    // Prove the fixture: walk every level the injection would visit, from the
    // file's directory up to but excluding the working directory, and fail if
    // any of them holds an `AGENTS.md`. The count guards against a vacuous loop.
    let mut levels = 0;
    let mut current = temp.path().join("sub/deeper");
    while current != temp.path() {
        assert!(
            !current.join("AGENTS.md").exists(),
            "fixture must hold no AGENTS.md at {}",
            current.display()
        );
        levels += 1;
        current = current.parent().expect("ancestor of fixture dir").to_path_buf();
    }
    assert_eq!(levels, 2, "the fixture must span two levels below the working dir");

    let output = ReadTool::new()
        .execute(
            json!({"file_path": "sub/deeper/sample.txt"}),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect("read execution should succeed")
        .output;

    crate::tool::nested_agents::clear_session_cache_for_test();

    println!("--- manual QA: read output with no ancestor AGENTS.md anywhere ---\n{output}\n--- end ---");
    assert!(!output.contains("[Directory Context:"), "output={output:?}");
    assert_eq!(output, "    1\talpha\n    2\tbeta\n", "output={output:?}");
}

/// A file inside a directory that holds an `AGENTS.md` gets that file's text
/// appended, so a session sees the rules of the directory it reads from.
#[tokio::test]
async fn read_appends_ancestor_agents_md() {
    let _guard = crate::storage::lock_test_env();
    let _env = EnvGuard::set("HORON_AGENT_NESTED_AGENTS_MD", "1");
    crate::tool::nested_agents::clear_session_cache_for_test();

    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(temp.path().join("sub")).expect("create subdir");
    std::fs::write(
        temp.path().join("sub/AGENTS.md"),
        "nested rules marker: ANCESTOR-MARKER-9f3c\n",
    )
    .expect("write AGENTS.md");
    std::fs::write(temp.path().join("sub/sample.txt"), "alpha\nbeta\n").expect("write sample file");

    let output = ReadTool::new()
        .execute(
            json!({"file_path": "sub/sample.txt"}),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect("read execution should succeed")
        .output;

    crate::tool::nested_agents::clear_session_cache_for_test();

    println!("--- manual QA: read output with ancestor AGENTS.md ---\n{output}\n--- end ---");
    assert!(output.contains("[Directory Context:"), "output={output:?}");
    assert!(output.contains("ANCESTOR-MARKER-9f3c"), "output={output:?}");
    assert!(output.starts_with("    1\talpha\n    2\tbeta\n"), "output={output:?}");
}

/// A ~300 KB listing is past the injection ceiling, so it comes back whole and
/// unadorned: appending there would only push the result into the overflow guard.
#[tokio::test]
async fn read_large_file_with_ancestor_agents_md_is_not_withheld() {
    let _guard = crate::storage::lock_test_env();
    let _env = EnvGuard::set("HORON_AGENT_NESTED_AGENTS_MD", "1");
    crate::tool::nested_agents::clear_session_cache_for_test();

    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(temp.path().join("a/b")).expect("create tree");
    std::fs::write(temp.path().join("a/AGENTS.md"), "outer rules\n").expect("write outer AGENTS.md");
    std::fs::write(temp.path().join("a/b/AGENTS.md"), "inner rules\n").expect("write inner AGENTS.md");

    let line = "y".repeat(1000);
    let body = (1..=300).map(|_| line.as_str()).collect::<Vec<_>>().join("\n");
    std::fs::write(temp.path().join("a/b/large.txt"), format!("{body}\n")).expect("write large file");
    assert!(body.len() > 300_000, "fixture must exceed the injection ceiling");

    let output = ReadTool::new()
        .execute(
            json!({"file_path": "a/b/large.txt"}),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect("read execution should succeed")
        .output;

    crate::tool::nested_agents::clear_session_cache_for_test();

    println!(
        "--- manual QA: large read bytes={} withheld={} injected={} ---",
        output.len(),
        output.contains("OUTPUT WITHHELD"),
        output.contains("[Directory Context:")
    );
    assert!(!output.contains("OUTPUT WITHHELD"), "output was withheld: {output:?}");
    assert!(
        !output.contains("[Directory Context:"),
        "oversized read must not be injected"
    );
    assert!(output.contains(&line), "the file body must survive intact");
}

#[tokio::test]
async fn read_hashline_marks_truncated_lines() {
    let long = "x".repeat(MAX_LINE_LEN + 10);
    let output = read_fixture(&format!("short\n{long}\n"), json!({"hashline": true})).await;
    let lines: Vec<&str> = output.lines().collect();

    assert_eq!(lines.len(), 2, "output={output:?}");
    assert_eq!(lines[0], "    1#f9b0:short", "output={output:?}");
    let expected_truncated = format!("    2#{TRUNCATED_TAG}:{}...", "x".repeat(MAX_LINE_LEN));
    assert_eq!(lines[1], expected_truncated);
    assert!(!lines[1].contains("#f9b0"), "a truncated line must not carry a tag");
}

#[tokio::test]
async fn read_hashline_tags_ignore_crlf_terminators() {
    // `str::lines()` strips the `\r`, so the emitted text and its tag both cover
    // the line without the terminator; a tag over `alpha\r` would be `07be`.
    let output = read_fixture("alpha\r\nbeta\r\n", json!({"hashline": true})).await;

    assert_eq!(output, "    1#8ed3:alpha\n    2#f44e:beta\n", "output={output:?}");
    assert!(!output.contains('\r'), "output={output:?}");
    assert_eq!(
        read_fixture("alpha\r\nbeta\r\n", json!({})).await,
        "    1\talpha\n    2\tbeta\n"
    );
}

#[tokio::test]
async fn read_hashline_handles_missing_trailing_newline() {
    let output = read_fixture("alpha\nbeta", json!({"hashline": true})).await;

    assert_eq!(output, "    1#8ed3:alpha\n    2#f44e:beta\n", "output={output:?}");
}

#[tokio::test]
async fn read_hashline_leaves_empty_file_output_alone() {
    assert_eq!(read_fixture("", json!({"hashline": true})).await, "(empty file)");
    assert_eq!(read_fixture("", json!({})).await, "(empty file)");
}

/// An empty file still carries its marker once the ancestor contract is
/// appended; injection alone would otherwise look like a non-empty body.
#[tokio::test]
async fn read_empty_file_with_ancestor_agents_md_keeps_the_empty_marker() {
    let _guard = crate::storage::lock_test_env();
    let _env = EnvGuard::set("HORON_AGENT_NESTED_AGENTS_MD", "1");
    crate::tool::nested_agents::clear_session_cache_for_test();

    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(temp.path().join("sub")).expect("create subdir");
    std::fs::write(temp.path().join("sub/AGENTS.md"), "nested rules\n").expect("write AGENTS.md");
    std::fs::write(temp.path().join("sub/empty.txt"), "").expect("write empty file");

    let output = ReadTool::new()
        .execute(
            json!({"file_path": "sub/empty.txt"}),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect("read execution should succeed")
        .output;

    crate::tool::nested_agents::clear_session_cache_for_test();

    println!("--- manual QA: empty file with ancestor AGENTS.md ---\n{output}\n--- end ---");
    assert!(output.contains("(empty file)"), "output={output:?}");
    assert!(output.contains("[Directory Context:"), "output={output:?}");
    assert!(output.starts_with("(empty file)"), "output={output:?}");
}

#[tokio::test]
async fn read_hashline_tags_change_with_the_revision() {
    let first = read_fixture("alpha\n", json!({"hashline": true})).await;
    let second = read_fixture("omega\n", json!({"hashline": true})).await;

    assert_eq!(first, "    1#8ed3:alpha\n");
    assert_eq!(second, "    1#304b:omega\n");
    assert_ne!(first, second, "an anchor from one revision must not match another");
    assert!(
        !second.contains("#8ed3:"),
        "the stale anchor must not appear: {second:?}"
    );
}

#[test]
fn read_effects_claim_network_only_for_a_remote_path() {
    let tool = ReadTool::new();

    assert_eq!(tool.effects(), ToolEffects::read());
    assert_eq!(
        tool.effects_for_input(&json!({"file_path": "src/lib.rs"})),
        ToolEffects::read(),
        "a local read must not claim network reach"
    );
    assert_eq!(
        tool.effects_for_input(&json!({"file_path": "agent://child-1/result"})),
        ToolEffects::read().union(ToolEffects::network())
    );
    assert_eq!(
        tool.effects_for_input(&json!({"file_path": "https://example.com/a.txt"})),
        ToolEffects::read().union(ToolEffects::network())
    );
    assert_eq!(
        tool.effects_for_input(&json!({"file_path": "artifact://0"})),
        ToolEffects::read(),
        "a session artifact is local data, not a network read"
    );
    assert_eq!(
        tool.effects_for_input(&json!({"file_path": "history://current"})),
        ToolEffects::read(),
        "session history is local data, not a network read"
    );
    assert!(
        tool.effects_for_input(&json!({"file_path": "https://example.com/a.txt"}))
            .parallel_safe(),
        "a network read may still overlap another call"
    );
}

/// Isolated `HORON_AGENT_HOME` (diff snapshots live there) plus a session id
/// no other test shares, so read memory cannot leak between tests.
struct DedupFixture {
    temp: tempfile::TempDir,
    _home: EnvGuard,
    session_id: String,
    calls: std::cell::Cell<u32>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl DedupFixture {
    fn new(session_id: &str) -> Self {
        let lock = crate::storage::lock_test_env();
        let temp = tempfile::tempdir().expect("tempdir");
        let home = EnvGuard::set("HORON_AGENT_HOME", temp.path().join("home").to_str().unwrap());
        crate::tool::read_state::forget_session(session_id);
        Self {
            temp,
            _home: home,
            session_id: session_id.to_string(),
            calls: std::cell::Cell::new(0),
            _lock: lock,
        }
    }

    fn write(&self, body: &str) {
        std::fs::write(self.temp.path().join("f.txt"), body).expect("write");
    }

    async fn read(&self, extra: serde_json::Value) -> String {
        self.calls.set(self.calls.get() + 1);
        let mut input = json!({"file_path": "f.txt"});
        input
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let mut ctx = make_ctx(self.temp.path().to_path_buf());
        ctx.session_id = self.session_id.clone();
        ctx.tool_call_id = format!("call-{}", self.calls.get());
        ReadTool::new().execute(input, ctx).await.expect("read").output
    }
}

impl Drop for DedupFixture {
    fn drop(&mut self) {
        crate::tool::read_state::forget_session(&self.session_id);
    }
}

fn numbered(lines: usize) -> String {
    (1..=lines).map(|i| format!("line {i}\n")).collect()
}

#[tokio::test]
async fn repeated_read_of_unchanged_lines_returns_a_short_notice() {
    let fx = DedupFixture::new("dedup-unchanged");
    fx.write(&numbered(200));
    let first = fx.read(json!({})).await;
    assert!(first.contains("  200\tline 200"));

    let second = fx.read(json!({})).await;
    assert!(
        second.starts_with("[read: f.txt lines 1-200 are unchanged since your earlier read (tool call call-1)"),
        "{second}"
    );
    assert!(second.len() < 512, "{} bytes", second.len());

    // A sub-range of what was shown is covered too; one never shown is not.
    let sub = fx.read(json!({"start_line": 10, "end_line": 20})).await;
    assert!(sub.starts_with("[read: f.txt lines 10-20 are unchanged"), "{sub}");
    let hashline = fx.read(json!({"hashline": true})).await;
    assert!(hashline.contains("#"), "a different format renders again: {hashline}");
}

#[tokio::test]
async fn whole_file_reread_after_an_edit_returns_a_diff() {
    let fx = DedupFixture::new("dedup-diff");
    fx.write(&numbered(200));
    fx.read(json!({})).await;

    fx.write(&numbered(200).replace("line 100\n", "line one hundred\n"));
    let changed = fx.read(json!({})).await;
    assert!(
        changed.starts_with("[read: f.txt changed since your earlier read (tool call call-1)"),
        "{changed}"
    );
    assert!(changed.contains("@@"), "{changed}");
    assert!(changed.contains("-line 100\n+line one hundred\n"), "{changed}");
    assert!(
        !changed.contains("line 10\n"),
        "only context lines around the change: {changed}"
    );

    // The diff is now the baseline: an identical re-read is unchanged.
    let again = fx.read(json!({})).await;
    assert!(again.starts_with("[read: f.txt lines 1-200 are unchanged"), "{again}");
}

#[tokio::test]
async fn full_true_and_forgotten_sessions_print_the_file() {
    let fx = DedupFixture::new("dedup-full");
    fx.write(&numbered(50));
    fx.read(json!({})).await;

    let full = fx.read(json!({"full": true})).await;
    assert!(full.contains("   50\tline 50"), "{full}");

    crate::tool::read_state::forget_session(&fx.session_id);
    let after_forget = fx.read(json!({})).await;
    assert!(after_forget.contains("   50\tline 50"), "{after_forget}");
}

#[tokio::test]
async fn partial_reread_of_a_changed_file_renders_normally() {
    let fx = DedupFixture::new("dedup-partial-change");
    fx.write(&numbered(50));
    fx.read(json!({})).await;
    fx.write(&numbered(50).replace("line 5\n", "line five\n"));
    let out = fx.read(json!({"start_line": 1, "end_line": 10})).await;
    assert!(out.contains("    5\tline five"), "{out}");
}

#[tokio::test]
async fn oversized_read_stops_at_the_byte_budget_and_the_next_page_is_not_deduped() {
    let budget = super::super::read_selector::READ_BYTE_BUDGET;
    let fx = DedupFixture::new("dedup-cap");
    // 4,000 lines of ~100 bytes: ~400 KB, well past the read byte budget.
    let body: String = (1..=4_000).map(|i| format!("{i:05} {}\n", "x".repeat(94))).collect();
    fx.write(&body);
    let out = fx.read(json!({"start_line": 1})).await;
    assert!(out.len() <= budget + 200, "{} bytes", out.len());
    let hint = out.lines().last().unwrap();
    assert!(hint.contains("more lines (use start_line="), "{hint}");
    let next: usize = hint
        .split("start_line=")
        .nth(1)
        .unwrap()
        .trim_end_matches(" to continue)")
        .parse()
        .unwrap();
    assert!(
        out.contains(&format!("{:>5}\t{:05} ", next - 1, next - 1)),
        "last emitted line precedes the hint"
    );
    assert!(!out.contains(&format!("{:>5}\t{:05} ", next, next)));

    // The next page is not short-circuited: those lines were never shown.
    let page = fx.read(json!({"start_line": next})).await;
    assert!(
        page.starts_with(&format!("{:>5}\t{:05} ", next, next)),
        "{}",
        &page[..80]
    );
}

fn numbered_lines(count: usize) -> String {
    (1..=count).map(|n| format!("L{n}\n")).collect()
}

#[tokio::test]
async fn read_selector_range_includes_leading_and_trailing_context() {
    let output = read_fixture(&numbered_lines(10), json!({"file_path": "sample.txt:2-3"})).await;

    assert!(output.contains("    1\tL1"), "output={output:?}");
    assert!(output.contains("    6\tL6"), "output={output:?}");
    assert!(!output.contains("L7"), "output={output:?}");
}

#[tokio::test]
async fn read_selector_tail_returns_the_last_lines() {
    let output = read_fixture(&numbered_lines(10), json!({"file_path": "sample.txt:-2"})).await;

    assert_eq!(output, "    9\tL9\n   10\tL10\n", "output={output:?}");
}

#[tokio::test]
async fn read_selector_raw_drops_line_numbers() {
    let output = read_fixture(&numbered_lines(4), json!({"file_path": "sample.txt:raw:2"})).await;

    assert_eq!(output, "L2\n", "output={output:?}");
}

#[tokio::test]
async fn read_selector_and_range_params_are_mutually_exclusive() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("sample.txt"), numbered_lines(5)).expect("write");
    let error = ReadTool::new()
        .execute(
            json!({"file_path": "sample.txt:1-2", "start_line": 3}),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect_err("mixing a selector with range parameters must fail");

    assert_eq!(
        error.to_string(),
        "Use either a path selector (path:N-M) or start_line/end_line/offset/limit, not both."
    );
}

#[tokio::test]
async fn read_selector_beyond_the_end_reports_the_file_length() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("sample.txt"), numbered_lines(5)).expect("write");
    let error = ReadTool::new()
        .execute(
            json!({"file_path": "sample.txt:40"}),
            make_ctx(temp.path().to_path_buf()),
        )
        .await
        .expect_err("past the end");

    assert_eq!(
        error.to_string(),
        "Line 40 is beyond end of file (5 lines total). Use :1 to read from the start, or :5 to read the last line."
    );
}

#[tokio::test]
async fn a_file_whose_name_contains_a_colon_reads_whole() {
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("a:1"), "alpha\nbeta\n").expect("write");
    let output = ReadTool::new()
        .execute(json!({"file_path": "a:1"}), make_ctx(temp.path().to_path_buf()))
        .await
        .expect("literal colon path");

    assert_eq!(output.output, "    1\talpha\n    2\tbeta\n");
}

#[tokio::test]
async fn a_missing_path_resolves_through_a_unique_suffix() {
    let temp = tempfile::tempdir().expect("tempdir");
    let nested = temp.path().join("crates/k/src");
    std::fs::create_dir_all(&nested).expect("mkdir");
    std::fs::write(nested.join("x.rs"), "fn main() {}\n").expect("write");

    let output = ReadTool::new()
        .execute(json!({"file_path": "src/x.rs"}), make_ctx(temp.path().to_path_buf()))
        .await
        .expect("suffix resolution should succeed");

    assert!(
        output
            .output
            .starts_with("[Path 'src/x.rs' not found; resolved to 'crates/k/src/x.rs' via suffix match]\n"),
        "output={:?}",
        output.output
    );
    assert!(output.output.contains("fn main() {}"), "output={:?}", output.output);
}

#[tokio::test]
async fn an_ambiguous_suffix_keeps_the_not_found_error() {
    let temp = tempfile::tempdir().expect("tempdir");
    for dir in ["a", "b"] {
        let nested = temp.path().join(dir).join("src");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::write(nested.join("x.rs"), "fn main() {}\n").expect("write");
    }

    let error = ReadTool::new()
        .execute(json!({"file_path": "src/x.rs"}), make_ctx(temp.path().to_path_buf()))
        .await
        .expect_err("ambiguous suffix must not pick a file");
    assert!(error.to_string().starts_with("File not found: src/x.rs"), "{error}");
}

#[tokio::test]
async fn a_selector_page_reports_the_next_selector_to_use() {
    let output = read_fixture(&numbered_lines(30), json!({"file_path": "sample.txt:1-4"})).await;

    // The explicit range ends at line 4, so it does not page; a range stopped by
    // the line cap does.
    assert!(!output.contains("more lines in file"), "output={output:?}");

    let output = read_fixture(
        &numbered_lines(super::DEFAULT_LIMIT + 5),
        json!({"file_path": "sample.txt:1-"}),
    )
    .await;
    assert!(
        output.contains("more lines in file. Use :"),
        "output tail={:?}",
        &output[output.len().saturating_sub(200)..]
    );
}

#[tokio::test]
async fn artifact_urls_require_a_numeric_id() {
    let dir = std::env::temp_dir();
    for path in ["artifact://", "artifact://abc"] {
        let error = ReadTool::new()
            .execute(json!({ "file_path": path }), make_ctx(dir.clone()))
            .await
            .expect_err("a non-numeric artifact id must be refused");
        assert!(
            error.to_string().starts_with("artifact:// ID must be numeric, got: "),
            "{error}"
        );
    }
}

/// Restores `HORON_AGENT_HOME` and the config cache no matter how a test ends.
struct HomeGuard {
    previous: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn set(home: &std::path::Path) -> Self {
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", home);
        crate::config::invalidate_config_cache();
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
        crate::config::invalidate_config_cache();
    }
}

/// Points the process at a temporary `HORON_AGENT_HOME` with the given config.
struct ConfigHome {
    _home: tempfile::TempDir,
    previous: Option<std::ffi::OsString>,
}

impl ConfigHome {
    fn with_config(contents: &str) -> Self {
        let home = tempfile::tempdir().expect("config home");
        let previous = std::env::var_os("HORON_AGENT_HOME");
        crate::env::set_var("HORON_AGENT_HOME", home.path());
        std::fs::write(home.path().join("config.toml"), contents).expect("write config");
        crate::config::invalidate_config_cache();
        Self { _home: home, previous }
    }
}

impl Drop for ConfigHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => crate::env::set_var("HORON_AGENT_HOME", value),
            None => crate::env::remove_var("HORON_AGENT_HOME"),
        }
        crate::config::invalidate_config_cache();
    }
}

fn history_ctx(session: &str, working_dir: std::path::PathBuf) -> ToolContext {
    ToolContext {
        session_id: session.to_string(),
        message_id: "history-message".to_string(),
        tool_call_id: "history-call".to_string(),
        working_dir: Some(working_dir),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: ToolExecutionMode::Direct,
    }
}

#[tokio::test]
async fn read_injects_a_matching_rulebook_rule_once_per_session() {
    let _guard = crate::storage::lock_test_env();
    crate::tool::nested_agents::clear_rule_session_cache_for_test();

    let home = tempfile::tempdir().expect("temp home");
    std::fs::write(
        home.path().join("config.toml"),
        "[features]\nforeign_rules = true\nforeign_rules_mode = \"rulebook\"\n",
    )
    .expect("config");
    let _home = HomeGuard::set(home.path());

    let temp = tempfile::tempdir().expect("work tree");
    std::fs::create_dir_all(temp.path().join(".cursor/rules")).expect("rules dir");
    std::fs::create_dir_all(temp.path().join("src")).expect("src dir");
    std::fs::write(
        temp.path().join(".cursor/rules/x.mdc"),
        "---\nglobs: src/**/*.rs\ndescription: Rust house style\n---\n\nPROJECT-RULE-BODY-7f2a",
    )
    .expect("rule file");
    std::fs::write(temp.path().join("src/a.rs"), "fn a() {}\n").expect("source file");

    let first = ReadTool::new()
        .execute(json!({"file_path": "src/a.rs"}), make_ctx(temp.path().to_path_buf()))
        .await
        .expect("read")
        .output;
    assert!(
        first.contains("<project-rule path=\".cursor/rules/x.mdc\">"),
        "output={first:?}"
    );
    assert!(first.contains("PROJECT-RULE-BODY-7f2a"), "output={first:?}");

    let second = ReadTool::new()
        .execute(json!({"file_path": "src/a.rs"}), make_ctx(temp.path().to_path_buf()))
        .await
        .expect("read")
        .output;
    assert!(
        !second.contains("PROJECT-RULE-BODY-7f2a"),
        "a rule is injected once per session: {second:?}"
    );

    std::fs::write(temp.path().join("README.md"), "docs\n").expect("readme");
    let unmatched = ReadTool::new()
        .execute(json!({"file_path": "README.md"}), make_ctx(temp.path().to_path_buf()))
        .await
        .expect("read")
        .output;
    assert!(
        !unmatched.contains("PROJECT-RULE-BODY-7f2a"),
        "a non-matching glob never injects: {unmatched:?}"
    );

    crate::tool::nested_agents::clear_rule_session_cache_for_test();
}

fn long_rust_file() -> String {
    let mut content = String::from("fn big() {\n");
    for index in 0..410 {
        content.push_str(&format!("    let x{index} = {index};\n"));
    }
    content.push_str("}\n");
    content
}

#[tokio::test]
async fn structural_summary_is_off_by_default() {
    let _lock = crate::storage::lock_test_env();
    let _config = ConfigHome::with_config("[tools]\nprofile = \"core\"\n");
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("big.rs"), long_rust_file()).expect("write file");

    let output = ReadTool::new()
        .execute(json!({"file_path": "big.rs"}), make_ctx(temp.path().to_path_buf()))
        .await
        .expect("read");

    assert!(!output.output.contains("elided"), "{}", output.output);
    assert!(output.output.contains("2\t    let x0 = 0;"), "{}", output.output);
    assert!(output.output.contains("411\t    let x409 = 409;"), "{}", output.output);
}

#[tokio::test]
async fn structural_summary_elides_long_bodies_when_enabled() {
    let _lock = crate::storage::lock_test_env();
    let _config = ConfigHome::with_config("[tools]\nprofile = \"core\"\nread_summary = true\n");
    let temp = tempfile::tempdir().expect("tempdir");
    std::fs::write(temp.path().join("big.rs"), long_rust_file()).expect("write file");

    let output = ReadTool::new()
        .execute(json!({"file_path": "big.rs"}), make_ctx(temp.path().to_path_buf()))
        .await
        .expect("read");

    assert!(output.output.contains("1\tfn big() {"), "{}", output.output);
    assert!(output.output.contains("     … lines 2-411 elided"), "{}", output.output);
    assert!(output.output.contains("412\t}"), "{}", output.output);
    assert!(
        output
            .output
            .contains("[410 lines elided. Read ranges with path:A-B, e.g. big.rs:2-411]"),
        "{}",
        output.output
    );
}

#[tokio::test]
async fn history_url_renders_the_stored_transcript_and_pages_it() {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir().unwrap();
    let _home = EnvGuard::set("HORON_AGENT_HOME", home.path().to_str().unwrap());
    std::fs::write(home.path().join("config.toml"), "[tools]\ncontext_notes = true\n").unwrap();
    crate::config::invalidate_config_cache();

    let mut session = crate::session::Session::create(None, Some("history-test".into()));
    session.add_message(
        crate::message::Role::User,
        vec![crate::message::ContentBlock::Text {
            text: "first question".into(),
            cache_control: None,
        }],
    );
    session.add_message(
        crate::message::Role::Assistant,
        vec![
            crate::message::ContentBlock::Text {
                text: "checking".into(),
                cache_control: None,
            },
            crate::message::ContentBlock::ToolUse {
                id: "call-1".into(),
                name: "read".into(),
                input: json!({"file_path": "a.rs"}),
                thought_signature: None,
            },
        ],
    );
    session.add_message(
        crate::message::Role::User,
        vec![crate::message::ContentBlock::ToolResult {
            tool_use_id: "call-1".into(),
            content: "file text".into(),
            is_error: None,
        }],
    );
    let session_id = session.id.clone();
    session.save().expect("the session is saved");

    let tool = ReadTool::new();
    let dir = tempfile::tempdir().unwrap();
    let whole = tool
        .execute(
            json!({"file_path": "history://current"}),
            history_ctx(&session_id, dir.path().to_path_buf()),
        )
        .await
        .unwrap()
        .output;
    assert!(whole.contains("## user #1"), "{whole}");
    assert!(whole.contains("first question"), "{whole}");
    assert!(whole.contains("## assistant #2"), "{whole}");
    assert!(whole.contains("→ read {\"file_path\":\"a.rs\"}"), "{whole}");
    assert!(whole.contains("← file text"), "{whole}");

    // A selector page carries the same 3 lines of trailing context a file read
    // gets, so lines 1-3 reach line 6 and stop before the tool result.
    let ranged = tool
        .execute(
            json!({"file_path": "history://current:1-3"}),
            history_ctx(&session_id, dir.path().to_path_buf()),
        )
        .await
        .unwrap()
        .output;
    assert!(ranged.contains("## user #1"), "{ranged}");
    assert!(ranged.contains("→ read {\"file_path\":\"a.rs\"}"), "{ranged}");
    assert!(!ranged.contains("← file text"), "{ranged}");

    // The plan's marquee form: a bounded peek at the start of the transcript.
    let peek = tool
        .execute(
            json!({"file_path": "history://current:1-5"}),
            history_ctx(&session_id, dir.path().to_path_buf()),
        )
        .await
        .unwrap()
        .output;
    assert!(peek.contains("## user #1"), "{peek}");
    assert!(peek.contains("## assistant #2"), "{peek}");
    assert!(peek.contains("## user #3"), "{peek}");
    assert!(!peek.contains("← file text"), "{peek}");

    // Offset paging reports how to reach the rest of the transcript.
    let paged = tool
        .execute(
            json!({"file_path": "history://current", "offset": 0, "limit": 3}),
            history_ctx(&session_id, dir.path().to_path_buf()),
        )
        .await
        .unwrap()
        .output;
    assert!(paged.contains("first question"), "{paged}");
    assert!(
        paged.contains("... 6 more lines (use offset=3 to continue)"),
        "the page must say how to continue: {paged:?}"
    );

    let missing = tool
        .execute(
            json!({"file_path": "history://current"}),
            history_ctx("no-such-session", dir.path().to_path_buf()),
        )
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(missing, "No stored history for this session yet.");
}
