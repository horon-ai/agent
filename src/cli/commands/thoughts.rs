//! Text-mode `run --print-thoughts`: provider-supplied visible reasoning goes
//! to stderr under a label, the answer stays on stdout.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;

use crate::agent::Agent;
use crate::protocol::ServerEvent;

/// Run one plain-text turn. Without `print_thoughts` this is exactly
/// `Agent::run_once`, so default output is unchanged.
pub(super) async fn run_plain_turn(
    agent: &mut Agent,
    message: &str,
    print_thoughts: bool,
    deadline_elapsed: &AtomicBool,
) -> Result<()> {
    if !print_thoughts {
        return agent.run_once(message).await;
    }
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut printer = Printer::new(std::io::stdout(), std::io::stderr());
    let mut run = std::pin::pin!(agent.run_once_streaming_mpsc(message, Vec::new(), None, event_tx.clone()));
    let result = loop {
        tokio::select! {
            result = &mut run => break result,
            Some(event) = event_rx.recv() => {
                if !deadline_elapsed.load(Ordering::Relaxed) {
                    printer.event(event)?;
                }
            }
        }
    };
    while let Ok(event) = event_rx.try_recv() {
        if !deadline_elapsed.load(Ordering::Relaxed) {
            printer.event(event)?;
        }
    }
    if !deadline_elapsed.load(Ordering::Relaxed) {
        printer.finish()?;
    }
    result
}

struct Printer<O, E> {
    out: O,
    err: E,
    in_thought: bool,
    ends_with_newline: bool,
    wrote_text: bool,
}

impl<O: Write, E: Write> Printer<O, E> {
    fn new(out: O, err: E) -> Self {
        Self {
            out,
            err,
            in_thought: false,
            ends_with_newline: true,
            wrote_text: false,
        }
    }

    fn event(&mut self, event: ServerEvent) -> Result<()> {
        match event {
            ServerEvent::ReasoningDelta { text } => {
                if !self.in_thought {
                    writeln!(self.err, "\n[thinking]")?;
                    self.in_thought = true;
                }
                write!(self.err, "{text}")?;
                self.err.flush()?;
            }
            ServerEvent::TextDelta { text } => {
                self.close_thought()?;
                self.wrote_text = true;
                self.ends_with_newline = text.ends_with('\n');
                write!(self.out, "{text}")?;
                self.out.flush()?;
            }
            _ => {}
        }
        Ok(())
    }

    fn close_thought(&mut self) -> Result<()> {
        if self.in_thought {
            writeln!(self.err, "\n[/thinking]")?;
            self.in_thought = false;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.close_thought()?;
        if self.wrote_text && !self.ends_with_newline {
            writeln!(self.out)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(events: Vec<ServerEvent>) -> (String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let mut printer = Printer::new(&mut out, &mut err);
        for event in events {
            printer.event(event).unwrap();
        }
        printer.finish().unwrap();
        (String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    fn reasoning(text: &str) -> ServerEvent {
        ServerEvent::ReasoningDelta { text: text.to_owned() }
    }

    fn text(text: &str) -> ServerEvent {
        ServerEvent::TextDelta { text: text.to_owned() }
    }

    #[test]
    fn reasoning_is_labelled_on_stderr_and_the_answer_stays_on_stdout() {
        let (out, err) = run(vec![reasoning("first "), reasoning("second"), text("answer")]);
        assert_eq!(out, "answer\n");
        assert_eq!(err, "\n[thinking]\nfirst second\n[/thinking]\n");
    }

    #[test]
    fn a_turn_without_reasoning_prints_no_label() {
        let (out, err) = run(vec![text("answer\n")]);
        assert_eq!((out.as_str(), err.as_str()), ("answer\n", ""));
    }
}
