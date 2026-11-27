//! The model's average output speed, for the `tokens` segment.
//!
//! A provider call is timed from the first token it streams (text, reasoning
//! or a tool call's arguments) to the usage report that ends it, so the wait
//! before the first token and the time tools run are not counted: the figure
//! is how fast the model writes once it has started.

use std::time::Instant;

use horon_agent_sdk::api::ApiEvent;

/// Shortest generation time an average is shown for; below it the figure is
/// noise from one tiny response.
const MIN_MS: u64 = 200;

/// Output tokens and the time spent streaming them, over the session.
#[derive(Debug, Clone, Default)]
pub struct Throughput {
    tokens: u64,
    ms: u64,
    since: Option<Instant>,
}

impl Throughput {
    /// Follow one event: a streamed token starts the clock, a usage report
    /// stops it and adds the call's output.
    pub fn observe(&mut self, event: &ApiEvent) {
        match event {
            ApiEvent::TextDelta { .. } | ApiEvent::ReasoningDelta { .. } | ApiEvent::ToolInputDelta { .. } => {
                self.since.get_or_insert_with(Instant::now);
            }
            ApiEvent::TokenUsage { output, .. } => {
                if let Some(started) = self.since.take() {
                    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    self.add(*output, ms);
                }
            }
            _ => {}
        }
    }

    /// Count `tokens` written over `ms` milliseconds of streaming.
    pub fn add(&mut self, tokens: u64, ms: u64) {
        if tokens > 0 && ms > 0 {
            self.tokens = self.tokens.saturating_add(tokens);
            self.ms = self.ms.saturating_add(ms);
        }
    }

    /// Average output tokens per second, once enough has been timed.
    pub fn per_second(&self) -> Option<u64> {
        (self.ms >= MIN_MS).then(|| self.tokens.saturating_mul(1000) / self.ms)
    }
}
