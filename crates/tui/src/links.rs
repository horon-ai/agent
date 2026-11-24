//! HTTP links in the newest assistant answers, and the one place that opens one.
//!
//! `/links` reads the http(s) URLs out of the assistant messages the transcript
//! already holds and offers them as rows. A row is only a string on screen until
//! the reader presses enter; then the URL is handed to the desktop's own opener
//! as one argument of the launcher, never interpolated into a command line the
//! client builds, so no model or tool output can start a process by itself.
//!
//! Only `http` and `https` are listed and opened. A `file:` URL, a custom
//! scheme, a URL that carries a user name or password, a control character or a
//! string that is not a URL at all is refused when the list is built and checked
//! again before the opener runs, because a row can outlive the answer it came
//! from.

use std::io;
use std::process::{Command, Stdio};

use url::Url;

/// The longest URL the client hands to the platform.
const MAX_URL: usize = 2048;

/// The http(s) URL one answer mentions, normalized for the opener.
///
/// The scanner reads what an answer actually carries: the inline markdown link
/// `[label](target)`, whose target is drawn as source in the transcript, and a
/// bare URL in prose. Everything else the document mentions — a `file:` URL, a
/// custom scheme, a link with a user name — is dropped here rather than shown
/// and refused later. The URL is returned as the parser writes it, so a row and
/// the string the opener receives are the same one.
pub(crate) fn extract(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let Some(offset) = bytes[index..]
            .iter()
            .position(|&byte| matches!(byte, b'[' | b'h' | b'H'))
        else {
            break;
        };
        index += offset;
        if bytes[index] == b'[' {
            if let Some((target, next)) = markdown_target(text, index) {
                if let Some(url) = normalized(target)
                    && !found.contains(&url)
                {
                    found.push(url);
                }
                index = next;
                continue;
            }
            index += 1;
            continue;
        }
        if scheme_at(text, index) {
            let (candidate, next) = bare(text, index);
            if let Some(url) = normalized(trimmed(candidate))
                && !found.contains(&url)
            {
                found.push(url);
            }
            index = next;
            continue;
        }
        index += 1;
    }
    found
}

/// The URL as the parser writes it, if it is an http(s) link the opener may be
/// given.
///
/// The scheme is `http` or `https` and a host is there; a user name or password
/// is refused because the browser would keep it out of the address bar, so the
/// reader never saw what it carries. Control characters and whitespace are
/// refused before parsing: the URL parser strips some of them, which would turn
/// a malformed string into a valid one. The normalized form is what the list
/// shows and what the opener receives.
pub(crate) fn normalized(url: &str) -> Option<String> {
    if url.is_empty() || url.len() > MAX_URL {
        return None;
    }
    if url.is_ascii() {
        // Reject ASCII whitespace, controls, and `\` in one byte pass.
        if url.bytes().any(|byte| byte <= b' ' || byte == 0x7f || byte == b'\\') {
            return None;
        }
    } else {
        if url
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return None;
        }
        // The parser reads `\` as `/` for http(s), which would move a host the
        // reader did not see; refuse the string instead of rewriting it.
        if url.contains('\\') {
            return None;
        }
    }
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let parsed = match Url::parse(url) {
        Ok(parsed) => parsed,
        Err(_) => return None,
    };
    let host = parsed.host_str()?;
    if host.is_empty() || !host.chars().any(char::is_alphanumeric) {
        return None;
    }
    if host.starts_with(['.', '-']) || host.ends_with(['.', '-']) {
        return None;
    }
    // Port zero names no service, and a port the platform cannot use is a
    // malformed URL rather than something to pass on.
    if matches!(parsed.port(), Some(0)) {
        return None;
    }
    Some(parsed.into())
}

/// Hand one URL to the platform handler.
///
/// The URL is checked again here: a row keeps the string it was built from, and
/// the list may outlive the answer that supplied it. Windows takes its own path
/// because the shared opener would hand the URL to `cmd /c start`; everywhere
/// else the opener is the one the login flow already uses.
pub(crate) fn launch(url: &str) -> io::Result<()> {
    let Some(url) = normalized(url) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "refused a URL that is not an http(s) link",
        ));
    };
    if std::env::consts::OS == "windows" {
        return launch_windows(&url);
    }
    open::that_detached(url)
}

/// The Windows handler, with the URL as its own argument.
///
/// Kept apart from [`launch`] so the shape of the call can be read on any host:
/// `rundll32` receives the URL as one argument, where the shared opener would
/// place it in a `cmd /c start` command line. Nothing between the client and
/// the handler parses the URL.
fn windows_handler(url: &str) -> Command {
    let mut command = Command::new("rundll32.exe");
    command.arg("url.dll,FileProtocolHandler").arg(url);
    command
}

/// Start the Windows handler without waiting for the browser.
fn launch_windows(url: &str) -> io::Result<()> {
    windows_handler(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
}

/// The target of the markdown link whose `[` is at `start`, and where the link
/// ends.
///
/// Only the inline form `[label](target "title")` is read, which is what the
/// renderer puts in the answer; a reference definition (`[label]: url`) leaves
/// a bare URL on the line, which the scan finds on its own.
fn markdown_target(text: &str, start: usize) -> Option<(&str, usize)> {
    let after_label = matching(text, start, '[', ']')?;
    if !text.get(after_label..)?.starts_with('(') {
        return None;
    }
    let end = matching(text, after_label, '(', ')')?;
    let inner = text.get(after_label + 1..end - 1)?;
    let target = if let Some(angle) = inner.strip_prefix('<') {
        angle.split_once('>')?.0
    } else {
        inner.split_whitespace().next().unwrap_or("")
    };
    Some((target, end))
}

/// The index just past the bracket matching the one at `start`.
fn matching(text: &str, start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut escaped = false;
    for (offset, character) in text[start..].char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' => escaped = true,
            character if character == open => depth += 1,
            character if character == close => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(start + offset + character.len_utf8());
                }
            }
            _ => {}
        }
    }
    None
}

/// Whether `text` starts with an http or https scheme at `start`.
fn scheme_at(text: &str, start: usize) -> bool {
    text.get(start..start.saturating_add(8))
        .is_some_and(|head| head.eq_ignore_ascii_case("https://"))
        || text
            .get(start..start.saturating_add(7))
            .is_some_and(|head| head.eq_ignore_ascii_case("http://"))
}

/// The run of URL characters from `start`, and where it stopped.
fn bare(text: &str, start: usize) -> (&str, usize) {
    let bytes = text.as_bytes();
    let mut index = start;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte < 0x80 {
            if byte <= b' '
                || byte == 0x7f
                || matches!(
                    byte,
                    b'\\' | b'"' | b'\'' | b'`' | b'<' | b'>' | b'{' | b'}' | b'|' | b'^'
                )
            {
                return (&text[start..index], index);
            }
            index += 1;
            continue;
        }
        for (offset, character) in text[index..].char_indices() {
            if character.is_whitespace()
                || character.is_control()
                || matches!(character, '\\' | '"' | '\'' | '`' | '<' | '>' | '{' | '}' | '|' | '^')
            {
                let end = index + offset;
                return (&text[start..end], end);
            }
        }
        return (&text[start..], text.len());
    }
    (&text[start..index], index)
}

/// A scanned run with the punctuation a sentence put after it removed.
///
/// Prose ends a sentence on the URL rather than in it, and a parenthesis that
/// closes a remark is not part of the address. A closing parenthesis is dropped
/// only while the run holds more of them than it opened, so a URL that really
/// carries balanced parentheses keeps them.
fn trimmed(url: &str) -> &str {
    let mut url = url.trim_end_matches(['.', ',', ';', ':', '!', '?', '*', '_', '~']);
    while url.ends_with(')') && url.matches(')').count() > url.matches('(').count() {
        url = &url[..url.len() - 1];
    }
    url
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::app::{Action, App};
    use crate::overlays::picker::{Kind, Picker, Row};
    use crate::transcript::Item;

    fn app() -> App {
        let (_sender, receiver) = std::sync::mpsc::channel();
        App::new("socket · session_a".to_string(), receiver)
    }

    fn press(app: &mut App, code: KeyCode) -> Option<Action> {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn only_http_and_https_urls_are_allowed() {
        for url in [
            "https://example.test/",
            "http://example.test",
            "HTTPS://Example.Test/a?b=1#c",
            "https://127.0.0.1:8080/x",
            "https://[::1]:9000/x",
            "https://example.test/a_(b)",
        ] {
            assert!(normalized(url).is_some(), "{url} should pass");
        }
        for url in [
            "",
            "file:///tmp/fixture",
            "ftp://example.test/",
            "javascript:alert(1)",
            "https://",
            "https:///path",
            "https://user:secret@example.test/",
            "https://user@example.test/",
            "https://example.test:0/x",
            "https://example.test:99999/x",
            "https://exa mple.test/",
            "https://example.test/\u{7}",
            "HTTPS://example\t.test/",
            "https://.example.test/",
        ] {
            assert!(normalized(url).is_none(), "{url} should be refused");
        }
    }

    #[test]
    fn the_windows_handler_passes_the_url_as_its_own_argument() {
        let command = windows_handler("https://example.test/");
        assert_eq!(command.get_program(), "rundll32.exe");
        let arguments: Vec<&std::ffi::OsStr> = command.get_args().collect();
        assert_eq!(
            arguments,
            vec![
                std::ffi::OsStr::new("url.dll,FileProtocolHandler"),
                std::ffi::OsStr::new("https://example.test/"),
            ],
            "the URL is not its own argument: {arguments:?}"
        );
    }

    #[test]
    fn the_list_shows_only_http_links_from_answers() {
        let mut app = app();
        app.transcript
            .push_assistant("[docs](https://example.test/) and file:///tmp/fixture");
        assert!(press(&mut app, KeyCode::Char('/')).is_none());
        for character in "links".chars() {
            assert!(press(&mut app, KeyCode::Char(character)).is_none());
        }
        assert!(press(&mut app, KeyCode::Enter).is_none());
        let picker = app.picker.as_ref().expect("/links did not open the list");
        assert_eq!(picker.kind(), Kind::Links);
        let rows: Vec<&str> = picker.visible().iter().map(|row| row.label.as_str()).collect();
        assert_eq!(rows, vec!["https://example.test/"]);
        // Escape closes the list and opens nothing.
        assert!(press(&mut app, KeyCode::Esc).is_none());
        assert!(app.picker.is_none());
        assert!(
            !app.transcript
                .items()
                .iter()
                .any(|item| matches!(item, Item::Notice(text) if text.starts_with("opening "))),
            "escape opened a link"
        );
    }

    #[test]
    fn choosing_a_refused_url_reports_it_and_opens_nothing() {
        let mut app = app();
        app.picker = Some(Picker::new(
            Kind::Links,
            "Links",
            vec![Row::new("file:///tmp/fixture", "file:///tmp/fixture")],
            "no links",
        ));
        assert!(press(&mut app, KeyCode::Enter).is_none());
        assert!(app.picker.is_none(), "the list stayed open");
        assert!(
            app.transcript
                .items()
                .iter()
                .any(|item| matches!(item, Item::Warning(text) if text.contains("refused"))),
            "a refused row was not reported: {:?}",
            app.transcript.items()
        );
    }
}
