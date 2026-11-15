//! Fields of the `TOOL_LIFECYCLE` log lines.
//!
//! A dispatch writes a `start` line and then a `done` or an `error` line.
//! What these fields say about a call is a pure function of the input and
//! the context, and neither changes between the two lines, so the dispatcher
//! builds the list once, lends it to the first line and relabels it for the
//! second.

use super::{ToolContext, path_policy};
use serde_json::Value;

const PHASE_KEY: &str = "phase";

/// The fields every lifecycle line of one call carries, `phase` first.
pub(super) fn fields(
    phase: &str,
    requested_name: &str,
    resolved_name: &str,
    input: &Value,
    ctx: &ToolContext,
) -> Vec<(String, String)> {
    let cwd = ctx
        .working_dir
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "none".to_string());
    let mut fields = vec![
        (PHASE_KEY.to_string(), phase.to_string()),
        ("tool_name".to_string(), requested_name.to_string()),
        ("resolved_tool_name".to_string(), resolved_name.to_string()),
        ("session_id".to_string(), ctx.session_id.clone()),
        ("message_id".to_string(), ctx.message_id.clone()),
        ("tool_call_id".to_string(), ctx.tool_call_id.clone()),
        ("execution_mode".to_string(), format!("{:?}", ctx.execution_mode)),
        ("cwd".to_string(), cwd),
        ("input_json_bytes".to_string(), json_len(input).to_string()),
    ];

    if let Some(object) = input.as_object() {
        let mut keys = object.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        fields.push(("input_keys".to_string(), keys.join(",")));

        let touched_paths = path_policy::touched_paths(input, ctx)
            .into_iter()
            .map(|(key, path)| format!("{key}:{}", path.display()))
            .collect::<Vec<_>>();
        if !touched_paths.is_empty() {
            fields.push(("touched_paths".to_string(), touched_paths.join(",")));
            fields.push(("touched_path_count".to_string(), touched_paths.len().to_string()));
        }

        for text_key in ["command", "prompt", "task", "query", "content"] {
            if let Some(text) = object.get(text_key).and_then(Value::as_str) {
                fields.push((format!("{text_key}_bytes"), text.len().to_string()));
                fields.push((format!("{text_key}_chars"), text.chars().count().to_string()));
            }
        }
    }

    fields
}

/// The list as borrowed pairs, for a line that leaves the list in place.
pub(super) fn borrowed(fields: &[(String, String)]) -> impl Iterator<Item = (&str, &str)> {
    fields.iter().map(|(key, value)| (key.as_str(), value.as_str()))
}

/// Relabel a list built by [`fields`] for a later line of the same call.
pub(super) fn set_phase(fields: &mut [(String, String)], phase: &str) {
    if let Some((_, value)) = fields.iter_mut().find(|(key, _)| key.as_str() == PHASE_KEY) {
        phase.clone_into(value);
    }
}

/// Length in bytes of `value` as compact JSON: the length of what
/// `serde_json::to_string` returns, counted without building the text.
fn json_len(value: &Value) -> usize {
    struct Count(usize);

    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }

        fn write_all(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            self.0 += bytes.len();
            Ok(())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut count = Count(0);
    match serde_json::to_writer(&mut count, value) {
        Ok(()) => count.0,
        // The text form reported an empty string, so 0, for a value it could
        // not write.
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::super::ToolExecutionMode;
    use super::*;
    use serde_json::json;
    use std::path::{Path, PathBuf};

    fn context(working_dir: Option<&str>) -> ToolContext {
        ToolContext {
            session_id: "session-1".to_string(),
            message_id: "message-1".to_string(),
            tool_call_id: "call-1".to_string(),
            working_dir: working_dir.map(PathBuf::from),
            stdin_request_tx: None,
            graceful_shutdown_signal: None,
            execution_mode: ToolExecutionMode::Direct,
        }
    }

    fn text_len(value: &Value) -> usize {
        serde_json::to_string(value).expect("a JSON value serializes").len()
    }

    #[test]
    fn json_len_is_the_length_of_the_serialized_text() {
        let values = [
            json!(null),
            json!(true),
            json!(""),
            json!("plain"),
            json!("quote \" backslash \\ newline \n tab \t nul \u{0} bell \u{7} del \u{7f}"),
            json!("한글 😀 é \u{2028}"),
            json!(0),
            json!(-17),
            json!(1.5),
            json!(1e300),
            json!(u64::MAX),
            json!(i64::MIN),
            json!([]),
            json!({}),
            json!([1, "a", null, true, {"k": [false]}]),
            json!({"key \" with \n escapes": "v", "한": {"a": {"b": [1, 2, 3]}}}),
            json!({"file_path": "src/lib.rs", "content": "x\n".repeat(5 * 1024)}),
        ];
        for value in &values {
            assert_eq!(json_len(value), text_len(value), "{value}");
        }
    }

    #[test]
    fn fields_keep_their_names_order_and_values() {
        let input = json!({"zeta": 1, "file_path": "notes/a.txt", "content": "héllo", "command": "ls"});
        let touched = Path::new("/work").join("notes/a.txt");
        let expected = [
            ("phase", "start".to_string()),
            ("tool_name", "write_alias".to_string()),
            ("resolved_tool_name", "write".to_string()),
            ("session_id", "session-1".to_string()),
            ("message_id", "message-1".to_string()),
            ("tool_call_id", "call-1".to_string()),
            ("execution_mode", "Direct".to_string()),
            ("cwd", "/work".to_string()),
            ("input_json_bytes", text_len(&input).to_string()),
            ("input_keys", "command,content,file_path,zeta".to_string()),
            ("touched_paths", format!("file_path:{}", touched.display())),
            ("touched_path_count", "1".to_string()),
            ("command_bytes", "2".to_string()),
            ("command_chars", "2".to_string()),
            ("content_bytes", "6".to_string()),
            ("content_chars", "5".to_string()),
        ]
        .map(|(key, value)| (key.to_string(), value));

        let built = fields("start", "write_alias", "write", &input, &context(Some("/work")));

        assert_eq!(built, expected);
    }

    #[test]
    fn an_input_that_is_not_an_object_has_only_the_call_fields() {
        let input = json!("just text");

        let built = fields("start", "probe", "probe", &input, &context(None));

        let keys = built.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>();
        assert_eq!(
            keys,
            [
                "phase",
                "tool_name",
                "resolved_tool_name",
                "session_id",
                "message_id",
                "tool_call_id",
                "execution_mode",
                "cwd",
                "input_json_bytes"
            ]
        );
        assert_eq!(built[7].1, "none");
        assert_eq!(built[8].1, text_len(&input).to_string());
    }

    #[test]
    fn a_relabelled_list_equals_one_built_for_that_phase() {
        let input = json!({"path": "a", "paths": ["b", "c"], "query": "find me", "prompt": "p"});
        let ctx = context(Some("/work"));
        for phase in ["done", "error"] {
            let mut relabelled = fields("start", "probe", "probe", &input, &ctx);
            set_phase(&mut relabelled, phase);

            assert_eq!(relabelled, fields(phase, "probe", "probe", &input, &ctx));
        }
    }

    #[test]
    fn borrowed_pairs_are_the_list_in_order() {
        let built = fields("start", "probe", "probe", &json!({"task": "t"}), &context(None));

        let pairs = borrowed(&built)
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<Vec<_>>();

        assert_eq!(pairs, built);
    }
}
