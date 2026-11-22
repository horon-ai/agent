//! Execution-scoped spend warning and ceiling for `horon-agent run`.
//!
//! The estimate comes from each response's reported token usage and the pricing
//! catalog. A route the catalog cannot price is counted apart and never charged
//! as zero, so a cap cannot be satisfied by an unpriced model. The cap gates
//! admission only: a request already sent settles and is counted, so spend can
//! overshoot the ceiling by what was in flight. The ledger is process-wide, so
//! child agents this process owns are charged to the same run.

use anyhow::{Result, bail};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    pub warn_usd: Option<f64>,
    pub max_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub limits: Limits,
    pub spent_usd: f64,
    pub priced_requests: u64,
    pub unpriced_requests: u64,
    pub warned: bool,
    pub stopped: bool,
}

#[derive(Debug)]
struct Ledger {
    limits: Limits,
    spent_usd: f64,
    priced_requests: u64,
    unpriced_requests: u64,
    warned: bool,
    stopped: bool,
}

static LEDGER: Mutex<Option<Ledger>> = Mutex::new(None);

fn ledger() -> std::sync::MutexGuard<'static, Option<Ledger>> {
    LEDGER.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Start counting spend for this process. Call once, before the first request.
pub fn install(limits: Limits) {
    *ledger() = Some(Ledger {
        limits,
        spent_usd: 0.0,
        priced_requests: 0,
        unpriced_requests: 0,
        warned: false,
        stopped: false,
    });
}

/// Refuse a new model request once the ceiling is reached. No-op when no cap is installed.
pub(super) fn admit() -> Result<()> {
    let mut guard = ledger();
    let Some(ledger) = guard.as_mut() else {
        return Ok(());
    };
    if let Some(max) = ledger.limits.max_usd
        && ledger.spent_usd >= max
    {
        ledger.stopped = true;
        bail!(
            "spend cap of ${max:.4} reached (about ${:.4} estimated); no new model request was sent",
            ledger.spent_usd
        );
    }
    Ok(())
}

/// Count one settled response. `cost_usd` is `None` when the route has no price.
pub(super) fn settle(model: &str, cost_usd: Option<f64>) {
    let mut guard = ledger();
    let Some(ledger) = guard.as_mut() else {
        return;
    };
    match cost_usd {
        Some(cost) => {
            ledger.spent_usd += cost;
            ledger.priced_requests += 1;
        }
        None => {
            if ledger.unpriced_requests == 0 {
                eprintln!(
                    "spend cap: no price is known for {model}; its requests are counted separately and are not limited"
                );
            }
            ledger.unpriced_requests += 1;
        }
    }
    if let Some(warn) = ledger.limits.warn_usd
        && !ledger.warned
        && ledger.spent_usd >= warn
    {
        ledger.warned = true;
        eprintln!(
            "spend warning: about ${:.4} estimated, at or above the ${warn:.4} warning level",
            ledger.spent_usd
        );
    }
}

/// What the run spent so far, or `None` when no cap was installed.
pub fn summary() -> Option<Summary> {
    ledger().as_ref().map(|ledger| Summary {
        limits: ledger.limits,
        spent_usd: ledger.spent_usd,
        priced_requests: ledger.priced_requests,
        unpriced_requests: ledger.unpriced_requests,
        warned: ledger.warned,
        stopped: ledger.stopped,
    })
}

impl super::Agent {
    /// Charge the response just settled to the run's spend ledger.
    pub(super) fn settle_spend(&self) {
        let usage = &self.last_usage;
        let model = self.provider_model();
        // A direct profile prices under its own `openai-compatible:<id>` route;
        // the display label would map a compatible endpoint to OpenAI's table.
        let source_key = self.compaction_route_from_provider().unwrap_or_else(|| {
            crate::provider_activity::source_key_for_provider_label(
                &self.provider_name(),
                std::env::var("HORON_AGENT_RUNTIME_PROVIDER").ok().as_deref(),
            )
        });
        settle(
            &model,
            crate::provider::pricing::metered_usage_cost_usd(
                &source_key,
                &model,
                usage.input_tokens,
                usage.output_tokens,
                usage.cache_read_input_tokens.unwrap_or(0),
                usage.cache_creation_input_tokens.unwrap_or(0),
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // One process-wide ledger: tests share it, so they run as one sequence.
    #[test]
    fn warns_once_stops_admission_at_the_cap_and_never_prices_unknown_as_zero() {
        install(Limits {
            warn_usd: Some(0.5),
            max_usd: Some(1.0),
        });
        assert!(admit().is_ok());
        settle("m", Some(0.6));
        assert!(summary().unwrap().spent_usd > 0.59);
        assert!(admit().is_ok(), "below the ceiling");
        settle("m", None);
        assert!(admit().is_ok(), "an unpriced response adds nothing");
        settle("m", Some(0.5));
        let error = admit().unwrap_err().to_string();
        assert!(error.contains("spend cap of $1.0000 reached"), "{error}");
        let summary = summary().unwrap();
        assert_eq!((summary.priced_requests, summary.unpriced_requests), (2, 1));
        assert!(summary.warned && summary.stopped);
    }
}
