//! `provider failover`: the failover order the current configuration produces.
//!
//! Read-only. It reads configuration, sends no model request and changes no
//! stored account, quota or provider selection, and prints no credential. Quota
//! cooldowns and per-account state live in a running daemon's memory, so they
//! are not part of this preview.
use anyhow::Result;
use serde::Serialize;

use crate::config::{CrossProviderFailoverMode, ProviderConfig};

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(super) struct Step {
    pub(super) order: u8,
    pub(super) name: &'static str,
    pub(super) active: bool,
    /// Why the step cannot run, or the condition that limits it.
    pub(super) reason: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub(super) struct Preview {
    pub(super) retry_attempts: u32,
    pub(super) steps: Vec<Step>,
    pub(super) not_covered: &'static str,
}

pub(super) fn build(provider: &ProviderConfig) -> Preview {
    let account = Step {
        order: 1,
        name: "same_provider_account",
        active: provider.same_provider_account_failover,
        reason: if provider.same_provider_account_failover {
            "tries another saved account of the same provider before leaving it".to_owned()
        } else {
            "excluded: provider.same_provider_account_failover is false".to_owned()
        },
    };
    let (cross_active, cross_reason) = match provider.cross_provider_failover {
        CrossProviderFailoverMode::Manual => (
            false,
            "excluded: provider.cross_provider_failover is manual, so a prompt is never resent to another provider on its own"
                .to_owned(),
        ),
        CrossProviderFailoverMode::Countdown => (
            true,
            "attended sessions show a cancelable countdown before resending the prompt to another provider".to_owned(),
        ),
    };
    let cross = Step {
        order: 2,
        name: "cross_provider",
        active: cross_active,
        reason: cross_reason,
    };
    let unattended = if !cross_active {
        (false, "excluded: cross-provider failover is off".to_owned())
    } else if !provider.autonomous_failover {
        (
            false,
            "excluded: provider.autonomous_failover is false, so an unattended run stops with the error".to_owned(),
        )
    } else if provider.autonomous_failover_max_attempts == 0 {
        (
            false,
            "excluded: provider.autonomous_failover_max_attempts is 0".to_owned(),
        )
    } else {
        (
            true,
            format!(
                "unattended runs switch routes on their own, at most {} per chain",
                provider.autonomous_failover_max_attempts
            ),
        )
    };
    let unattended = Step {
        order: 3,
        name: "unattended_switch",
        active: unattended.0,
        reason: unattended.1,
    };
    Preview {
        retry_attempts: provider.max_retries,
        steps: vec![account, cross, unattended],
        not_covered: "Quota cooldowns and which saved accounts are currently available are held by a running daemon, so this preview does not show them.",
    }
}

pub(super) fn render(preview: &Preview) -> String {
    let mut text = format!(
        "Retries on a transient error come first: up to {} attempts including the first.\n\nThen, in this order:\n",
        preview.retry_attempts
    );
    for step in &preview.steps {
        let state = if step.active { "on " } else { "off" };
        text.push_str(&format!("{}. [{state}] {}: {}\n", step.order, step.name, step.reason));
    }
    text.push_str(&format!("\n{}\n", preview.not_covered));
    text
}

pub(crate) fn run_provider_failover_command(json: bool) -> Result<()> {
    let preview = build(&crate::config::config().provider);
    if json {
        println!("{}", serde_json::to_string_pretty(&preview)?);
    } else {
        print!("{}", render(&preview));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn states(provider: &ProviderConfig) -> Vec<(&'static str, bool)> {
        build(provider)
            .steps
            .iter()
            .map(|step| (step.name, step.active))
            .collect()
    }

    #[test]
    fn the_defaults_switch_accounts_and_providers_for_attended_sessions_only() {
        assert_eq!(
            states(&ProviderConfig::default()),
            [
                ("same_provider_account", true),
                ("cross_provider", true),
                ("unattended_switch", false)
            ]
        );
        let preview = build(&ProviderConfig::default());
        assert_eq!(preview.retry_attempts, 8);
        assert!(preview.steps[2].reason.contains("autonomous_failover is false"));
    }

    #[test]
    fn each_disabled_step_names_the_setting_that_excludes_it() {
        let provider = ProviderConfig {
            same_provider_account_failover: false,
            cross_provider_failover: CrossProviderFailoverMode::Manual,
            autonomous_failover: true,
            ..ProviderConfig::default()
        };
        let preview = build(&provider);
        assert_eq!(
            states(&provider),
            [
                ("same_provider_account", false),
                ("cross_provider", false),
                ("unattended_switch", false)
            ]
        );
        assert!(
            preview.steps[0]
                .reason
                .contains("same_provider_account_failover is false")
        );
        assert!(preview.steps[1].reason.contains("cross_provider_failover is manual"));
        assert!(preview.steps[2].reason.contains("cross-provider failover is off"));
    }

    #[test]
    fn the_unattended_switch_reports_its_attempt_budget_or_a_zero_budget() {
        let enabled = ProviderConfig {
            autonomous_failover: true,
            ..ProviderConfig::default()
        };
        assert!(build(&enabled).steps[2].reason.ends_with("at most 3 per chain"));
        let zero = ProviderConfig {
            autonomous_failover_max_attempts: 0,
            ..enabled
        };
        let step = &build(&zero).steps[2];
        assert!(!step.active && step.reason.contains("max_attempts is 0"));
    }
}
