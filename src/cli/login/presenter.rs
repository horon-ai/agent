//! Login presentation for the shared authentication flows.
//!
//! The headless build carries no terminal crate, so this presenter renders the
//! login URL as plain text and reads secrets as ordinary stdin lines. The
//! shared auth flows still route through the `LoginPresenter` hook, so an
//! interactive client can swap in a richer presenter without touching them.

use super::*;

/// Plain-text implementation of the shared auth flows' login presenter.
struct PlainLoginPresenter;

impl crate::auth::login_ui::LoginPresenter for PlainLoginPresenter {
    fn qr_section(&self, data: &str, heading: &str, indent: &str, browser_suppressed: bool) -> Option<String> {
        url_section(data, heading, indent, browser_suppressed)
    }

    fn read_secret_line(&self) -> Result<String> {
        read_secret_line()
    }
}

pub fn install_login_presenter() {
    crate::auth::login_ui::install(Box::new(PlainLoginPresenter));
}

/// Whether the login URL section prints even when a browser will open.
///
/// The removed QR section appeared automatically when no browser would open, or
/// when one of the login-QR environment flags was set. The plain section keeps
/// the same gate so an ordinary browser login stays quiet.
fn url_section_enabled() -> bool {
    fn truthy(key: &str) -> bool {
        std::env::var(key)
            .map(|value| {
                let trimmed = value.trim();
                !trimmed.is_empty() && trimmed != "0" && !trimmed.eq_ignore_ascii_case("false")
            })
            .unwrap_or(false)
    }
    truthy("HORON_AGENT_SHOW_LOGIN_QR") || truthy("HORON_AGENT_LOGIN_QR")
}

/// A heading plus `data` (a login URL or pairing URI) as plain, indented text.
///
/// No terminal control and no QR glyphs: the value prints verbatim so it stays
/// copy-pasteable. `browser_suppressed` is true when no local browser will open,
/// which is when this section matters most. Returns `None` to show nothing.
pub fn url_section(data: &str, heading: &str, indent: &str, browser_suppressed: bool) -> Option<String> {
    if !browser_suppressed && !url_section_enabled() {
        return None;
    }
    Some(format!("{heading}\n\n{indent}{data}"))
}

/// Read one line of secret input from stdin, trimmed of its trailing newline.
pub fn read_secret_line() -> Result<String> {
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

pub(super) fn read_line_trimmed(prompt: &str) -> Result<String> {
    print!("{}", prompt);
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}
