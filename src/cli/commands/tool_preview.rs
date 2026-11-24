use anyhow::{Context, Result};

/// `horon preview <tool> '<json input>'`: run one read-only built-in without a model or session.
pub(crate) async fn run_preview_command(tool: &str, input: &str, json: bool) -> Result<()> {
    let input: serde_json::Value = serde_json::from_str(input).context("input must be a JSON object")?;
    anyhow::ensure!(input.is_object(), "input must be a JSON object");
    let preview = crate::tool::preview::run(tool, input, std::env::current_dir()?).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&preview)?);
    } else {
        if let Some(backend) = &preview.backend {
            eprintln!("backend: {backend}");
        }
        print!("{}", preview.output);
        if preview.truncated {
            eprintln!(
                "\n[preview truncated at {} bytes]",
                crate::tool::preview::PREVIEW_MAX_BYTES
            );
        }
    }
    Ok(())
}
