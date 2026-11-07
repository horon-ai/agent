//! Text truncation shared by the import converters.

pub(super) fn truncate_import_text(text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let suffix = format!("\n[... {} bytes omitted from imported history]", text.len() - max_bytes);
    let prefix_budget = max_bytes.saturating_sub(suffix.len());
    format!(
        "{}{}",
        horon_agent_core::util::truncate_str(&text, prefix_budget),
        suffix
    )
}
