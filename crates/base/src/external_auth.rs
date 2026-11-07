//! External-auth-source review and auto-import flow.
//!
//! Discovers credentials left behind by other tools (Claude Code, Codex,
//! Copilot, Cursor, Gemini CLI, ...), asks the user to approve trusting them,
//! and imports approved sources. This is provider/auth domain logic that
//! depends only on core modules (`auth`, `config`, `provider`,
//! `provider_catalog`), so it lives in the core layer and can be driven by
//! both the CLI login flow and the TUI auth UI.

use anyhow::Result;
use std::io::{self, IsTerminal, Write};

use crate::auth;

pub fn can_prompt_for_external_auth() -> bool {
    std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
        && std::env::var("HORON_AGENT_NON_INTERACTIVE").is_err()
}

pub fn external_auth_blocked_message(
    provider_name: &str,
    source_name: &str,
    path: &std::path::Path,
    login_hint: &str,
) -> String {
    format!(
        "Found existing {} credentials from {} at {} but horon-agent will not read them without confirmation. Re-run in an interactive terminal to approve this auth source for future horon-agent sessions, or run `{}`.",
        provider_name,
        source_name,
        path.display(),
        login_hint
    )
}

pub fn prompt_to_trust_external_auth(provider_name: &str, source_name: &str, path: &std::path::Path) -> Result<bool> {
    eprintln!();
    eprintln!(
        "Found existing {} credentials from {} at {}.",
        provider_name,
        source_name,
        path.display()
    );
    eprintln!("horon-agent will only read that source in place after you approve it.");
    eprintln!("It will not move, delete, or rewrite the original auth there.");
    eprint!("Trust this auth source for future horon-agent sessions? [y/N]: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExternalAuthReviewAction {
    SharedExternal(auth::external::ExternalAuthSource),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalAuthReviewCandidate {
    pub(crate) provider_summary: String,
    pub(crate) source_name: String,
    pub(crate) path: std::path::PathBuf,
    action: ExternalAuthReviewAction,
}

// Read-only accessors. Kept available outside tests so onboarding/UI can
// summarize detected import candidates (e.g. for the first-run welcome card).
impl ExternalAuthReviewCandidate {
    pub fn provider_summary(&self) -> &str {
        &self.provider_summary
    }

    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// Canonical HoronAgent provider ids (`claude`, `openai`, ...) this source would
    /// sign in, matching `horon-agent auth status --json` ids. Lets a UI tell which
    /// detected logins HoronAgent already has.
    pub fn provider_ids(&self) -> Vec<&'static str> {
        self.provider_summary
            .split(',')
            .filter_map(|label| provider_id_for_label(label.trim()))
            .collect()
    }

    /// Canonical provider ids this candidate activates on a successful import.
    /// Unknown display labels are skipped.
    pub fn activated_provider_ids(&self) -> Vec<&'static str> {
        match &self.action {
            ExternalAuthReviewAction::SharedExternal(_) => {
                self.provider_ids().into_iter().filter(|id| *id != "cursor").collect()
            }
        }
    }

    /// Where the detected credential lives, for display only.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Build a synthetic candidate for tests / UI fixtures. The candidate
    /// points at a shared external source so it can be summarized and rendered,
    /// but is not expected to actually import successfully.
    #[doc(hidden)]
    pub fn fixture(provider_summary: impl Into<String>, source_name: impl Into<String>) -> Self {
        Self {
            provider_summary: provider_summary.into(),
            source_name: source_name.into(),
            path: std::path::PathBuf::from("/dev/null"),
            action: ExternalAuthReviewAction::SharedExternal(auth::external::ExternalAuthSource::OpenCode),
        }
    }
}

fn provider_id_for_label(label: &str) -> Option<&'static str> {
    match label {
        "Cursor" => Some("cursor"),
        "OpenAI/Codex" => Some("openai"),
        "Claude" => Some("claude"),
        "Gemini" => Some("gemini"),
        "Antigravity" => Some("antigravity"),
        "GitHub Copilot" => Some("copilot"),
        "OpenRouter/API-key providers" => Some("openrouter"),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalAuthAutoImportOutcome {
    pub imported: usize,
    pub messages: Vec<String>,
    /// Canonical provider ids each successfully imported source activates. May
    /// contain more entries than `imported` when one source carries multiple
    /// providers.
    pub imported_providers: Vec<&'static str>,
}

impl ExternalAuthAutoImportOutcome {
    /// Provider to activate after a successful multi-source import. This mirrors
    /// horon-agent's global provider preference: Anthropic first, then OpenAI, followed
    /// by the remaining supported providers. The precise OAuth/API-key variant
    /// is resolved from `AuthStatus` by the caller when possible.
    pub fn preferred_activation_provider(&self) -> Option<&'static str> {
        const ORDER: &[&str] = &[
            "claude",
            "openai",
            "copilot",
            "gemini",
            "cursor",
            "antigravity",
            "openrouter",
        ];
        ORDER
            .iter()
            .copied()
            .find(|provider| self.imported_providers.contains(provider))
    }

    pub fn render_markdown(&self) -> String {
        if self.messages.is_empty() {
            return "No external logins were imported.".to_string();
        }

        // Messages are tagged with a leading "✓"/"✕" marker by the importer.
        let imported: Vec<&String> = self.messages.iter().filter(|m| m.starts_with('✓')).collect();
        let skipped: Vec<&String> = self.messages.iter().filter(|m| m.starts_with('✕')).collect();

        let mut out = String::from("**Logins imported**\n");
        out.push('\n');
        if imported.is_empty() {
            out.push_str("No logins could be imported.");
        } else {
            out.push_str(&format!(
                "Reusing {} existing login{}:",
                imported.len(),
                if imported.len() == 1 { "" } else { "s" }
            ));
        }
        for line in &imported {
            out.push_str("\n- ");
            out.push_str(line.trim_start_matches('✓').trim());
        }

        if !skipped.is_empty() {
            out.push_str(&format!(
                "\n\nSkipped {} source{}:",
                skipped.len(),
                if skipped.len() == 1 { "" } else { "s" }
            ));
            for line in &skipped {
                out.push_str("\n- ");
                out.push_str(line.trim_start_matches('✕').trim());
            }
        }

        out
    }
}

/// Logins from other tools that horon-agent could import and has not been told to
/// trust yet. This is what first-run onboarding offers.
pub fn pending_external_auth_review_candidates() -> Result<Vec<ExternalAuthReviewCandidate>> {
    Ok(detected_external_auth_review_candidates()?
        .into_iter()
        .filter(|candidate| !external_auth_review_candidate_trusted(candidate))
        .collect())
}

/// Every importable login from another tool on this machine, whether or not
/// horon-agent already trusts it. This is what a brand-new horon-agent install would find,
/// so onboarding rehearsals use it to show the real first-run screen on a
/// machine where horon-agent is already set up. Importing an already-trusted
/// candidate is a no-op, so offering these is safe.
pub fn detected_external_auth_review_candidates() -> Result<Vec<ExternalAuthReviewCandidate>> {
    let mut candidates = Vec::new();

    for source in auth::external::detected_sources() {
        let provider_summary = auth::external::source_provider_labels(source).join(", ");
        if provider_summary.is_empty() {
            continue;
        }
        candidates.push(ExternalAuthReviewCandidate {
            provider_summary,
            source_name: source.display_name().to_string(),
            path: source.path()?,
            action: ExternalAuthReviewAction::SharedExternal(source),
        });
    }

    Ok(candidates)
}

pub fn parse_external_auth_review_selection(input: &str, count: usize) -> Result<Vec<usize>> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if matches!(trimmed.to_ascii_lowercase().as_str(), "a" | "all") {
        return Ok((0..count).collect());
    }

    let mut selected = Vec::new();
    for part in trimmed.split(',') {
        let value = part.trim();
        if value.is_empty() {
            continue;
        }
        let index: usize = value
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid selection '{}'. Enter numbers like 1,3 or 'a' for all.", value))?;
        if index == 0 || index > count {
            anyhow::bail!(
                "Selection '{}' is out of range. Enter 1-{} or 'a' for all.",
                index,
                count
            );
        }
        let zero_based = index - 1;
        if !selected.contains(&zero_based) {
            selected.push(zero_based);
        }
    }
    Ok(selected)
}

fn prompt_to_review_external_auth_sources(candidates: &[ExternalAuthReviewCandidate]) -> Result<Vec<usize>> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    eprintln!();
    eprintln!("Found existing logins that horon-agent can reuse.");
    eprintln!("Nothing has been imported yet.");
    eprintln!("Approve the sources you want horon-agent to read in place; rejected sources stay untouched.");
    eprintln!();

    for (index, candidate) in candidates.iter().enumerate() {
        eprintln!(
            "  {}. {:<22} via {}",
            index + 1,
            candidate.provider_summary,
            candidate.source_name
        );
        eprintln!("     {}", candidate.path.display());
    }

    eprintln!();
    eprint!("Approve sources [a=all, Enter=skip, example: 1,3]: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    parse_external_auth_review_selection(&input, candidates.len())
}

/// Whether horon-agent already trusts this candidate's source. Imports use this to
/// stay idempotent: re-running onboarding (or importing again) must never undo
/// or duplicate a decision the user already made.
fn external_auth_review_candidate_trusted(candidate: &ExternalAuthReviewCandidate) -> bool {
    match candidate.action {
        ExternalAuthReviewAction::SharedExternal(source) => auth::external::source_allowed(source),
    }
}

fn approve_external_auth_review_candidate(
    candidate: &ExternalAuthReviewCandidate,
    already_trusted: bool,
) -> Result<()> {
    if already_trusted {
        return Ok(());
    }
    match candidate.action {
        ExternalAuthReviewAction::SharedExternal(source) => auth::external::trust_external_auth_source(source)?,
    }
    Ok(())
}

fn revoke_external_auth_review_candidate(candidate: &ExternalAuthReviewCandidate) -> Result<()> {
    match candidate.action {
        ExternalAuthReviewAction::SharedExternal(source) => {
            crate::config::Config::revoke_external_auth_source_for_path(source.source_id(), &candidate.path)?
        }
    }
    Ok(())
}

// Import validation is intentionally *non-destructive*: it only checks that
// reusable credentials are present after trusting the source. It must NOT
// perform a live OAuth refresh, because OAuth refresh tokens are single-use --
// refreshing here would rotate (and thus burn) the source's refresh token and
// then discard the rotated result, breaking both horon-agent and the original tool.
// Expired tokens are still imported: they get refreshed lazily (and persisted)
// at request time, or the user is prompted to /login.

fn validate_openrouter_like_import() -> Result<String> {
    for (env_key, env_file) in crate::provider_catalog::openrouter_like_api_key_sources() {
        if crate::provider_catalog::load_api_key_from_env_or_config(&env_key, &env_file).is_some() {
            return Ok(format!("Loaded API key for `{}`.", env_key));
        }
    }
    anyhow::bail!("No reusable API key became available after import.")
}

async fn validate_shared_external_import(source: auth::external::ExternalAuthSource) -> Result<String> {
    let mut errors = Vec::new();
    for label in auth::external::source_provider_labels(source) {
        let result = match label {
            "OpenAI-compatible API-key providers" => validate_openrouter_like_import(),
            _ => continue,
        };
        match result {
            Ok(detail) => return Ok(detail),
            Err(err) => errors.push(format!("{}: {}", label, err)),
        }
    }
    anyhow::bail!(errors.join("; "))
}

async fn validate_external_auth_review_candidate(candidate: &ExternalAuthReviewCandidate) -> Result<String> {
    match candidate.action {
        ExternalAuthReviewAction::SharedExternal(source) => validate_shared_external_import(source).await,
    }
}

pub async fn maybe_run_external_auth_auto_import_flow() -> Result<Option<usize>> {
    if !can_prompt_for_external_auth() {
        return Ok(None);
    }

    let candidates = pending_external_auth_review_candidates()?;
    if candidates.is_empty() {
        return Ok(None);
    }

    let selected = prompt_to_review_external_auth_sources(&candidates)?;
    let outcome = run_external_auth_auto_import_candidates(&candidates, &selected).await?;
    for line in &outcome.messages {
        eprintln!("{}", line);
    }
    auth::AuthStatus::invalidate_cache();
    Ok(Some(outcome.imported))
}

pub fn format_external_auth_review_candidates_markdown(candidates: &[ExternalAuthReviewCandidate]) -> String {
    let mut message = String::from(
        "**Auto Import Existing Logins**\n\nFound existing logins that horon-agent can reuse. Nothing has been imported yet.\n\nReply with `a` to approve all, `1,3` to approve specific sources, or `/cancel` to abort.\n",
    );
    for (index, candidate) in candidates.iter().enumerate() {
        message.push_str(&format!(
            "\n{}. **{}** via {}\n   - `{}`\n",
            index + 1,
            candidate.provider_summary,
            candidate.source_name,
            candidate.path.display()
        ));
    }
    message
}

pub async fn run_external_auth_auto_import_candidates(
    candidates: &[ExternalAuthReviewCandidate],
    selected: &[usize],
) -> Result<ExternalAuthAutoImportOutcome> {
    let mut outcome = ExternalAuthAutoImportOutcome {
        imported: 0,
        messages: Vec::new(),
        imported_providers: Vec::new(),
    };

    for &index in selected {
        let Some(candidate) = candidates.get(index) else {
            continue;
        };
        let already_trusted = external_auth_review_candidate_trusted(candidate);
        approve_external_auth_review_candidate(candidate, already_trusted)?;
        match validate_external_auth_review_candidate(candidate).await {
            Ok(detail) => {
                outcome.imported += 1;
                outcome.imported_providers.extend(candidate.activated_provider_ids());
                outcome.messages.push(format!(
                    "✓ {} (from {}): {}",
                    candidate.provider_summary, candidate.source_name, detail
                ));
            }
            Err(err) => {
                // Only undo trust this import added. Revoking a source the user
                // trusted before would silently sign them out of it.
                if !already_trusted {
                    let _ = revoke_external_auth_review_candidate(candidate);
                }
                outcome.messages.push(format!(
                    "✕ {} (from {}): {}",
                    candidate.provider_summary, candidate.source_name, err
                ));
            }
        }
    }

    auth::AuthStatus::invalidate_cache();
    Ok(outcome)
}

/// Whether HoronAgent already has a usable login for a canonical provider id, as
/// returned by [`ExternalAuthReviewCandidate::provider_ids`].
pub fn provider_connected_in_horon_agent(status: &auth::AuthStatus, provider_id: &str) -> bool {
    provider_id == "openai" && status.openai_compatible_any == auth::AuthState::Available
}

/// Like [`run_external_auth_auto_import_candidates`], but never lets an
/// imported source shadow a login HoronAgent already has. Connection state is
/// re-read at import time, and any selected source that would touch an
/// already connected provider is left alone. Those count as usable (the user
/// asked for that login and HoronAgent has it), so re-running onboarding on a set-up
/// install succeeds without asking the user to sign in again.
pub async fn run_external_auth_import_candidates_preserving_existing(
    candidates: &[ExternalAuthReviewCandidate],
    selected: &[usize],
) -> Result<ExternalAuthAutoImportOutcome> {
    auth::AuthStatus::invalidate_cache();
    let status = auth::AuthStatus::check();
    let mut kept = Vec::new();
    let mut already_connected = Vec::new();
    for &index in selected {
        let Some(candidate) = candidates.get(index) else {
            continue;
        };
        if candidate
            .provider_ids()
            .iter()
            .any(|id| provider_connected_in_horon_agent(&status, id))
        {
            already_connected.push(format!(
                "✓ {} (from {}): already connected in horon-agent, kept the existing login",
                candidate.provider_summary, candidate.source_name
            ));
        } else {
            kept.push(index);
        }
    }
    let mut outcome = run_external_auth_auto_import_candidates(candidates, &kept).await?;
    outcome.imported += already_connected.len();
    outcome.messages.extend(already_connected);
    Ok(outcome)
}

#[cfg(test)]
mod render_markdown_tests {
    use super::ExternalAuthAutoImportOutcome;

    #[test]
    fn empty_outcome_reports_nothing_imported() {
        let outcome = ExternalAuthAutoImportOutcome {
            imported: 0,
            messages: Vec::new(),
            imported_providers: Vec::new(),
        };
        assert_eq!(outcome.render_markdown(), "No external logins were imported.");
    }

    #[test]
    fn groups_imported_and_skipped_with_counts() {
        let outcome = ExternalAuthAutoImportOutcome {
            imported: 2,
            messages: vec![
                "✓ OpenAI/Codex (from Codex auth.json): Loaded OpenAI OAuth credentials.".to_string(),
                "✓ Claude (from Claude Code): Loaded Claude credentials.".to_string(),
                "✕ Cursor (from Cursor native): no usable auth token.".to_string(),
            ],
            imported_providers: Vec::new(),
        };
        let md = outcome.render_markdown();
        assert!(md.starts_with("**Logins imported**"), "got: {md}");
        assert!(md.contains("Reusing 2 existing logins:"), "got: {md}");
        assert!(
            md.contains("- OpenAI/Codex (from Codex auth.json): Loaded OpenAI OAuth credentials."),
            "got: {md}"
        );
        assert!(md.contains("Skipped 1 source:"), "got: {md}");
        assert!(
            md.contains("- Cursor (from Cursor native): no usable auth token."),
            "got: {md}"
        );
        // Markers themselves should be stripped from the rendered list.
        assert!(!md.contains('✓'), "got: {md}");
        assert!(!md.contains('✕'), "got: {md}");
    }

    #[test]
    fn singular_wording_for_one_login() {
        let outcome = ExternalAuthAutoImportOutcome {
            imported: 1,
            messages: vec!["✓ Gemini (from Gemini CLI): Loaded Gemini credentials.".to_string()],
            imported_providers: Vec::new(),
        };
        let md = outcome.render_markdown();
        assert!(md.contains("Reusing 1 existing login:"), "got: {md}");
    }

    #[test]
    fn preferred_activation_provider_is_deterministic_for_multi_import() {
        let outcome = ExternalAuthAutoImportOutcome {
            imported: 3,
            messages: Vec::new(),
            imported_providers: vec!["gemini", "claude", "openai"],
        };
        assert_eq!(outcome.preferred_activation_provider(), Some("claude"));

        let anthropic_only = ExternalAuthAutoImportOutcome {
            imported: 1,
            messages: Vec::new(),
            imported_providers: vec!["claude"],
        };
        assert_eq!(anthropic_only.preferred_activation_provider(), Some("claude"));
    }

    #[test]
    fn provider_ids_match_auth_status_ids() {
        use super::ExternalAuthReviewCandidate;
        let candidate =
            ExternalAuthReviewCandidate::fixture("OpenAI/Codex, Claude, OpenRouter/API-key providers", "OpenCode");
        assert_eq!(candidate.provider_ids(), vec!["openai", "claude", "openrouter"]);
        let cursor = ExternalAuthReviewCandidate::fixture("Cursor", "Cursor");
        assert_eq!(cursor.provider_ids(), vec!["cursor"]);
        assert!(
            ExternalAuthReviewCandidate::fixture("Unknown", "x")
                .provider_ids()
                .is_empty()
        );
    }
}
