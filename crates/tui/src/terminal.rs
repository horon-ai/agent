//! Putting the terminal back the way the client found it.
//!
//! The client turns on bracketed paste, takes the alternate screen, and asks
//! for mouse reporting unless the setting is off; it also pushes the shell's
//! window title. Every way out it runs itself restores those: the normal exit,
//! the error path and the panic hooks. A signal is not an exit it runs. The
//! default disposition replaces the process, so a client killed by `SIGTERM`
//! or `SIGHUP` left the next program in a terminal that still reported pastes
//! as one event and delivered mouse escapes as text (#431).
//!
//! The Unix half below runs the same restore sequence from those signals and
//! then re-raises, so the exit status still reports the signal.
//!
//! Handing the screen to the external editor and taking it back lives here too,
//! because both are the client giving up and reclaiming the terminal it owns.
//! What the window is named is [`crate::window`]'s job; the single frame drawn
//! before the client gives up on a terminal it cannot start is here, because it
//! needs the real terminal and the app state at once.

use crate::{app::App, draw, paste, scroll, title};

/// Put the terminal back the way the client found it.
///
/// One sequence for the normal exit and the signal path: if the two drift, a
/// signal would restore less than `/quit` does and leave the next program in a
/// half-owned terminal.
pub(crate) fn restore(pop_title: bool) {
    // The progress indicator and the status record outlive the process that
    // wrote them. The normal exit clears them through its own writer, which
    // knows what it wrote; this covers the panic hook and the signal thread,
    // which have no writer left, and finds nothing to do after the writer ran.
    crate::progress::clear_stdout();
    // The running local command belongs to this terminal and exits with it.
    crate::commands::shell::kill_active_group();
    // The voice recorder, recognizer or player is this terminal's child too.
    crate::voice::kill_active_process();
    release(pop_title);
}

/// Hand the terminal to a child process that draws on it.
///
/// The screen is released exactly as [`restore`] releases it, with one
/// difference: the window title stays. The title names this client, and the
/// child is this client's editor, so the stack entry [`restore`] pops is left
/// for the exit rather than being popped and pushed once per hand-over.
///
/// A child that read a terminal still reporting bracketed pastes would take the
/// client's paste mode into its own text, so nothing is left on.
pub(crate) fn hand_over() {
    release(false);
}

/// Take the terminal back after [`hand_over`].
///
/// The inverse of the release, so the next frame paints the whole screen
/// instead of a difference against rows the editor overwrote. Pastes and the
/// mouse belong to the caller, which reports their refusals where the startup
/// path does; the title was never given up.
///
/// Attempt every step and report the first failure so the caller can restore
/// the terminal and exit safely.
pub(crate) fn take_back<B>(terminal: &mut ratatui::Terminal<B>) -> std::io::Result<()>
where
    B: ratatui::backend::Backend,
    B::Error: std::fmt::Display,
{
    let mut failure = None;
    let mut note = |error: String| {
        if failure.is_none() {
            failure = Some(std::io::Error::other(error));
        }
    };
    if let Err(error) = crossterm::terminal::enable_raw_mode() {
        note(format!("cannot turn raw mode back on: {error}"));
    }
    let mut output = std::io::stdout();
    if let Err(error) = crossterm::execute!(output, crossterm::terminal::EnterAlternateScreen) {
        note(format!("cannot enter the alternate screen again: {error}"));
    }
    if let Err(error) = terminal.clear() {
        note(format!("cannot clear the screen: {error}"));
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Undo the modes the client turned on, or only step off the screen.
fn release(pop_title: bool) {
    paste::disable();
    // The stack entry goes back with the screen: the title the shell set is the
    // one the next prompt should be in. A hand-over keeps it, because the client
    // still owns the window and only the screen is lent out.
    if pop_title {
        title::pop();
    }
    // Released on the normal exit as well as the error one: a captured terminal
    // does not hand a drag back to its own text selection.
    scroll::disable_mouse();
    ratatui::restore();
}

/// Restore the terminal when a signal ends the client.
///
/// Best effort like the rest of the exit path: a client that cannot install
/// the handlers reports the refusal and keeps running, because losing the
/// signal path is not a reason to refuse to start.
#[cfg(unix)]
pub(crate) fn guard_signals(pop_title: bool) -> std::io::Result<()> {
    unix::install(pop_title)
}

#[cfg(not(unix))]
pub(crate) fn guard_signals(_pop_title: bool) -> std::io::Result<()> {
    // The console is torn down with the process on Windows.
    Ok(())
}

/// Draw the connection-failure banner once, when there is a terminal to draw it
/// on, then restore it.
pub(crate) fn show_failure(message: &str) {
    use std::io::IsTerminal as _;

    eprintln!("horon-tui: {message}");
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return;
    }
    let mut app = App::failure(message);
    match ratatui::try_init() {
        Ok(mut terminal) => {
            let failure = draw(&mut terminal, &mut app).err();
            paste::disable();
            ratatui::restore();
            if let Some(message) = failure {
                eprintln!("horon-tui: {message}");
            }
        }
        Err(error) => {
            paste::disable();
            ratatui::restore();
            eprintln!("horon-tui: cannot open a terminal: {error}");
        }
    }
}

#[cfg(unix)]
mod unix {
    //! `SIGTERM`, `SIGHUP` and `SIGQUIT` through a self-pipe.
    //!
    //! A signal handler may only call async-signal-safe functions, so it does
    //! the smallest possible thing: writes one byte holding the signal number.
    //! The restore sequence runs on an ordinary thread that reads it, where
    //! writing escape sequences, using the allocator and asking crossterm to
    //! leave raw mode are all allowed.

    use std::io;
    use std::sync::atomic::{AtomicI32, Ordering};

    /// Write end of the self-pipe, or -1 before it exists and after a failed
    /// install.
    static WAKE_FD: AtomicI32 = AtomicI32::new(-1);

    extern "C" fn deliver(signal: libc::c_int) {
        let fd = WAKE_FD.load(Ordering::Relaxed);
        if fd < 0 {
            return;
        }
        let byte = signal as u8;
        // `write` is async-signal-safe, and the descriptor is non-blocking so
        // this can never wait. The result is deliberately ignored: a handler
        // that cannot report has nowhere to report to.
        unsafe { libc::write(fd, (&byte as *const u8).cast(), 1) };
    }

    pub(super) fn install(pop_title: bool) -> io::Result<()> {
        let mut fds = [0 as libc::c_int; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let (read_fd, write_fd) = (fds[0], fds[1]);
        for fd in [read_fd, write_fd] {
            // Neither end may leak into a program the client starts later.
            unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) };
        }
        let flags = unsafe { libc::fcntl(write_fd, libc::F_GETFL) };
        if flags >= 0 {
            unsafe { libc::fcntl(write_fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        }
        WAKE_FD.store(write_fd, Ordering::Release);
        for signal in [libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
            let handler = deliver as *const () as libc::sighandler_t;
            if unsafe { libc::signal(signal, handler) } == libc::SIG_ERR {
                let error = io::Error::last_os_error();
                WAKE_FD.store(-1, Ordering::Release);
                return Err(error);
            }
        }
        std::thread::Builder::new().name("tui-signal".into()).spawn(move || {
            let mut byte = 0u8;
            loop {
                let read = unsafe { libc::read(read_fd, (&mut byte as *mut u8).cast(), 1) };
                if read == 1 {
                    break;
                }
                if read == 0 {
                    // Every writer is gone, so no further signal can arrive.
                    return;
                }
                if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    return;
                }
            }
            super::restore(pop_title);
            let signal = libc::c_int::from(byte);
            // The default disposition takes the process, so a wrapper sees
            // death by signal (143 through a shell for `SIGTERM`) rather than
            // the clean exit the restore above would otherwise look like.
            unsafe {
                libc::signal(signal, libc::SIG_DFL);
                libc::raise(signal);
            }
            // `raise` returns only when the signal is blocked for this process;
            // the command has still been ended by a signal, so say so.
            std::process::exit(128 + signal);
        })?;
        Ok(())
    }
}
