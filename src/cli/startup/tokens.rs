use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use anyhow::{Context, Result};

use crate::util::{TokenFamily, estimate_tokens_for_family};

const MAX_INPUT_BYTES: usize = 16 * 1024 * 1024;

pub(super) fn run(text: Option<&str>, file: Option<&Path>, cwd: Option<&Path>, model: Option<&str>) -> Result<()> {
    let input;
    let text = match (file, text) {
        (Some(file), _) => {
            let path = cwd.map_or_else(|| file.to_path_buf(), |cwd| cwd.join(file));
            input = read_utf8(File::open(&path).with_context(|| format!("cannot open {}", path.display()))?)?;
            &input
        }
        (None, None | Some("-")) => {
            input = read_utf8(io::stdin().lock())?;
            &input
        }
        (None, Some(text)) => text,
    };
    report(text, model, io::stdout().lock())
}

fn read_utf8(input: impl Read) -> Result<String> {
    let mut bytes = Vec::new();
    input.take((MAX_INPUT_BYTES + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_INPUT_BYTES {
        anyhow::bail!("token estimation input exceeds 16 MiB");
    }
    String::from_utf8(bytes).context("token estimation input must be UTF-8")
}

fn report(text: &str, model: Option<&str>, mut out: impl Write) -> Result<()> {
    let matches_family = |families: &[&str]| {
        model.is_some_and(|model| {
            families.iter().any(|family| {
                model == *family || model.strip_prefix(family).is_some_and(|suffix| suffix.starts_with('-'))
            })
        })
    };
    let (family, label) = if matches_family(&["gpt-4o", "gpt-4.1", "gpt-5", "o1", "o3", "o4"]) {
        (TokenFamily::O200k, "o200k estimate")
    } else if matches_family(&["gpt-4", "gpt-3.5-turbo"]) {
        (TokenFamily::Cl100k, "cl100k estimate")
    } else {
        (TokenFamily::O200k, "o200k fallback estimate")
    };
    let count = estimate_tokens_for_family(text, family);
    if let Some(model) = model {
        writeln!(out, "{count} tokens ({label}, model {model:?})")?;
    } else {
        writeln!(out, "{count} tokens ({label}, no model specified)")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_report_names_the_model_estimator() -> Result<()> {
        let mut output = Vec::new();
        report("안녕하세요", Some("gpt-4o"), &mut output)?;
        assert_eq!(
            String::from_utf8(output)?,
            "4 tokens (o200k estimate, model \"gpt-4o\")\n"
        );
        Ok(())
    }

    #[test]
    fn unknown_model_labels_the_fallback() -> Result<()> {
        let mut output = Vec::new();
        report("안녕하세요", Some("unknown"), &mut output)?;
        assert_eq!(
            String::from_utf8(output)?,
            "4 tokens (o200k fallback estimate, model \"unknown\")\n"
        );
        Ok(())
    }
}
