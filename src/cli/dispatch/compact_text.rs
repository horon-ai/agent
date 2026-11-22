//! `compact-text`: summarize a local text file or stdin without changing it.
use anyhow::{Context, Result, bail};
use std::io::Read;

use crate::aux_completion::{CompletionRequest, MAX_PROMPT_BYTES, complete};

/// Input cap, leaving room in the request for the fixed instructions.
const MAX_INPUT_BYTES: usize = MAX_PROMPT_BYTES - 4 * 1024;

const SYSTEM: &str = "You compact text. Return JSON with `summary` (a shorter text that keeps every decision, \
name, number and instruction) and `omitted` (an array of short strings, one per kind of detail you left out). \
Never invent facts.";

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["summary", "omitted"],
        "properties": {
            "summary": {"type": "string"},
            "omitted": {"type": "array", "items": {"type": "string"}}
        }
    })
}

/// Read the source once, refusing oversized or non-UTF-8 input.
pub(super) fn read_source(source: &str) -> Result<String> {
    let mut bytes = Vec::new();
    let limit = MAX_INPUT_BYTES as u64 + 1;
    if source == "-" {
        std::io::stdin().lock().take(limit).read_to_end(&mut bytes)?;
    } else {
        std::fs::File::open(source)
            .with_context(|| format!("cannot open {source}"))?
            .take(limit)
            .read_to_end(&mut bytes)?;
    }
    if bytes.len() > MAX_INPUT_BYTES {
        bail!("input is larger than {MAX_INPUT_BYTES} bytes");
    }
    if bytes.contains(&0) {
        bail!("input looks binary (contains NUL bytes)");
    }
    String::from_utf8(bytes).map_err(|_| anyhow::anyhow!("input is not valid UTF-8 text"))
}

/// Split a validated response into the summary and the loss report.
pub(super) fn parse_result(text: &str) -> Result<(String, Vec<String>)> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).context("response was not JSON")?;
    let summary = value["summary"].as_str().context("response has no summary")?.to_owned();
    let omitted = value["omitted"]
        .as_array()
        .context("response has no omitted list")?
        .iter()
        .filter_map(|item| item.as_str().map(str::to_owned))
        .collect();
    Ok((summary, omitted))
}

pub(super) async fn run(source: &str, max_tokens: Option<u32>) -> Result<()> {
    let text = read_source(source)?;
    let mut request = CompletionRequest::new(text)?
        .with_system(SYSTEM)?
        .with_schema(schema())?;
    if let Some(max_tokens) = max_tokens {
        request = request.with_max_tokens(max_tokens)?;
    }
    let (summary, omitted) = parse_result(&complete(request).await?)?;
    println!("{summary}");
    eprintln!("omitted ({}):", omitted.len());
    for item in &omitted {
        eprintln!("- {item}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_oversized_and_missing_input_is_refused_and_the_source_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("b.bin");
        std::fs::write(&binary, b"a\0b").unwrap();
        let big = dir.path().join("big.txt");
        std::fs::write(&big, "x".repeat(MAX_INPUT_BYTES + 1)).unwrap();
        let good = dir.path().join("ok.txt");
        std::fs::write(&good, "hello").unwrap();
        assert!(read_source(binary.to_str().unwrap()).is_err());
        assert!(read_source(big.to_str().unwrap()).is_err());
        assert!(read_source(dir.path().join("none").to_str().unwrap()).is_err());
        assert_eq!(read_source(good.to_str().unwrap()).unwrap(), "hello");
        assert_eq!(std::fs::read(&binary).unwrap(), b"a\0b");
        assert_eq!(std::fs::read_to_string(&good).unwrap(), "hello");
    }

    #[test]
    fn the_result_separates_the_summary_from_what_was_left_out() {
        let (summary, omitted) = parse_result(r#"{"summary":"short","omitted":["dates","names"]}"#).unwrap();
        assert_eq!(summary, "short");
        assert_eq!(omitted, ["dates", "names"]);
        assert!(parse_result("not json").is_err());
    }
}
