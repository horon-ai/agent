//! Time and provider usage for the turn started by this terminal.

use super::{Instant, Item, Transcript, Turn, TurnMetrics};

impl Transcript {
    /// Keep, or stop keeping, a finished turn's time and token usage.
    ///
    /// Read when the turn ends, so a turn already running takes the new value
    /// and a turn that already ended keeps the footer it was drawn with.
    pub fn set_turn_usage(&mut self, show: bool) {
        self.turn_usage = show;
    }

    /// Start measuring the turn the client is about to send.
    ///
    /// A turn already being measured is replaced: one message is in flight at a
    /// time, so the only way to reach this twice without an end is a send that
    /// never produced a turn.
    pub fn begin_turn(&mut self) {
        self.turn = Some(Turn {
            started: Instant::now(),
            input: 0,
            output: 0,
            reported: false,
        });
    }

    /// Add one provider call's usage to the turn being measured.
    ///
    /// Counts are added with saturation and never subtracted, so a provider
    /// that reports a smaller number than an earlier call, or a counter a
    /// reconnect restarted, cannot make the turn's usage negative. A call that
    /// reported no usage adds nothing; a turn with no calls at all keeps its
    /// token field empty.
    pub fn note_usage(&mut self, input: u64, output: u64) {
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        turn.input = turn.input.saturating_add(input);
        turn.output = turn.output.saturating_add(output);
        turn.reported = true;
    }

    /// Close the turn being measured and draw what it cost under it.
    ///
    /// Nothing is drawn for a turn this client did not start, or while
    /// `transcript.turn_usage` is off. The elapsed time is taken once here and
    /// stored with the item, so drawing it later reads no clock.
    pub fn end_turn(&mut self) {
        let Some(turn) = self.turn.take() else {
            return;
        };
        if !self.turn_usage {
            return;
        }
        // The answer is committed before the footer, so the footer reads as the
        // last line of the turn rather than sitting above streaming text.
        self.finish_stream();
        let elapsed_ms = u64::try_from(turn.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.items.push(Item::TurnUsage(TurnMetrics {
            elapsed_ms,
            tokens: turn.reported.then_some((turn.input, turn.output)),
        }));
    }

    /// Stop measuring the turn without drawing anything.
    ///
    /// Used when there is no completed turn to describe: the send never
    /// reached the harness, the connection died, or the session changed.
    pub fn discard_turn(&mut self) {
        self.turn = None;
    }
}
