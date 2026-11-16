//! The operation ring stores exactly `truncate(redact(text))` for both fields,
//! whatever buffers it reuses to get there.

use super::*;
use serde_json::json;
use std::ffi::OsString;
use std::sync::MutexGuard;
use std::sync::atomic::Ordering;

const FAKE_KEY: &str = "ghp_FAKEKEYFAKEKEYFAKEKEYFAKEKEY12";

/// Characters `{"command":"` puts in front of a `command` value.
const COMMAND_PREFIX_CHARS: usize = 12;

/// Holds the test environment lock with recording switched on and an empty ring.
///
/// The agent home points at a temporary directory for as long as recording is
/// on: another test's panic hook writes a bundle wherever the home is.
struct Recording {
    previous_home: Option<OsString>,
    _home: tempfile::TempDir,
    _env: MutexGuard<'static, ()>,
}

impl Recording {
    fn start() -> Self {
        let env = crate::storage::lock_test_env();
        let previous_home = std::env::var_os("HORON_HOME");
        let home = tempfile::TempDir::new().expect("create temp agent home");
        crate::env::set_var("HORON_HOME", home.path());
        operations().clear();
        BUNDLES_ENABLED.store(true, Ordering::SeqCst);
        Self {
            previous_home,
            _home: home,
            _env: env,
        }
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        BUNDLES_ENABLED.store(false, Ordering::SeqCst);
        operations().clear();
        match self.previous_home.take() {
            Some(value) => crate::env::set_var("HORON_HOME", value),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

/// The stored form of one field, written out independently of the ring code.
fn stored_form(text: &str, limit: usize) -> String {
    let redacted = redact_secrets(text);
    match redacted.char_indices().nth(limit) {
        Some((index, _)) => format!("{}[truncated]", &redacted[..index]),
        None => redacted,
    }
}

fn stored_detail(input: &serde_json::Value) -> String {
    let serialized = serde_json::to_string(input).expect("a JSON value serializes");
    stored_form(&serialized, MAX_FIELD_CHARS)
}

fn record_and_read(tool: &str, input: &serde_json::Value) -> OperationRecord {
    record_tool_operation(tool, input);
    operations_snapshot().pop().expect("the dispatch was recorded")
}

#[test]
fn detail_is_the_redacted_then_truncated_input() {
    let _recording = Recording::start();
    let inputs = [
        json!({ "file_path": "src/main.rs", "limit": 20 }),
        json!({ "command": "x".repeat(MAX_FIELD_CHARS * 3) }),
        json!({ "content": "가".repeat(MAX_FIELD_CHARS) }),
        json!({ "content": format!("a{}", "🙂é".repeat(MAX_FIELD_CHARS)) }),
        json!({ "command": format!("curl -H 'Authorization: {FAKE_KEY}'") }),
        json!({ "command": format!("{} Authorization: {FAKE_KEY}", "p".repeat(MAX_FIELD_CHARS * 2)) }),
    ];

    for input in &inputs {
        let record = record_and_read("bash", input);
        assert_eq!(record.detail, stored_detail(input));
        assert_eq!(record.tool, "bash");
        assert!(
            !record.detail.contains(FAKE_KEY),
            "the ring kept a token: {}",
            record.detail
        );
    }
}

#[test]
fn detail_is_cut_only_past_the_character_limit() {
    let _recording = Recording::start();
    // `{"k":"` and `"}` add eight characters around the value.
    let at_limit = json!({ "k": "x".repeat(MAX_FIELD_CHARS - 8) });
    let one_over = json!({ "k": "x".repeat(MAX_FIELD_CHARS - 7) });

    let kept = record_and_read("write", &at_limit);
    assert_eq!(kept.detail.chars().count(), MAX_FIELD_CHARS);
    assert_eq!(kept.detail, serde_json::to_string(&at_limit).expect("serializes"));

    // One character over the limit: only the closing brace is cut.
    let cut = record_and_read("write", &one_over);
    assert_eq!(cut.detail.chars().count(), MAX_FIELD_CHARS + "[truncated]".len());
    assert!(
        cut.detail.ends_with("x\"[truncated]"),
        "unexpected tail: {}",
        cut.detail
    );
}

#[test]
fn secret_across_the_cut_is_redacted_before_the_cut() {
    let _recording = Recording::start();
    // The token starts two characters before the limit, so a cut made before
    // redaction would keep its first two characters and hide the rest from the scan.
    let lead = " Authorization: ";
    let padding = MAX_FIELD_CHARS - COMMAND_PREFIX_CHARS - lead.len() - 2;
    let input = json!({ "command": format!("{}{lead}{FAKE_KEY} tail", "p".repeat(padding)) });

    let record = record_and_read("bash", &input);

    assert_eq!(record.detail, stored_detail(&input));
    assert!(
        record.detail.ends_with("[truncated]"),
        "unexpected tail: {}",
        record.detail
    );
    assert!(
        !record.detail.contains("ghp_"),
        "the ring kept part of a token: {}",
        record.detail
    );
    assert!(!record.detail.contains("gh[truncated]"), "the cut ran before redaction");
}

#[test]
fn tool_name_is_redacted_and_cut_at_its_own_limit() {
    let _recording = Recording::start();
    let long_name = "t".repeat(MAX_TOOL_CHARS + 40);
    let secret_name = format!("mcp__server__{FAKE_KEY}");
    let input = json!({});

    let long = record_and_read(&long_name, &input);
    assert_eq!(long.tool, stored_form(&long_name, MAX_TOOL_CHARS));
    assert_eq!(long.tool.chars().count(), MAX_TOOL_CHARS + "[truncated]".len());

    let secret = record_and_read(&secret_name, &input);
    assert_eq!(secret.tool, stored_form(&secret_name, MAX_TOOL_CHARS));
    assert!(
        !secret.tool.contains(FAKE_KEY),
        "the ring kept a token: {}",
        secret.tool
    );
}

#[test]
fn a_reused_slot_holds_only_its_new_record() {
    let _recording = Recording::start();
    let long_name = "n".repeat(MAX_TOOL_CHARS * 2);
    for index in 0..OPERATION_RING_CAPACITY {
        record_tool_operation(
            &long_name,
            &json!({ "command": "L".repeat(MAX_FIELD_CHARS * 2 + index) }),
        );
    }

    // Every record from here on is written into strings an older, longer record owned.
    let short_inputs: Vec<serde_json::Value> = (0..OPERATION_RING_CAPACITY + 7)
        .map(|index| json!({ "index": index }))
        .collect();
    for input in &short_inputs {
        record_tool_operation("ls", input);
    }

    let records = operations_snapshot();
    assert_eq!(records.len(), OPERATION_RING_CAPACITY);
    let newest = &short_inputs[short_inputs.len() - OPERATION_RING_CAPACITY..];
    for (record, input) in records.iter().zip(newest) {
        assert_eq!(record.tool, "ls");
        assert_eq!(record.detail, stored_detail(input));
    }
    for pair in records.windows(2) {
        assert_eq!(pair[1].seq, pair[0].seq + 1, "the ring must keep dispatch order");
    }
}

#[test]
fn oversized_input_is_recorded_and_its_buffer_released() {
    let _recording = Recording::start();
    let oversized = json!({ "content": "z".repeat(SCRATCH_KEEP_BYTES * 2) });
    let small = json!({ "file_path": "README.md" });

    let record = record_and_read("write", &oversized);
    assert_eq!(record.detail, stored_detail(&oversized));
    let kept_after_oversized = SERIALIZE_SCRATCH.with(|scratch| scratch.borrow().capacity());
    assert!(
        kept_after_oversized <= SCRATCH_KEEP_BYTES,
        "the thread kept a {kept_after_oversized}-byte buffer"
    );

    let record = record_and_read("read", &small);
    assert_eq!(record.detail, stored_detail(&small));
}

#[test]
fn nothing_is_recorded_while_bundles_are_off() {
    let _recording = Recording::start();
    BUNDLES_ENABLED.store(false, Ordering::SeqCst);

    record_tool_operation("bash", &json!({ "command": "true" }));

    assert!(operations_snapshot().is_empty());
}
