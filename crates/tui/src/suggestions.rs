//! Local draft suggestions: an optional ghost completion of the line being
//! typed, offered from the drafts this session has already sent.
//!
//! Nothing here sends, queues or stores a draft. A suggestion is a suffix kept
//! beside the editor and drawn after the caret; it reaches the input only when
//! the accept key is pressed, and `esc` puts it away. The provider is local and
//! synchronous — no socket, no harness request, no background thread — so an
//! unsent draft cannot leave the client, and a client with the setting off never
//! asks for one at all.
//!
//! A suggestion belongs to one request generation and to the exact text it was
//! computed from. A changed draft starts a new request and a new generation,
//! and an answer that names an older one is dropped rather than drawn, which is
//! what keeps a late answer from replacing text that has moved on. The work is
//! bounded at every edge: a draft longer than [`MAX_DRAFT_CHARS`] is not asked
//! about, a suffix longer than [`MAX_SUGGESTION_CHARS`] is never stored, the
//! local provider reads at most [`MAX_SCAN_ENTRIES`] history entries, and an
//! answer that took longer than the budget is discarded without being shown.

use std::time::Duration;

/// Longest draft a suggestion is computed for, in characters.
pub(crate) const MAX_DRAFT_CHARS: usize = 4_096;

/// Longest suffix a suggestion may carry, in characters.
pub(crate) const MAX_SUGGESTION_CHARS: usize = 200;

/// History entries the local provider reads before it gives up.
pub(crate) const MAX_SCAN_ENTRIES: usize = 100;

/// Time one provider call may take before its answer is thrown away.
///
/// The local provider is a bounded scan over drafts in memory and finishes far
/// inside this; a slower provider added later has its answer dropped here
/// rather than shown.
pub(crate) const BUDGET: Duration = Duration::from_millis(8);

/// One request: the generation it belongs to and the exact text it asks about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Request {
    generation: u64,
    text: String,
}

/// An answer that may still be drawn: the suffix and the text it completes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Offer {
    generation: u64,
    text: String,
    suffix: String,
}

/// The suggestion state of one editor.
///
/// A two-phase request (`begin` then `finish`) is what makes a stale answer
/// representable: the caller computes between the two calls, and `finish`
/// compares the answer's generation and text against the request in force.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Suggestions {
    enabled: bool,
    /// Bumped for every new request; an answer naming another one is stale.
    generation: u64,
    /// The draft fingerprint the last request was made for.
    synced: Option<u64>,
    /// The request an answer would belong to, if one is outstanding.
    outstanding: Option<Request>,
    /// The suffix on offer, for the generation and text it was made for.
    offer: Option<Offer>,
}

impl Suggestions {
    /// Suggestions off, as a client starts.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether the draft with this fingerprint has already been asked about.
    ///
    /// A disabled client is never in line with a draft, so the first frame
    /// after the setting turns on starts a request.
    pub fn is_synced(&self, fingerprint: u64) -> bool {
        self.enabled && self.synced == Some(fingerprint)
    }

    /// Start a request for the exact draft `text`.
    ///
    /// A new request always wins the generation, so an answer to the previous
    /// one can no longer be offered. `None` means the draft is one no local
    /// suggestion applies to: empty, a command line, or longer than the cap.
    pub fn begin(&mut self, fingerprint: u64, text: &str) -> Option<Request> {
        self.skip(fingerprint);
        if text.is_empty() || text.starts_with('/') || text.chars().count() > MAX_DRAFT_CHARS {
            return None;
        }
        let request = Request {
            generation: self.generation,
            text: text.to_string(),
        };
        self.outstanding = Some(request.clone());
        Some(request)
    }

    /// Note that the draft was seen, without asking a provider about it.
    ///
    /// Used for a draft the caller already knows is past the cap: the text is
    /// never built, so the bound holds before the allocation rather than after
    /// it.
    pub fn skip(&mut self, fingerprint: u64) {
        self.enabled = true;
        self.generation = self.generation.wrapping_add(1);
        self.synced = Some(fingerprint);
        self.outstanding = None;
        self.offer = None;
    }

    /// Offer the answer to `request`, or drop it as stale.
    ///
    /// The answer is kept only when it names the request in force, when it
    /// arrived inside the budget, and when it is a single-line suffix within
    /// the size cap. Anything else leaves the editor exactly as it was.
    pub fn finish(&mut self, request: &Request, suffix: &str, elapsed: Duration) -> bool {
        if elapsed > BUDGET {
            return false;
        }
        if self.outstanding.as_ref() != Some(request) {
            return false;
        }
        if self.generation != request.generation {
            return false;
        }
        let count = suffix.chars().count();
        if count == 0 || count > MAX_SUGGESTION_CHARS {
            return false;
        }
        if suffix.chars().any(char::is_control) {
            return false;
        }
        self.offer = Some(Offer {
            generation: request.generation,
            text: request.text.clone(),
            suffix: suffix.to_string(),
        });
        true
    }

    /// Turn suggestions off and forget any request or answer.
    pub fn withdraw(&mut self) {
        self.enabled = false;
        self.synced = None;
        self.outstanding = None;
        self.offer = None;
    }

    /// Whether an answer exists for the generation in force.
    ///
    /// Cheap enough for a frame to check before it builds the draft's text for
    /// the exact comparison.
    pub fn pending(&self) -> bool {
        self.enabled
            && self
                .offer
                .as_ref()
                .is_some_and(|offer| offer.generation == self.generation)
    }

    /// The suffix, when the answer was made for exactly `text`.
    ///
    /// The text comparison is the whole point: a generation that matches but a
    /// draft that does not is still a stale answer, and the editor must never
    /// draw or accept one.
    pub fn visible(&self, text: &str) -> Option<&str> {
        let offer = self.offer.as_ref()?;
        if !self.enabled || offer.generation != self.generation || offer.text != text {
            return None;
        }
        Some(offer.suffix.as_str())
    }

    /// Accept the answer for `text`: the whole draft including the suffix.
    ///
    /// The editor changes only here; until this call the draft is untouched.
    pub fn accept(&mut self, text: &str) -> Option<String> {
        let suffix = self.visible(text)?.to_string();
        self.offer = None;
        Some(format!("{text}{suffix}"))
    }

    /// Put the answer away without changing the draft; `false` when none was on
    /// offer.
    pub fn dismiss(&mut self) -> bool {
        self.offer.take().is_some()
    }
}

/// The rest of the newest draft that starts with `draft`, for a local provider.
///
/// Only one line is ever suggested: a draft completed across a line break
/// cannot be drawn after the caret, so a multi-line entry is skipped. The scan
/// is bounded by [`MAX_SCAN_ENTRIES`] and the suffix by [`MAX_SUGGESTION_CHARS`],
/// so a long history costs the same as a short one.
pub(crate) fn local_suffix<'a>(draft: &str, drafts: impl Iterator<Item = &'a str>) -> Option<String> {
    if draft.is_empty() || draft.chars().count() > MAX_DRAFT_CHARS {
        return None;
    }
    drafts.take(MAX_SCAN_ENTRIES).find_map(|entry| {
        if entry.len() <= draft.len() || !entry.starts_with(draft) {
            return None;
        }
        let suffix = &entry[draft.len()..];
        let count = suffix.chars().count();
        if count == 0 || count > MAX_SUGGESTION_CHARS {
            return None;
        }
        if suffix.chars().any(char::is_control) {
            return None;
        }
        if suffix.trim().is_empty() {
            return None;
        }
        Some(suffix.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The request an eligible draft gets.
    fn request(state: &mut Suggestions, text: &str) -> Request {
        state.begin(1, text).expect("an eligible draft")
    }

    #[test]
    fn an_answer_is_shown_and_accepted_only_for_its_own_text() {
        let mut state = Suggestions::new();
        let request = request(&mut state, "hel");
        assert!(state.finish(&request, "lo", Duration::ZERO));
        assert_eq!(state.visible("hel"), Some("lo"));
        // The same generation with different text is still a stale answer.
        assert_eq!(state.visible("hell"), None);
        assert_eq!(state.accept("hell"), None);
        assert_eq!(state.accept("hel").as_deref(), Some("hello"));
        assert_eq!(state.visible("hel"), None, "an accepted answer is spent");
    }

    #[test]
    fn an_answer_for_an_older_generation_is_dropped() {
        let mut state = Suggestions::new();
        let stale = request(&mut state, "hel");
        let current = request(&mut state, "hello");
        assert!(!state.finish(&stale, " world", Duration::ZERO));
        assert_eq!(state.visible("hello"), None);
        assert!(state.finish(&current, " world", Duration::ZERO));
        assert_eq!(state.visible("hello"), Some(" world"));
    }

    #[test]
    fn an_answer_slower_than_the_budget_is_dropped() {
        let mut state = Suggestions::new();
        let request = request(&mut state, "hel");
        assert!(!state.finish(&request, "lo", BUDGET + Duration::from_millis(1)));
        assert!(!state.pending());
        assert_eq!(state.visible("hel"), None);
    }

    #[test]
    fn a_suffix_over_the_cap_is_never_stored() {
        let mut state = Suggestions::new();
        let request = request(&mut state, "hel");
        let suffix = "x".repeat(MAX_SUGGESTION_CHARS + 1);
        assert!(!state.finish(&request, &suffix, Duration::ZERO));
        assert_eq!(state.visible("hel"), None);
        assert!(!state.finish(&request, "two\nlines", Duration::ZERO));
        assert_eq!(state.visible("hel"), None);
    }

    #[test]
    fn a_draft_over_the_cap_or_a_command_line_is_never_asked_about() {
        let mut state = Suggestions::new();
        let long = "x".repeat(MAX_DRAFT_CHARS + 1);
        assert!(state.begin(1, &long).is_none());
        assert!(!state.pending());
        assert!(state.begin(2, "/help").is_none());
        assert!(state.begin(3, "").is_none());
    }

    #[test]
    fn a_dismissed_answer_stays_away_and_leaves_the_draft_to_the_caller() {
        let mut state = Suggestions::new();
        let request = request(&mut state, "hel");
        assert!(state.finish(&request, "lo", Duration::ZERO));
        assert!(state.dismiss());
        assert!(!state.dismiss());
        assert_eq!(state.visible("hel"), None);
        assert_eq!(state.accept("hel"), None);
    }

    #[test]
    fn a_skipped_draft_is_marked_seen_with_no_request() {
        let mut state = Suggestions::new();
        let request = request(&mut state, "he");
        state.finish(&request, "llo", Duration::ZERO);
        state.skip(7);
        assert!(state.is_synced(7));
        assert!(!state.pending());
        assert_eq!(state.visible("he"), None);
    }

    #[test]
    fn the_local_provider_matches_a_prefix_of_one_line() {
        let drafts = ["hello world", "help me", "other"];
        assert_eq!(local_suffix("hel", drafts.iter().copied()).as_deref(), Some("lo world"));
        assert_eq!(local_suffix("hello world", drafts.iter().copied()), None);
        assert_eq!(local_suffix("zzz", drafts.iter().copied()), None);
        assert_eq!(local_suffix("", drafts.iter().copied()), None);
        assert_eq!(local_suffix("a", ["a\nb"].into_iter()), None);
        let long = format!("h{}", "x".repeat(MAX_SUGGESTION_CHARS + 1));
        assert_eq!(local_suffix("h", [long.as_str()].into_iter()), None);
        assert_eq!(local_suffix("h", ["h   "].into_iter()), None);
    }

    #[test]
    fn the_local_provider_stops_after_a_bounded_scan() {
        let mut drafts: Vec<String> = (0..MAX_SCAN_ENTRIES).map(|_| "nothing".to_string()).collect();
        drafts.push("hello".to_string());
        assert_eq!(local_suffix("hel", drafts.iter().map(String::as_str)), None);
    }

    #[test]
    fn the_screen_draws_the_suffix_after_the_caret_and_drops_it_on_escape() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        use crate::app::App;
        use crate::settings::Sources;
        use crate::settings::layers::LayerSet;

        let (_sender, receiver) = std::sync::mpsc::channel();
        let mut app = App::new("socket · session".to_string(), receiver);
        let directory = tempfile::tempdir().expect("a temporary directory");
        let global = directory.path().join("tui.toml");
        std::fs::write(&global, "[editor]\nsuggestions = true\n").expect("write the settings");
        let sources = Sources {
            global,
            project: None,
            configs: Vec::new(),
        };
        let mut warnings = Vec::new();
        let mut layers = LayerSet::new(&sources);
        layers.refresh(&mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        let (settings, resolve_warnings) = layers.resolve(|_: &str| None);
        assert!(resolve_warnings.is_empty(), "{resolve_warnings:?}");
        app.refresh_settings(&settings);
        app.history.record("hello world");
        for character in "hel".chars() {
            app.on_key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE));
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("a test terminal");
        crate::draw(&mut terminal, &mut app).expect("a frame");
        let screen = format!("{}", terminal.backend());
        assert!(screen.contains("hello world"), "the ghost was not drawn:\n{screen}");
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        crate::draw(&mut terminal, &mut app).expect("a frame");
        let screen = format!("{}", terminal.backend());
        assert!(!screen.contains("hello world"), "the ghost stayed:\n{screen}");
        assert_eq!(app.input.text(), "hel");
    }
}
