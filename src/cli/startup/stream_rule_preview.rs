use std::io::{self, Read, Write};

use anyhow::{Context, Result};

use crate::agent::stream_rules::preview_stream_rules;
use crate::config::{Config, StreamRuleConfig};

const MAX_SAMPLE_BYTES: usize = 64 * 1024;

pub(in crate::cli) fn run(sample: Option<&str>) -> Result<()> {
    let config = Config::load_strict()?;
    let stdin_sample;
    let sample = if sample == Some("-") {
        let mut text = String::new();
        io::stdin()
            .lock()
            .take((MAX_SAMPLE_BYTES + 1) as u64)
            .read_to_string(&mut text)
            .context("cannot read a UTF-8 stream rule sample")?;
        stdin_sample = text;
        Some(stdin_sample.as_str())
    } else {
        sample
    };
    check(&config.stream_rules, sample, io::stdout().lock())
}

fn check(entries: &[StreamRuleConfig], sample: Option<&str>, mut out: impl Write) -> Result<()> {
    if sample.is_some_and(|text| text.len() > MAX_SAMPLE_BYTES) {
        anyhow::bail!("stream rule sample exceeds {MAX_SAMPLE_BYTES} bytes");
    }
    let matches = preview_stream_rules(entries, sample)?;
    if sample.is_none() {
        writeln!(out, "Validated {} stream rule(s).", entries.len())?;
    } else if matches.is_empty() {
        writeln!(out, "No text rules matched the sample.")?;
    } else {
        for (id, delivery) in matches {
            writeln!(out, "{id:?}: text matched the pattern ({delivery:?} delivery)")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_matches_text_rules_without_emitting_rule_bodies() -> Result<()> {
        let rules = [
            StreamRuleConfig {
                id: "text-warning".to_owned(),
                pattern: r"\bwarning\b".to_owned(),
                body: "private reminder".to_owned(),
                ..Default::default()
            },
            StreamRuleConfig {
                id: "tool-warning".to_owned(),
                pattern: "warning".to_owned(),
                scope: vec!["tool:bash".to_owned()],
                ..Default::default()
            },
        ];
        let mut output = Vec::new();
        check(&rules, Some("a warning showed up"), &mut output)?;
        assert_eq!(
            String::from_utf8(output)?,
            "\"text-warning\": text matched the pattern (Interrupt delivery)\n"
        );
        Ok(())
    }

    #[test]
    fn invalid_regex_identifies_the_rule_without_reporting_any_matches() {
        let rules = [StreamRuleConfig {
            id: "invalid-regex".to_owned(),
            pattern: "[".to_owned(),
            ..Default::default()
        }];
        let mut output = Vec::new();
        let error = check(&rules, None, &mut output).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("invalid-regex") && message.contains("pattern does not compile"));
        assert!(output.is_empty());
    }
}
