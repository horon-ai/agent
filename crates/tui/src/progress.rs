//! Progress and program-status signals for the terminal.
//!
//! Two optional channels tell the terminal what the client is doing, so the tab
//! or taskbar carries it while the window is behind another one:
//!
//! * `OSC 9;4` sets the progress indicator a terminal draws in its tab and
//!   taskbar. A report with no number is the indeterminate spinner, which is
//!   what a turn with nothing counted reports.
//! * `OSC 7501` reports one program-status record (`working`, `blocked` with
//!   the kind of input it waits for, `done`, `error`), which a terminal that
//!   hosts other programs can read back.
//!
//! Both are opt-in ([`Options`]) and both reach only a terminal that names
//! itself as understanding the protocol ([`Environment`]): nothing in either
//! protocol lets a plain-output client ask first, and an emulator that does not
//! know the sequence prints its bytes as text or ignores them. A redirected
//! stdout writes a file. Unsupported terminals therefore receive no bytes at
//! all, and a terminal that was written to is put back on every way out, because
//! a record outlives the process that made it.
//!
//! [`Signals::sync`] runs once a frame and writes only what the terminal does not
//! already hold, so a held state costs nothing on the wire.

use std::io::{IsTerminal, Stdout, Write};
use std::sync::atomic::{AtomicU8, Ordering};

use crate::settings::Settings;

/// What a blocked client is waiting for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// An approval the user has to give.
    Permission,
    /// A question the user has to answer.
    Question,
}

impl Kind {
    /// The `kind` value of the OSC 7501 protocol.
    fn name(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
        }
    }
}

/// How the last turn ended, until the user acts on it.
///
/// A status line goes back to `idle` the moment a turn ends, which is right for
/// the person already at the screen; a terminal tab behind another window has no
/// other way to say a turn finished or failed, so both protocols have a state
/// for it and the mark stays until the user looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// The turn finished normally.
    Done,
    /// The turn stopped without finishing: a provider failure, a crash, a
    /// guardrail or a spent limit.
    Failed,
}

impl Outcome {
    /// The state the protocols name this outcome by.
    pub(crate) fn signal(self) -> Signal {
        match self {
            Self::Done => Signal::done(),
            Self::Failed => Signal::error(),
        }
    }
}

/// What the client is doing, as the two protocols name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    /// At rest, waiting for the user's next instruction.
    Idle,
    /// A turn is running.
    Working,
    /// A turn cannot continue until the user decides something.
    Blocked(Kind),
    /// A turn finished and the user has not acted on it yet.
    Done,
    /// The connection or the turn failed.
    Error,
}

/// One state, ready to be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Signal {
    pub(crate) state: State,
    /// Share of the counted work that is done, 0 to 100.
    ///
    /// Only a running turn carries one. Its absence is not "zero": it is the
    /// indeterminate state both protocols draw as a spinner, which is what a
    /// turn with no counted list has to report.
    pub(crate) percent: Option<u8>,
}

impl Signal {
    pub(crate) fn idle() -> Self {
        Self {
            state: State::Idle,
            percent: None,
        }
    }

    pub(crate) fn working(percent: Option<u8>) -> Self {
        Self {
            state: State::Working,
            percent: percent.map(|percent| percent.min(100)),
        }
    }

    pub(crate) fn blocked(kind: Kind) -> Self {
        Self {
            state: State::Blocked(kind),
            percent: None,
        }
    }

    pub(crate) fn done() -> Self {
        Self {
            state: State::Done,
            percent: Some(100),
        }
    }

    pub(crate) fn error() -> Self {
        Self {
            state: State::Error,
            percent: None,
        }
    }
}

/// Which channels the settings allow.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Options {
    /// `terminal.progress`: the tab and taskbar indicator.
    pub(crate) progress: bool,
    /// `terminal.program_status`: the OSC 7501 record.
    pub(crate) program_status: bool,
}

impl Options {
    /// The channels the resolved settings allow.
    pub(crate) fn from_settings(settings: &Settings) -> Self {
        Self {
            progress: settings.boolean("terminal.progress").unwrap_or(false),
            program_status: settings.boolean("terminal.program_status").unwrap_or(false),
        }
    }
}

/// Which protocols this terminal understands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Capabilities {
    pub(crate) progress: bool,
    pub(crate) program_status: bool,
}

/// The environment the client already sees, as far as the signals care.
#[derive(Debug, Default, Clone)]
pub(crate) struct Environment {
    /// `TERM`: `dumb` prints escape bytes as text, so it gets nothing.
    pub(crate) term: Option<String>,
    /// `TERM_PROGRAM`: set by an emulator that names itself.
    pub(crate) term_program: Option<String>,
    /// `WT_SESSION`: Windows Terminal, which reads `OSC 9;4`.
    pub(crate) wt_session: Option<String>,
    /// `ConEmuANSI`: ConEmu, which defines `OSC 9;4`.
    pub(crate) conemu_ansi: Option<String>,
    /// `HC_TERMINAL_ID`: herdr-core, which stores the `OSC 7501` record.
    pub(crate) hc_terminal: Option<String>,
}

impl Environment {
    /// The environment of this process.
    pub(crate) fn current() -> Self {
        let read = |name: &str| std::env::var_os(name).map(|value| value.to_string_lossy().into_owned());
        Self {
            term: read("TERM"),
            term_program: read("TERM_PROGRAM"),
            wt_session: read("WT_SESSION"),
            conemu_ansi: read("ConEmuANSI"),
            hc_terminal: read("HC_TERMINAL_ID"),
        }
    }

    /// The protocols this environment's terminal understands.
    pub(crate) fn capabilities(&self) -> Capabilities {
        // A terminal that prints escape bytes as text must never be written to,
        // whatever else it says about itself.
        if self.term.as_deref() == Some("dumb") {
            return Capabilities::default();
        }
        let program = self.term_program.as_deref();
        let named = |name: &str| program.is_some_and(|program| program.eq_ignore_ascii_case(name));
        // Emulators that read `OSC 9;4`. A terminal not named here is treated as
        // not understanding it: its bytes would be noise at best.
        let progress = ["ghostty", "wezterm", "iterm.app", "vscode"]
            .iter()
            .any(|name| named(name))
            || self.wt_session.is_some()
            || self.conemu_ansi.as_deref() == Some("ON");
        // herdr-core stores the record and Ghostty reads it as the emulator.
        let program_status = self.hc_terminal.is_some() || named("ghostty");
        Capabilities {
            progress,
            program_status,
        }
    }
}

/// `state=clear` removes every record the program wrote.
const STATUS_CLEAR: &str = "\x1b]7501;state=clear\x07";

/// `OSC 9;4` state that removes the indicator.
const BAR_REMOVE: u8 = 0;
/// Work in progress with a known share.
const BAR_SET: u8 = 1;
/// The work stopped because it failed.
const BAR_ERROR: u8 = 2;
/// Work in progress with no share to report.
const BAR_INDETERMINATE: u8 = 3;
/// The work is waiting on the user.
const BAR_PAUSED: u8 = 4;

/// The `state=` body of a record, with the app name a terminal groups records by.
pub(crate) fn status_sequence(signal: Signal) -> String {
    let mut body = String::from("state=");
    match signal.state {
        // Idle has no record: the protocol's `clear` removes one, and a client
        // at rest is exactly the program that is not doing anything.
        State::Idle => return STATUS_CLEAR.to_string(),
        State::Working => {
            body.push_str("working");
            if let Some(percent) = signal.percent {
                body.push_str(&format!(":progress={percent}"));
            }
        }
        State::Blocked(kind) => {
            body.push_str("blocked:kind=");
            body.push_str(kind.name());
        }
        State::Done => body.push_str("done"),
        State::Error => body.push_str("error"),
    }
    // The app name is the stable key a terminal groups records by, not a label.
    body.push_str(":app=horon");
    format!("\x1b]7501;{body}\x07")
}

/// The tab-indicator state and share for a signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Bar {
    state: u8,
    percent: Option<u8>,
}

pub(crate) fn bar_of(signal: Signal) -> Bar {
    match signal.state {
        State::Idle => Bar {
            state: BAR_REMOVE,
            percent: None,
        },
        State::Working => Bar {
            // No share to show is the protocol's indeterminate spinner, so a
            // turn never loses the indicator for want of a count.
            state: if signal.percent.is_some() {
                BAR_SET
            } else {
                BAR_INDETERMINATE
            },
            percent: signal.percent,
        },
        // A finished turn leaves a full bar: the mark is the whole point of
        // reporting completion, and the removal state is the exit path's.
        State::Done => Bar {
            state: BAR_SET,
            percent: Some(100),
        },
        State::Blocked(_) => Bar {
            state: BAR_PAUSED,
            percent: None,
        },
        State::Error => Bar {
            state: BAR_ERROR,
            percent: None,
        },
    }
}

pub(crate) fn bar_sequence(bar: Bar) -> String {
    match bar.percent {
        Some(percent) => format!("\x1b]9;4;{};{percent}\x07", bar.state),
        None => format!("\x1b]9;4;{}\x07", bar.state),
    }
}

/// Bits naming the channels whose bytes the terminal is holding.
const STATUS_HELD: u8 = 1;
const BAR_HELD: u8 = 2;

/// What this process reported on the real terminal.
///
/// The exit paths that cannot reach the loop's [`Signals`] — a panic hook and
/// the signal thread — read it here, and it is only written for the process's
/// own standard output, never for a test's buffer.
static HELD: AtomicU8 = AtomicU8::new(0);

/// The signals written to one stream, remembering what the terminal holds.
pub(crate) struct Signals<W: Write> {
    output: W,
    /// Whether `output` leads to a terminal worth writing to.
    terminal: bool,
    capabilities: Capabilities,
    /// Whether these writes are what [`HELD`] describes.
    track: bool,
    /// The record the terminal holds, so the same one is not written twice.
    status: Option<Signal>,
    /// The indicator the terminal shows, or none when nothing is shown.
    bar: Option<Bar>,
    /// The terminal refused a sequence. The signals are informational and a
    /// refusal is unlikely to be temporary, so the error is reported once and
    /// then left alone instead of warning on every frame.
    broken: bool,
}

impl Signals<Stdout> {
    /// The signals for the process's standard output.
    ///
    /// A redirected stdout writes a file, not a window, so nothing goes through
    /// it: the capability decides the bytes, and this decides whether there is
    /// anywhere to put them.
    pub(crate) fn stdout() -> Self {
        let output = std::io::stdout();
        let terminal = output.is_terminal();
        Self {
            output,
            terminal,
            capabilities: Environment::current().capabilities(),
            track: true,
            status: None,
            bar: None,
            broken: false,
        }
    }
}

impl<W: Write> Signals<W> {
    /// Signals written to `output`, or nowhere when `terminal` is false.
    ///
    /// The test cases build a writer over a buffer, which is why a capability
    /// set is a parameter rather than always read from the environment.
    #[cfg(test)]
    pub(crate) fn with(capabilities: Capabilities, output: W, terminal: bool) -> Self {
        Self {
            output,
            terminal,
            capabilities,
            track: false,
            status: None,
            bar: None,
            broken: false,
        }
    }

    /// Bring the terminal up to date with `signal`.
    ///
    /// Called once a frame; a state the terminal already holds writes nothing.
    /// A channel the settings turned off, or this terminal does not support,
    /// gets no bytes.
    pub(crate) fn sync(&mut self, signal: Signal, options: Options) -> std::io::Result<()> {
        if self.broken || !self.terminal {
            return Ok(());
        }
        if let Err(error) = self.show_status(signal, options.program_status) {
            self.broken = true;
            return Err(error);
        }
        if let Err(error) = self.show_bar(signal, options.progress) {
            self.broken = true;
            return Err(error);
        }
        Ok(())
    }

    /// Remove everything this writer reported, and say whether the terminal
    /// took the sequences.
    ///
    /// Runs on the normal exit, where the writer still knows exactly which
    /// channels it put bytes on, so it writes only those. The panic and signal
    /// paths have no writer left and go through [`clear_stdout`] instead.
    pub(crate) fn clear(&mut self) -> std::io::Result<()> {
        if self.broken || !self.terminal {
            return Ok(());
        }
        if let Err(error) = self.retract_status() {
            self.broken = true;
            return Err(error);
        }
        if let Err(error) = self.retract_bar() {
            self.broken = true;
            return Err(error);
        }
        Ok(())
    }

    /// Write the record, unless the terminal already holds what `signal` means.
    fn show_status(&mut self, signal: Signal, enabled: bool) -> std::io::Result<()> {
        if !self.capabilities.program_status || !enabled {
            return self.retract_status();
        }
        if signal.state == State::Idle {
            return self.retract_status();
        }
        if self.status == Some(signal) {
            return Ok(());
        }
        self.emit(&status_sequence(signal))?;
        self.status = Some(signal);
        self.hold(STATUS_HELD, true);
        Ok(())
    }

    /// Remove the record the terminal holds, if it holds one.
    fn retract_status(&mut self) -> std::io::Result<()> {
        if self.status.is_none() {
            return Ok(());
        }
        self.emit(STATUS_CLEAR)?;
        self.status = None;
        self.hold(STATUS_HELD, false);
        Ok(())
    }

    /// Write the indicator, unless the terminal already shows what `signal`
    /// means.
    fn show_bar(&mut self, signal: Signal, enabled: bool) -> std::io::Result<()> {
        if !self.capabilities.progress || !enabled {
            return self.retract_bar();
        }
        let bar = bar_of(signal);
        // Nothing is shown and nothing has to be: a removal would be a byte
        // that changes nothing.
        if bar.state == BAR_REMOVE && self.bar.is_none() {
            return Ok(());
        }
        if self.bar == Some(bar) {
            return Ok(());
        }
        self.emit(&bar_sequence(bar))?;
        // The removal state leaves nothing behind, so it is not remembered as
        // something the terminal holds.
        if bar.state == BAR_REMOVE {
            self.bar = None;
            self.hold(BAR_HELD, false);
        } else {
            self.bar = Some(bar);
            self.hold(BAR_HELD, true);
        }
        Ok(())
    }

    /// Remove the indicator, if the terminal shows one.
    fn retract_bar(&mut self) -> std::io::Result<()> {
        if self.bar.is_none() {
            return Ok(());
        }
        self.emit(&bar_sequence(Bar {
            state: BAR_REMOVE,
            percent: None,
        }))?;
        self.bar = None;
        self.hold(BAR_HELD, false);
        Ok(())
    }

    /// Record what the process's terminal is holding.
    fn hold(&self, bit: u8, held: bool) {
        if !self.track {
            return;
        }
        if held {
            HELD.fetch_or(bit, Ordering::Relaxed);
        } else {
            HELD.fetch_and(!bit, Ordering::Relaxed);
        }
    }

    /// Write one sequence to the terminal behind this stream.
    fn emit(&mut self, sequence: &str) -> std::io::Result<()> {
        self.output.write_all(sequence.as_bytes())?;
        self.output.flush()
    }

    /// What the signals have written so far.
    #[cfg(test)]
    pub(crate) fn written(&self) -> &W {
        &self.output
    }
}

/// Put the terminal back on a path that cannot reach the loop.
///
/// The panic hook and the signal thread both run here. The normal exit clears
/// through its own writer, which knows exactly what it wrote; this covers the
/// ways out that have no writer left, so a signal cannot leave a record behind
/// that `/quit` would have cleared. The bits say which channels reported, and
/// are read here only, so the two paths never clear the same bytes twice.
///
/// Best effort by design: the terminal may already be gone, and a second error
/// there would replace the one the caller is trying to report. Reports success
/// when there was nothing to clear.
pub(crate) fn clear_stdout() -> bool {
    let held = HELD.swap(0, Ordering::Relaxed);
    if held == 0 {
        return true;
    }
    // The bits say which channels actually reported, which is what has to be
    // undone: asking the environment again could only disagree with the run
    // that wrote the bytes.
    let mut output = std::io::stdout();
    if !output.is_terminal() {
        return false;
    }
    if held & STATUS_HELD != 0 && !write_clear(&mut output, STATUS_CLEAR) {
        return false;
    }
    if held & BAR_HELD != 0 {
        let removal = bar_sequence(Bar {
            state: BAR_REMOVE,
            percent: None,
        });
        if !write_clear(&mut output, &removal) {
            return false;
        }
    }
    output.flush().is_ok()
}

/// Write one clear sequence, reporting whether the terminal took it.
///
/// A separate function so the two channels and the flush share one error
/// convention: a false answer stops the rest instead of writing a second
/// sequence after a failed one.
fn write_clear<W: Write>(output: &mut W, sequence: &str) -> bool {
    output.write_all(sequence.as_bytes()).is_ok()
}

/// Clear the signals before a panic unwinds, then let the installed hook run.
///
/// Installed after `try_init`, so the hook it chains is the one that restores
/// the alternate screen.
pub(crate) fn guard_panics() {
    let restoring = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        clear_stdout();
        restoring(info);
    }));
}
