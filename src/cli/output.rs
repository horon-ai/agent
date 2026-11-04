pub const QUIET_ENV: &str = "HORON_AGENT_QUIET";

pub fn set_quiet_enabled(enabled: bool) {
    if enabled {
        crate::env::set_var(QUIET_ENV, "1");
    } else {
        crate::env::remove_var(QUIET_ENV);
    }
}

pub fn quiet_enabled() -> bool {
    std::env::var(QUIET_ENV)
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

pub fn stderr_info(message: impl AsRef<str>) {
    if !quiet_enabled() {
        eprintln!("{}", crate::output_style::terminal_text(message.as_ref()));
    }
}

pub fn terminal_title(title: impl AsRef<str>) -> String {
    crate::output_style::terminal_text(title.as_ref()).into_owned()
}

pub fn stderr_blank_line() {
    if !quiet_enabled() {
        eprintln!();
    }
}

/// Print each line centered to the terminal width (CLI update/restart banners).
///
/// Width comes from the `COLUMNS` environment variable (falling back to 80)
/// because the headless build carries no terminal-size library.
pub fn print_centered(msg: &str) {
    let msg = crate::output_style::terminal_text(msg);
    let width = terminal_width();
    for line in msg.lines() {
        let visible_len = visible_width(line);
        if visible_len >= width {
            println!("{}", line);
        } else {
            let pad = (width - visible_len) / 2;
            println!("{:>pad$}{}", "", line, pad = pad);
        }
    }
}

fn terminal_width() -> usize {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|width| *width > 0)
        .unwrap_or(80)
}

fn visible_width(s: &str) -> usize {
    crate::message::strip_ansi_escape_sequences(s).chars().count()
}
