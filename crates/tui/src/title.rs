//! The terminal window title.
//!
//! The window is named `horon · <label>`, so a stack of terminals says which one
//! belongs to which session. The title the shell set goes onto the terminal's
//! title stack first and comes back on every way out — normal exit, error exit
//! and panic — because the shell still owns the window after the client leaves.
//!
//! Nothing is written when standard output is not a terminal: a redirect writes
//! a file rather than a window, and escape bytes in it are noise.
//!
//! With `title.state` on, the name is followed by what the turn is doing — a
//! fixed word, not a spinner — so a window seen from another one says whether
//! its session is busy. The word is written only when it changes, which is why
//! it can be static: a moving glyph would need a timer and a sequence every
//! frame for a decoration the terminal may not even show.

use std::io::{IsTerminal, Stdout, Write};

/// The environment the client already sees, as far as the title stack cares.
///
/// The save/restore pair (`CSI 22 ; 2 t` / `CSI 23 ; 2 t`) is an extension: a
/// terminal that does not keep a title stack ignores both sequences, so writing
/// them there changed nothing and the promise that the shell's title comes back
/// was silently false. Nothing in the protocol lets a plain-output client ask,
/// so the decision is made once, from what the terminal says about itself.
#[derive(Debug, Default, Clone)]
pub(crate) struct TitleEnvironment {
    /// `TERM`: a `dumb` terminal has no title handling at all.
    pub(crate) term: Option<String>,
    /// `TERM_PROGRAM`: set by an emulator that names itself.
    pub(crate) term_program: Option<String>,
    /// `WT_SESSION`: Windows Terminal, which keeps the stack.
    pub(crate) wt_session: Option<String>,
    /// `ConEmuANSI`: ConEmu, which keeps the stack.
    pub(crate) conemu_ansi: Option<String>,
}

impl TitleEnvironment {
    /// The environment of this process.
    pub(crate) fn current() -> Self {
        // `var_os` rather than `var`: a terminal name that is not valid UTF-8
        // still means "this variable is set", and lossy text is enough to
        // compare against the handful of markers below.
        let read = |name: &str| std::env::var_os(name).map(|value| value.to_string_lossy().into_owned());
        Self {
            term: read("TERM"),
            term_program: read("TERM_PROGRAM"),
            wt_session: read("WT_SESSION"),
            conemu_ansi: read("ConEmuANSI"),
        }
    }

    /// Whether this environment keeps a title stack.
    ///
    /// `windows` is a parameter so the decision is a pure function of the
    /// environment and the platform, and a test on either platform can ask
    /// about either one.
    pub(crate) fn keeps_title_stack(&self, windows: bool) -> bool {
        // A program that names itself is a real emulator: `TERM` is a fallback
        // for environments that did not say what they are.
        if self.term_program.is_some() {
            return true;
        }
        // `dumb` is the terminal saying it has no title handling.
        if self.term.as_deref() == Some("dumb") {
            return false;
        }
        // Any other `TERM` is an emulator that told us what it is, including
        // one running on Windows (mintty, a shell under Windows Terminal).
        if self.term.is_some() {
            return true;
        }
        // What is left with no `TERM` on Windows is the plain console, the one
        // terminal whose default ignores the stack pair; its modern
        // replacements set a marker of their own.
        if windows && self.wt_session.is_none() && self.conemu_ansi.is_none() {
            return false;
        }
        // Unknown but not `dumb`: assume xterm-like, which is what the client
        // has always assumed.
        true
    }
}

/// Whether this process's terminal keeps a title stack.
pub(crate) fn title_stack_supported() -> bool {
    TitleEnvironment::current().keeps_title_stack(std::env::consts::OS == "windows")
}

/// Longest label the title keeps, in characters.
const LABEL_CHARS: usize = 80;

/// What every title starts with.
const NAME: &str = "horon";

/// Separates the name from the label.
const SEPARATOR: &str = " · ";

/// Pushes the title the terminal shows now onto its stack (`CSI 22 ; 2 t`).
pub(crate) const PUSH: &str = "\x1b[22;2t";

/// Pops the title the stack holds back into place (`CSI 23 ; 2 t`).
pub(crate) const POP: &str = "\x1b[23;2t";

/// The label the title shows.
///
/// The session's own title wins; a session the daemon never titled is named
/// after its working directory, so a fresh window still says where it is.
///
/// Control characters are dropped because the label ends up inside an escape
/// sequence: a session titled `x`, ESC, `]0;evil` would otherwise close the
/// sequence and write the rest as a title of its own choosing.
pub(crate) fn label(title: Option<&str>, working_dir: Option<&str>) -> String {
    if let Some(named) = title.map(clean).filter(|label| !label.is_empty()) {
        return named;
    }
    match working_dir.and_then(last_component) {
        Some(component) => clean(component),
        None => String::new(),
    }
}

/// The last component of a path, or none for the root and a trailing `..`.
fn last_component(path: &str) -> Option<&str> {
    std::path::Path::new(path).file_name().and_then(|name| name.to_str())
}

/// Text fit for a title: no control characters, and no more than the cap.
fn clean(text: &str) -> String {
    let visible: String = text.chars().filter(|character| !character.is_control()).collect();
    visible.trim().chars().take(LABEL_CHARS).collect()
}

/// The bytes that name the window.
///
/// `OSC 0` sets the icon name and the title together and `OSC 2` sets the title
/// alone; window managers read one or the other, so both are sent. BEL closes
/// each sequence, which is safe because the label carries no control characters
/// by the time it gets here.
pub(crate) fn set(label: &str) -> String {
    let name = if label.is_empty() {
        NAME.to_string()
    } else {
        format!("{}{}{}", NAME, SEPARATOR, label)
    };
    format!("\x1b]0;{name}\x07\x1b]2;{name}\x07")
}

/// What the turn is doing, as the window title says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunState {
    /// A turn is in flight.
    Running,
    /// A prompt is on screen, waiting for the user's answer.
    Waiting,
    /// Nothing is happening.
    Idle,
}

impl RunState {
    /// The state these facts describe.
    ///
    /// The order is the status segment's: a prompt outranks the turn, because
    /// the turn is stopped while the user decides, and a session the daemon
    /// calls `generating` is running whether or not this client holds the turn.
    pub(crate) fn of(prompt: bool, turn: bool, session_status: Option<&str>) -> Self {
        if prompt {
            return Self::Waiting;
        }
        if turn || session_status == Some("generating") {
            return Self::Running;
        }
        Self::Idle
    }

    /// The word the title carries for this state.
    pub(crate) fn marker(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Idle => "idle",
        }
    }
}

/// The label with the state marker appended, or the label alone when no state
/// is shown.
///
/// The label is the one [`label`] returned, so the off case passes it through
/// instead of copying it; it already carries no control characters, and the
/// marker is a fixed word from this file.
pub(crate) fn with_state(mut label: String, state: Option<RunState>) -> String {
    match state {
        None => label,
        Some(state) if label.is_empty() => state.marker().to_string(),
        Some(state) => {
            label.push_str(SEPARATOR);
            label.push_str(state.marker());
            label
        }
    }
}

/// The window title, remembering what the window already says.
///
/// `W` is the stream the sequences go to; it is a parameter so a test can read
/// back exactly which bytes were written.
pub(crate) struct Title<W: Write> {
    /// Where the sequences go.
    output: W,
    /// Whether `output` leads to a terminal worth writing to.
    terminal: bool,
    /// The label the window already reads, so the same one is not written twice.
    current: Option<String>,
    /// Whether the terminal keeps a title stack, so the save/restore pair is
    /// worth writing there.
    stack: bool,
    /// The terminal refused a sequence. The title is cosmetic and a refusal is
    /// unlikely to be temporary, so the error is reported once and then left
    /// alone instead of warning again on every change.
    broken: bool,
}

/// The title for the process's standard output.
pub(crate) type WindowTitle = Title<Stdout>;

impl Title<Stdout> {
    /// The title on standard output.
    ///
    /// A redirected stdout writes a file, not a window, so nothing goes through
    /// it.
    pub(crate) fn stdout(enabled: bool) -> Self {
        let output = std::io::stdout();
        let terminal = enabled && output.is_terminal();
        Self::with_stack(output, terminal, terminal && title_stack_supported())
    }
}

impl<W: Write> Title<W> {
    /// A title written to `output`, or nowhere when `terminal` is false.
    /// A title that also knows whether the terminal keeps a title stack.
    pub(crate) fn with_stack(output: W, terminal: bool, stack: bool) -> Self {
        Self {
            output,
            terminal,
            current: None,
            stack,
            broken: false,
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.terminal
    }

    /// A child may have changed the title while it owned the screen.
    pub(crate) fn forget_label(&mut self) {
        self.current = None;
        self.broken = false;
    }

    /// Whether the title may carry the turn state marker.
    ///
    /// The marker rides the same sequences as the name, and a terminal that
    /// keeps no title stack is one whose title handling cannot be trusted: it
    /// keeps the name alone, exactly as it did before the setting existed.
    pub(crate) fn state_supported(&self) -> bool {
        self.stack
    }

    /// Push the title the terminal shows now, then name the window.
    ///
    /// The push happens once, after the alternate screen is up: the stack entry
    /// is what [`pop`] brings back when the client leaves.
    pub(crate) fn start(&mut self, label: &str) -> std::io::Result<()> {
        // A terminal that keeps no stack ignores the push, and the matching pop
        // would then restore nothing: only terminals that keep one get the pair.
        if self.stack {
            self.emit(PUSH)?;
        }
        self.show(label)
    }

    /// Name the window `horon · <label>`, unless it already reads that.
    pub(crate) fn show(&mut self, label: &str) -> std::io::Result<()> {
        if self.broken || self.current.as_deref() == Some(label) {
            return Ok(());
        }
        match self.emit(&set(label)) {
            Ok(()) => {
                self.current = Some(label.to_string());
                Ok(())
            }
            Err(error) => {
                self.broken = true;
                Err(error)
            }
        }
    }

    /// Write one sequence, or nothing when there is no terminal behind it.
    fn emit(&mut self, sequence: &str) -> std::io::Result<()> {
        if !self.terminal {
            return Ok(());
        }
        self.output.write_all(sequence.as_bytes())?;
        self.output.flush()
    }

    /// What the title has written so far.
    #[cfg(test)]
    pub(crate) fn written(&self) -> &W {
        &self.output
    }
}

/// Give the terminal its title back, and say whether it took the sequence.
///
/// Best effort by design: it runs on the exit and panic paths, where the
/// terminal may already be gone and a second error would replace the one the
/// caller is trying to report. The answer is there for a caller with somewhere
/// to put it, as the paste mode's `disable` has.
pub(crate) fn pop() -> bool {
    let mut output = std::io::stdout();
    if !output.is_terminal() {
        return false;
    }
    write_pop(&mut output, title_stack_supported())
}

/// Write the restore sequence when the terminal keeps a title stack.
///
/// Reports success when there was nothing to write: a terminal without the
/// extension has no stack to restore, which is not a failure.
pub(crate) fn write_pop<W: Write>(output: &mut W, stack: bool) -> bool {
    if !stack {
        return true;
    }
    let written = output.write_all(POP.as_bytes()).is_ok();
    written && output.flush().is_ok()
}

/// Give the title back before a panic unwinds, then let the installed hook run.
///
/// Installed after `try_init`, so the hook it chains is the one that restores
/// the alternate screen.
pub(crate) fn guard_panics() {
    let restoring = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        pop();
        restoring(info);
    }));
}
