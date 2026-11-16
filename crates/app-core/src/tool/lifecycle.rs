//! Fields of the `TOOL_LIFECYCLE` log lines.
//!
//! A dispatch writes a `start` line and then a `done` or an `error` line.
//! What these fields say about a call is a pure function of the input and
//! the context, and neither changes between the two lines, so the dispatcher
//! builds the list once, lends it to the first line and relabels it for the
//! second.
//!
//! A field borrows from the input and the context instead of holding text of
//! its own: the list is one allocation, and a value is printed straight into
//! the line when the line is written.

use super::{ToolContext, ToolExecutionMode, path_policy};
use serde_json::{Map, Value};
use std::fmt;
use std::path::{Path, PathBuf};

const PHASE_KEY: &str = "phase";

/// Room for every field of a `done` line: the call, the input, the timings
/// and the output sizes.
const FIELD_CAPACITY: usize = 32;

/// The text inputs a line reports the size of, each with its two field names.
const TEXT_SIZE_FIELDS: [(&str, &str, &str); 5] = [
    ("command", "command_bytes", "command_chars"),
    ("prompt", "prompt_bytes", "prompt_chars"),
    ("task", "task_bytes", "task_chars"),
    ("query", "query_bytes", "query_chars"),
    ("content", "content_bytes", "content_chars"),
];

/// The value of one lifecycle field, printed when its line is written.
pub(crate) enum FieldValue<'a> {
    /// Text that outlives the line: a name, an id, a phase, a reason.
    Text(&'a str),
    /// Text made for this line.
    Owned(String),
    Count(usize),
    Millis(u64),
    Mode(ToolExecutionMode),
    /// The working directory, `none` without one.
    Cwd(Option<&'a Path>),
    /// The keys of an object input, sorted and comma-joined.
    InputKeys(&'a Map<String, Value>),
    /// The paths a call names, each as `key:path`, comma-joined.
    TouchedPaths(Vec<(&'static str, PathBuf)>),
}

impl fmt::Display for FieldValue<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text(text) => f.write_str(text),
            Self::Owned(text) => f.write_str(text),
            Self::Count(count) => write!(f, "{count}"),
            Self::Millis(millis) => write!(f, "{millis}"),
            Self::Mode(mode) => write!(f, "{mode:?}"),
            Self::Cwd(Some(path)) => write!(f, "{}", path.display()),
            Self::Cwd(None) => f.write_str("none"),
            Self::InputKeys(object) => fmt::Display::fmt(&SortedKeys(object.keys()), f),
            Self::TouchedPaths(paths) => {
                for (index, (key, path)) in paths.iter().enumerate() {
                    if index > 0 {
                        f.write_str(",")?;
                    }
                    write!(f, "{key}:{}", path.display())?;
                }
                Ok(())
            }
        }
    }
}

/// Keys printed sorted and comma-joined.
///
/// A JSON object hands its keys out sorted unless `serde_json` is built to
/// keep insertion order, so the sort and its list are paid for only then.
struct SortedKeys<I>(I);

impl<'k, I> fmt::Display for SortedKeys<I>
where
    I: Iterator<Item = &'k String> + Clone,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.clone().is_sorted() {
            return write_joined(f, self.0.clone());
        }
        let mut keys = self.0.clone().collect::<Vec<_>>();
        keys.sort();
        write_joined(f, keys.into_iter())
    }
}

fn write_joined<'k>(f: &mut fmt::Formatter<'_>, keys: impl Iterator<Item = &'k String>) -> fmt::Result {
    for (index, key) in keys.enumerate() {
        if index > 0 {
            f.write_str(",")?;
        }
        f.write_str(key)?;
    }
    Ok(())
}

/// The fields of one lifecycle line, in the order they were added.
pub(super) type Fields<'a> = Vec<(&'static str, FieldValue<'a>)>;

/// The fields every lifecycle line of one call carries, `phase` first.
pub(super) fn fields<'a>(
    phase: &'a str,
    requested_name: &'a str,
    resolved_name: &'a str,
    input: &'a Value,
    ctx: &'a ToolContext,
) -> Fields<'a> {
    let mut fields = Vec::with_capacity(FIELD_CAPACITY);
    fields.extend([
        (PHASE_KEY, FieldValue::Text(phase)),
        ("tool_name", FieldValue::Text(requested_name)),
        ("resolved_tool_name", FieldValue::Text(resolved_name)),
        ("session_id", FieldValue::Text(&ctx.session_id)),
        ("message_id", FieldValue::Text(&ctx.message_id)),
        ("tool_call_id", FieldValue::Text(&ctx.tool_call_id)),
        ("execution_mode", FieldValue::Mode(ctx.execution_mode)),
        ("cwd", FieldValue::Cwd(ctx.working_dir.as_deref())),
        ("input_json_bytes", FieldValue::Count(json_len(input))),
    ]);

    if let Some(object) = input.as_object() {
        fields.push(("input_keys", FieldValue::InputKeys(object)));

        let touched_paths = path_policy::touched_paths(input, ctx);
        if !touched_paths.is_empty() {
            let count = touched_paths.len();
            fields.push(("touched_paths", FieldValue::TouchedPaths(touched_paths)));
            fields.push(("touched_path_count", FieldValue::Count(count)));
        }

        for (text_key, bytes_key, chars_key) in TEXT_SIZE_FIELDS {
            if let Some(text) = object.get(text_key).and_then(Value::as_str) {
                fields.push((bytes_key, FieldValue::Count(text.len())));
                fields.push((chars_key, FieldValue::Count(text.chars().count())));
            }
        }
    }

    fields
}

/// The list as borrowed pairs, for a line that leaves the list in place.
pub(super) fn borrowed<'f, 'a>(
    fields: &'f [(&'static str, FieldValue<'a>)],
) -> impl Iterator<Item = (&'static str, &'f FieldValue<'a>)> {
    fields.iter().map(|(key, value)| (*key, value))
}

/// Relabel a list built by [`fields`] for a later line of the same call.
pub(super) fn set_phase<'a>(fields: &mut [(&'static str, FieldValue<'a>)], phase: &'a str) {
    if let Some((_, value)) = fields.iter_mut().find(|(key, _)| *key == PHASE_KEY) {
        *value = FieldValue::Text(phase);
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
    use super::*;
    use serde_json::json;

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

    /// The fields as the text a line carries.
    fn rendered(fields: &[(&'static str, FieldValue<'_>)]) -> Vec<(String, String)> {
        fields
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
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

        let ctx = context(Some("/work"));
        let built = fields("start", "write_alias", "write", &input, &ctx);

        assert_eq!(rendered(&built), expected);
    }

    #[test]
    fn an_input_that_is_not_an_object_has_only_the_call_fields() {
        let input = json!("just text");
        let ctx = context(None);

        let built = fields("start", "probe", "probe", &input, &ctx);

        let keys = built.iter().map(|(key, _)| *key).collect::<Vec<_>>();
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
        assert_eq!(built[7].1.to_string(), "none");
        assert_eq!(built[8].1.to_string(), text_len(&input).to_string());
    }

    #[test]
    fn a_relabelled_list_equals_one_built_for_that_phase() {
        let input = json!({"path": "a", "paths": ["b", "c"], "query": "find me", "prompt": "p"});
        let ctx = context(Some("/work"));
        for phase in ["done", "error"] {
            let mut relabelled = fields("start", "probe", "probe", &input, &ctx);
            set_phase(&mut relabelled, phase);

            assert_eq!(
                rendered(&relabelled),
                rendered(&fields(phase, "probe", "probe", &input, &ctx))
            );
        }
    }

    #[test]
    fn borrowed_pairs_are_the_list_in_order() {
        let input = json!({"task": "t"});
        let ctx = context(None);
        let built = fields("start", "probe", "probe", &input, &ctx);

        let pairs = borrowed(&built)
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<Vec<_>>();

        assert_eq!(pairs, rendered(&built));
    }

    #[test]
    fn keys_handed_out_of_order_are_printed_sorted() {
        let unsorted = ["zeta", "alpha", "mid"].map(str::to_string);
        assert_eq!(SortedKeys(unsorted.iter()).to_string(), "alpha,mid,zeta");

        let sorted = ["alpha", "mid", "zeta"].map(str::to_string);
        assert_eq!(SortedKeys(sorted.iter()).to_string(), "alpha,mid,zeta");

        let none: [String; 0] = [];
        assert_eq!(SortedKeys(none.iter()).to_string(), "");
    }

    #[test]
    fn a_value_prints_the_text_the_line_used_to_hold() {
        let turn = ToolExecutionMode::AgentTurn;
        assert_eq!(FieldValue::Mode(turn).to_string(), format!("{turn:?}"));
        assert_eq!(FieldValue::Mode(turn).to_string(), "AgentTurn");
        assert_eq!(
            FieldValue::Owned("made for the line".to_string()).to_string(),
            "made for the line"
        );
        assert_eq!(FieldValue::Millis(u64::MAX).to_string(), u64::MAX.to_string());
        assert_eq!(FieldValue::Count(0).to_string(), "0");

        let paths = vec![
            ("path", PathBuf::from("/work/a")),
            ("paths", PathBuf::from("/work/b c")),
        ];
        assert_eq!(
            FieldValue::TouchedPaths(paths).to_string(),
            "path:/work/a,paths:/work/b c"
        );
    }
}
