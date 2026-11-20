//! Putting the terminal back the way the client found it, and naming its window.
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
//! What the window is named, and the single frame drawn before the client gives
//! up on a terminal it cannot start, are decided here too: both are what the
//! client does to a terminal other than drawing into it, and both need the app
//! state and the real terminal at once.

use crate::{app::App, draw, paste, scroll, title};

/// Put the terminal back the way the client found it.
///
/// One sequence for the normal exit and the signal path: if the two drift, a
/// signal would restore less than `/quit` does and leave the next program in a
/// half-owned terminal.
pub(crate) fn restore() {
    paste::disable();
    // The stack entry goes back with the screen: the title the shell set is the
    // one the next prompt should be in.
    title::pop();
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
pub(crate) fn guard_signals() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        unix::install()
    }
    #[cfg(not(unix))]
    {
        // Windows has no signal to catch here: the console is torn down with
        // the process, and a console-control handler would need a Windows
        // dependency the client does not carry.
        Ok(())
    }
}

/// Draw the connection-failure banner once, when there is a terminal to draw it
/// on, then restore it.
pub(crate) fn show_failure(message: &str) {
    eprintln!("horon-tui: {message}");
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

/// The name the window carries for the app's state.
///
/// The session's own title wins and the working directory names a session the
/// daemon never titled, so a stack of terminals says which one belongs to which
/// session and a fresh window still says where it is. With `title.state` on,
/// the turn state follows the name as a fixed word, so a window seen from
/// another one says whether its session is busy without being switched to.
pub(crate) fn window_label(app: &App, state_supported: bool) -> String {
    let label = title::label(app.session_title.as_deref(), app.cwd.as_deref());
    title::with_state(label, window_state(app, state_supported))
}

/// The state the title names, or none when the setting is off or the terminal
/// keeps no title worth adding to.
pub(crate) fn window_state(app: &App, state_supported: bool) -> Option<title::RunState> {
    if !state_supported || !app.title_state {
        return None;
    }
    Some(title::RunState::of(
        app.prompt.is_some(),
        app.pending_turn,
        app.session_status.as_deref(),
    ))
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

    pub(super) fn install() -> io::Result<()> {
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
            super::restore();
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
