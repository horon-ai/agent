//! Open a URL in this machine's browser.
//!
//! Login flows run in the CLI process, which is normally the user's own
//! desktop session, so handing the URL to the local opener is all that is
//! needed. Callers print the URL themselves when nothing was launched.

/// Open `url` with the local opener. Returns whether a launch was attempted
/// successfully.
///
/// Test binaries never pop real windows: OAuth flows are exercised heavily by
/// unit tests and each ungated open would take over the developer's desktop.
pub fn open_url(url: &str) -> bool {
    if crate::auth::running_in_test_harness() {
        return false;
    }
    open::that_detached(url).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_harness_never_opens_a_browser() {
        // `running_in_test_harness` is true for this binary, so the URL must be
        // reported as not opened instead of reaching the real opener.
        assert!(!open_url("https://example.com/oauth"));
    }
}
