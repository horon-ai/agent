//! One benchmark observation: the JSONL row written per `(task, repeat)`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A single run result. Field names are the JSONL contract shared by `run` and
/// `compare`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ResultRow {
    pub task: String,
    pub rep: u32,
    pub arm: String,
    pub pass: bool,
    /// Total number of `tool_start` events in the run.
    #[serde(default)]
    pub tool_calls: u64,
    /// `tool_start` counts by tool name.
    #[serde(default)]
    pub tool_counts: BTreeMap<String, u64>,
    /// Sum of `tokens.input` across every `tokens` event.
    #[serde(default)]
    pub input_tokens: u64,
    /// Sum of `tokens.output` across every `tokens` event.
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub duration_ms: u64,
}

impl ResultRow {
    /// Sum of [`ResultRow::tool_counts`].
    pub fn counted_tool_calls(&self) -> u64 {
        self.tool_counts.values().sum()
    }
}

/// Read a `results.jsonl` file, skipping blank lines.
pub(crate) fn read_jsonl(text: &str, origin: &str) -> anyhow::Result<Vec<ResultRow>> {
    let mut rows = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let row: ResultRow =
            serde_json::from_str(line).map_err(|error| anyhow::anyhow!("{origin}:{}: {error}", index + 1))?;
        rows.push(row);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_round_trip_through_jsonl() {
        let row = ResultRow {
            task: "task-1".into(),
            rep: 0,
            arm: "base".into(),
            pass: true,
            tool_calls: 3,
            tool_counts: BTreeMap::from([("read".to_string(), 2), ("edit".to_string(), 1)]),
            input_tokens: 1200,
            output_tokens: 340,
            duration_ms: 9000,
        };
        let line = serde_json::to_string(&row).unwrap();
        let parsed = read_jsonl(&line, "inline").unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].task, "task-1");
        assert!(parsed[0].pass);
        assert_eq!(parsed[0].counted_tool_calls(), 3);
    }

    #[test]
    fn missing_optional_fields_default() {
        let rows = read_jsonl(r#"{"task":"t","rep":0,"arm":"a","pass":false}"#, "inline").unwrap();
        assert_eq!(rows[0].tool_calls, 0);
        assert_eq!(rows[0].output_tokens, 0);
    }

    #[test]
    fn blank_lines_are_skipped_and_bad_lines_are_located() {
        let rows = read_jsonl("\n{\"task\":\"t\",\"rep\":0,\"arm\":\"a\",\"pass\":true}\n\n", "x").unwrap();
        assert_eq!(rows.len(), 1);
        let error = read_jsonl("\nnot json\n", "results.jsonl").unwrap_err().to_string();
        assert!(error.starts_with("results.jsonl:2:"), "{error}");
    }
}
