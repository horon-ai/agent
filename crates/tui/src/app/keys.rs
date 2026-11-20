//! Keystroke, paste and mouse handling: the key map in force decides what a key
//! means, a live prompt or overlay gets the key first, and nothing is swallowed.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

use super::{Action, App};
use crate::keys::{self, Key};
use crate::overlays::settings::Outcome;
use crate::prompts::{Prompt, Response};
use crate::scroll;
use crate::transcript::Decision;

impl App {
    /// Translate one keystroke into an action, if it produced one.
    ///
    /// A live prompt takes every key first: a half-answered approval must not
    /// let a stray character into the message being drafted. The key map
    /// decides what each key means; a printable character that no action claims
    /// is inserted, so a text field cannot lose a letter to a binding.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        // The popup is read against the draft on screen, before this key
        // changes it, and brought up to date again after it: a key that moved
        // the draft leaves a list that belongs to the text it left behind.
        self.sync_slash();
        self.sync_suggestions();
        let action = self.handle_key(key);
        self.sync_slash();
        self.sync_suggestions();
        action
    }

    /// What one keystroke does, with the popup already in line with the draft.
    fn handle_key(&mut self, key: KeyEvent) -> Option<Action> {
        // The repaint key answers before every surface, and it takes no other
        // key from them: it changes nothing on screen, so a picker, the
        // settings screen or a prompt must not swallow it, and none of their
        // own keys may be claimed in exchange. What it throws away is the
        // frame the terminal holds, not the state it shows.
        if Key::from_event(key)
            .is_some_and(|pressed| self.bindings.resolve_editing(&pressed) == Some(keys::ids::APP_REPAINT))
        {
            self.request_repaint();
            return None;
        }
        // A list sits at the top of the precedence order: while it is open no
        // key reaches the settings screen, a prompt, the slash popup, the
        // scroll keys, the history or the draft underneath it.
        if self.picker.is_some() {
            return self.picker_key(key);
        }
        // An open overlay owns every key: a letter meant for a value must not
        // reach the message being drafted underneath it.
        if let Some(mut overlay) = self.overlay.take() {
            let outcome = overlay.on_key(key);
            match outcome {
                Outcome::Handled => self.overlay = Some(overlay),
                Outcome::Closed => self.pending_change = None,
                Outcome::Change(change) => {
                    self.pending_change = Some(change);
                    self.overlay = Some(overlay);
                }
            }
            return None;
        }
        if let Some(prompt) = self.prompt.as_mut() {
            match prompt.on_key_with(&self.bindings, key) {
                Some(Response::Approval {
                    request_id,
                    approved,
                    always,
                }) => {
                    // The prompt comes down before the answer is sent: the
                    // daemon refuses a second answer to the same request, and a
                    // prompt still on screen invites one.
                    let covered = self.take_prompt();
                    // The row says what the answer was; the event says the
                    // same when it arrives, and deciding twice is harmless.
                    let decision = match (approved, always) {
                        (true, true) => Decision::AllowedAlways,
                        (true, false) => Decision::Allowed,
                        (false, _) => Decision::Denied,
                    };
                    self.transcript.decide_request(&request_id, decision);
                    self.scroll.reflow();
                    self.transcript
                        .push_notice(approval_answer_line(&covered, approved, always));
                    return Some(Action::Approval {
                        request_id,
                        approved,
                        always,
                    });
                }
                Some(Response::Ask { ask_id, answers }) => {
                    let covered = answers.len();
                    self.take_prompt();
                    self.transcript.push_notice(ask_answer_line(covered));
                    return Some(Action::Ask { ask_id, answers });
                }
                None => {
                    // The prompt handles its own keys first. Ctrl+C and an
                    // explicit quit binding can still leave the parked prompt.
                    let pressed = Key::from_event(key);
                    let bound = pressed
                        .as_ref()
                        .and_then(|pressed| self.bindings.resolve_editing(pressed));
                    match bound {
                        Some(keys::ids::APP_CANCEL) if self.pending_turn => {
                            self.withdraw_prompts("the turn was cancelled");
                            return self.cancel_turn();
                        }
                        Some(keys::ids::APP_QUIT) if pressed.is_some_and(is_ctrl_d) => return None,
                        Some(keys::ids::APP_CANCEL | keys::ids::APP_QUIT) => return Some(Action::Quit),
                        // The transcript still scrolls while a prompt waits:
                        // reading further back neither answers the prompt nor
                        // reaches the draft, and these keys are not in the
                        // prompt's scope, so the prompt stays answerable.
                        Some(action) => {
                            self.apply_scroll(action);
                            return None;
                        }
                        _ => return None,
                    }
                }
            }
        }

        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let pressed = Key::from_event(key);
        let bound = pressed.and_then(|pressed| self.bindings.resolve_editing(&pressed));
        // The popup sits above the editor and below the prompt, so while it is
        // open the keys that pick a row are its own: `up` and `down` move the
        // selection instead of the history, `esc` closes the list rather than
        // interrupting the turn, and `tab` fills the row it has chosen. Every
        // other key falls through to the editor, which is what keeps the draft
        // editable with the list on screen.
        if self.slash.is_open() {
            // `esc` closes the list whatever `app.interrupt` is bound to: the
            // list is the topmost surface, and a key that means "not this" must
            // not leave it open because a file moved the interrupt key.
            if key.code == KeyCode::Esc || bound == Some(keys::ids::APP_INTERRUPT) {
                self.slash.dismiss();
                return None;
            }
            if let Some(action) = bound {
                match action {
                    keys::ids::EDITOR_HISTORY_PREV => {
                        self.slash.move_by(-1);
                        return None;
                    }
                    keys::ids::EDITOR_HISTORY_NEXT => {
                        self.slash.move_by(1);
                        return None;
                    }
                    // `shift+tab` walks the effort ladder everywhere the draft
                    // is not choosing a command. With the popup open `tab` is
                    // the popup's own key, so its shifted form completes the
                    // row on screen rather than changing the effort under the
                    // list the user is reading.
                    keys::ids::EDITOR_COMPLETE | keys::ids::APP_CYCLE_EFFORT => {
                        self.complete();
                        return None;
                    }
                    _ => {}
                }
            }
        }
        // A ghost suggestion sits above the editor and below the popup: the
        // accept key fills the rest of the draft, and `esc` puts the offer away
        // instead of interrupting the turn. Every other key reaches the editor,
        // and the sync after this key withdraws an offer the changed draft no
        // longer matches.
        if self.suggestion_available() {
            if key.code == KeyCode::Esc || bound == Some(keys::ids::APP_INTERRUPT) {
                self.suggestions.dismiss();
                return None;
            }
            if bound == Some(keys::ids::EDITOR_ACCEPT_SUGGESTION) {
                self.accept_suggestion();
                return None;
            }
        }
        match bound {
            Some(keys::ids::APP_SUBMIT) => {
                // `editor.submit_on_enter = false` disconnects the send key, so
                // the key that no longer sends builds the draft instead: that is
                // how a multi-line message is typed without a paste.
                let submits = self.settings.boolean("editor.submit_on_enter").unwrap_or(true);
                if !submits {
                    self.input.newline();
                    return None;
                }
                let text = self.input.take();
                // A backslash at the end of the draft asks for another line
                // rather than a send: the escape comes off and the draft breaks
                // there. Nothing is remembered or queued, because the message
                // is not finished and the next Enter is the one that sends it.
                if let Some(kept) = continued_draft(&text) {
                    self.input.set_text(&kept);
                    self.input.newline();
                    return None;
                }
                if text.trim().is_empty() {
                    return None;
                }
                // The draft is remembered before it is resolved: a slash line
                // that turns out to be an error is still a line the user typed.
                self.history.record(&text);
                return self.submit(&text);
            }
            Some(keys::ids::EDITOR_COMPLETE) => {
                self.complete();
                return None;
            }
            Some(keys::ids::EDITOR_HISTORY_SEARCH) => {
                self.open_history_search();
                return None;
            }
            Some(keys::ids::APP_CANCEL) => {
                // Cancel whenever a turn is actually running, which includes
                // the stretch after a mid-turn error: the daemon has not
                // finished the turn yet, so leaving here would drop it.
                if self.pending_turn {
                    return self.cancel_turn();
                }
                return Some(Action::Quit);
            }
            Some(keys::ids::APP_INTERRUPT) => {
                // `esc` interrupts and never leaves: a stray key on an idle
                // client must not end the session. A queued message is what the
                // turn can still take, so it goes into the running turn instead
                // of the turn being stopped; with nothing queued it cancels.
                if self.pending_turn {
                    return self.steer_turn();
                }
                return None;
            }
            Some(keys::ids::APP_QUIT) => {
                // `ctrl+d` on this key answers the draft before the session:
                // with text in front of it, the cursor eats the character it
                // sits on, and at the end of the draft there is nothing to eat,
                // so the draft is kept. Only an empty draft leaves, and only
                // while no turn is running: a key meant for the draft must not
                // end work in progress. A file that moves `app.quit` to another
                // key keeps the plain leave there, since this branch then
                // answers a key that is not `ctrl+d`.
                if pressed.is_some_and(is_ctrl_d) {
                    if !self.input.is_empty() {
                        self.history.reset();
                        self.input.delete();
                        return None;
                    }
                    if self.pending_turn {
                        return None;
                    }
                }
                return Some(Action::Quit);
            }
            Some(keys::ids::APP_EXPAND_TOOL) => {
                self.expand_tool();
                return None;
            }
            Some(keys::ids::APP_TOGGLE_TOOL_ACTIVITY) => {
                self.toggle_tool_activity();
                return None;
            }
            Some(keys::ids::APP_CYCLE_EFFORT) => return self.cycle_effort(),
            Some(keys::ids::APP_PICK_MODEL) => return self.pick_model(),
            Some(keys::ids::APP_TOGGLE_THINKING) => {
                self.toggle_thinking();
                return None;
            }
            // Every edit ends a walk through the history: the draft on screen
            // is being changed, and a later `down` must not replace the change
            // with the entry the walk started from.
            Some(keys::ids::EDITOR_BACKSPACE) => {
                self.history.reset();
                self.input.backspace();
            }
            Some(keys::ids::EDITOR_DELETE) => {
                self.history.reset();
                self.input.delete();
            }
            Some(keys::ids::EDITOR_KILL_TO_START) => {
                self.history.reset();
                self.input.kill_to_start();
            }
            Some(keys::ids::EDITOR_KILL_TO_END) => {
                self.history.reset();
                self.input.kill_to_end();
            }
            Some(keys::ids::EDITOR_DELETE_WORD) => {
                self.history.reset();
                self.input.delete_word();
            }
            Some(keys::ids::EDITOR_LEFT) => self.input.left(),
            Some(keys::ids::EDITOR_RIGHT) => self.input.right(),
            Some(keys::ids::EDITOR_HOME) => self.input.home(),
            Some(keys::ids::EDITOR_END) => self.input.end(),
            Some(keys::ids::EDITOR_WORD_LEFT) => self.input.word_left(),
            Some(keys::ids::EDITOR_WORD_RIGHT) => self.input.word_right(),
            Some(keys::ids::EDITOR_NEWLINE) => self.input.newline(),
            Some(keys::ids::EDITOR_HISTORY_PREV) => self.history_previous(),
            Some(keys::ids::EDITOR_HISTORY_NEXT) => self.history_next(),
            // Whatever else the map names is the transcript's own scrolling:
            // the view moves and the draft, its history and the caret stay
            // exactly where they were.
            _ => {
                if let Some(action) = bound {
                    self.apply_scroll(action);
                }
            }
        }
        match key.code {
            KeyCode::Char(character) if !control => {
                self.history.reset();
                self.input.insert(character);
            }
            _ => {}
        }
        None
    }

    /// Take one paste into the draft.
    ///
    /// A paste never submits and never answers anything: pasted text arriving
    /// at a prompt would let a copied `y` approve a tool call, so a paste while
    /// a prompt or the settings screen is open is dropped, exactly as its
    /// keystrokes would be. Otherwise the whole text lands in the draft as one
    /// edit, with its line breaks kept.
    pub fn on_paste(&mut self, text: &str) {
        if self.prompt.is_some() || self.overlay.is_some() || self.picker.is_some() {
            return;
        }
        self.history.reset();
        self.input.insert_text(text);
    }

    /// Move the transcript view for one of the scroll actions.
    ///
    /// Answers whether the action moved it, so a caller holding a key back can
    /// tell a scroll key from one it has no use for. The offset is not bounded
    /// here: the next frame measures the content and clamps it, which is what
    /// makes a page past the first row stop at the first row.
    fn apply_scroll(&mut self, action: &str) -> bool {
        match action {
            keys::ids::APP_SCROLL_PAGE_UP => self.scroll.up(self.page_rows()),
            keys::ids::APP_SCROLL_PAGE_DOWN => self.scroll.down(self.page_rows()),
            keys::ids::APP_SCROLL_UP => self.scroll.up(1),
            keys::ids::APP_SCROLL_DOWN => self.scroll.down(1),
            keys::ids::APP_SCROLL_BOTTOM => self.scroll.to_bottom(),
            _ => return false,
        }
        self.select_tool_in_view();
        true
    }

    /// Make the tool call on screen the one the expand key acts on.
    ///
    /// The view is the selection: scrolling to an earlier result makes that
    /// result the one `ctrl+o` opens, and following the end selects the newest
    /// kept call again. The row arithmetic lives here because the scrollback
    /// owns the heights and the offset; the transcript owns which item sits at
    /// a row.
    fn select_tool_in_view(&mut self) {
        if self.scroll.is_following() {
            self.transcript.select_newest_tool();
            return;
        }
        // Without a measurement from a held frame there is no row to map, and
        // the selection is left where it was rather than guessed.
        let heights = self.scroll.heights().map(|heights| heights.to_vec());
        let (Some(heights), Some(view)) = (heights, self.viewport_height) else {
            return;
        };
        let view = usize::from(view.saturating_sub(1).max(1));
        let open = self.scroll.open_heights();
        let total = heights.iter().sum::<usize>() + open[0] + open[1];
        let top = total.saturating_sub(self.scroll.offset()).saturating_sub(view);
        self.transcript.select_tool_at(&heights, top, view);
    }

    /// Draw or hide the record of finished tool calls for this session.
    ///
    /// The setting is persisted from `/settings`; this key is the quick look
    /// the conversation needs while a turn runs, so it changes what this client
    /// draws now and leaves the file alone.
    fn toggle_tool_activity(&mut self) {
        let show = !self.transcript.show_tool_activity();
        self.transcript.set_tool_activity(show);
        // Rows appeared or went away without output arriving: the held view
        // keeps the row it was reading instead of jumping.
        self.scroll.reflow();
        self.select_tool_in_view();
    }

    /// Show the whole output of the selected cut tool call, or the preview.
    ///
    /// With nothing to expand the key says so, because a key that silently does
    /// nothing reads as a broken key rather than an empty one.
    fn expand_tool(&mut self) {
        // The frame just drawn knows where the view sits; a scroll key pressed
        // before that frame cannot select a result it had no measurement for,
        // so the press settles the selection again here.
        self.select_tool_in_view();
        if self.transcript.toggle_expand() {
            // The height of one block just changed without output arriving:
            // the held view is recounted and moved by the difference, which
            // keeps the row on screen where it was instead of jumping to the
            // end of the conversation.
            self.scroll.reflow();
        } else {
            self.transcript.push_notice(crate::tool_expand::NOTHING_TO_EXPAND);
        }
    }

    /// Rows one scroll page covers: the transcript pane as it was last drawn.
    ///
    /// A held view spends the pane's bottom row on the follow indicator, so a
    /// page is never taller than the rows it can actually move.
    fn page_rows(&self) -> usize {
        usize::from(self.viewport_height.unwrap_or(24).saturating_sub(1).max(1))
    }

    /// Take a mouse event into the transcript view or the draft.
    ///
    /// The wheel is used only while `transcript.mouse_scroll` holds the capture
    /// that makes the terminal report it, and the settings screen covers the
    /// pane the wheel would move. A left click in the draft places the caret at
    /// the cell it covered on the last frame; a click elsewhere on screen has
    /// nothing to mean.
    pub fn on_mouse(&mut self, mouse: MouseEvent) {
        if !self.mouse_scroll || self.overlay.is_some() || self.picker.is_some() {
            return;
        }
        match mouse.kind {
            MouseEventKind::ScrollUp => self.scroll.up(scroll::WHEEL_ROWS),
            MouseEventKind::ScrollDown => self.scroll.down(scroll::WHEEL_ROWS),
            MouseEventKind::Down(MouseButton::Left) => {
                self.input.place_caret(mouse.column, mouse.row);
                return;
            }
            _ => return,
        }
        self.select_tool_in_view();
    }

    /// Move up: through the lines of the draft, then back through the history.
    fn history_previous(&mut self) {
        if self.input.up() {
            return;
        }
        let current = self.input.text();
        if let Some(entry) = self.history.previous(&current) {
            self.input.set_text(&entry);
        }
    }

    /// Move down: through the lines of the draft, then forward through the
    /// history and back to the draft the walk started from.
    fn history_next(&mut self) {
        if self.input.down() {
            return;
        }
        if let Some(entry) = self.history.forward() {
            self.input.set_text(&entry);
        }
    }
}

/// Whether a keystroke is the shipped `ctrl+d`.
///
/// Only this spelling carries the draft-aware meaning; `app.quit` on any other
/// key leaves exactly as it always did. Shift on a character never reaches
/// [`Key`] (the terminal sends the character itself), and alt makes it another
/// key, which keeps `alt+ctrl+d` out of the draft branch.
fn is_ctrl_d(key: Key) -> bool {
    key.code == keys::Code::Char('d') && key.ctrl && !key.alt
}

/// The draft with the backslash that asks for another line taken off.
///
/// A draft ends in a continuation when its last character is a backslash that is
/// not itself escaped, so an odd run of trailing backslashes breaks the line and
/// an even run is that many literal backslashes and sends the draft as typed. A
/// backslash anywhere else is text, and so is a space after the last backslash:
/// the run has to reach the end of the draft.
fn continued_draft(draft: &str) -> Option<String> {
    let trailing = draft.chars().rev().take_while(|character| *character == '\\').count();
    if trailing.is_multiple_of(2) {
        return None;
    }
    draft.strip_suffix('\\').map(str::to_string)
}

/// The line the transcript keeps after an approval is answered.
///
/// An answer is recorded, not resumed. A tool call outside an ACP permission
/// bridge is refused with "Nothing was executed", and only the model calling the
/// same tool again runs it: the gate reads the recorded decision on the retry
/// and then removes the request (`crates/app-core/src/tool/permission_bridge.rs`,
/// module doc; `Registry::approval_refusal` in `crates/app-core/src/tool/mod.rs`).
/// A line that just said "approved" would read as "it ran".
///
/// `allow_always` is not a rule either: the daemon has no persistent always
/// store, and records the answer as one approval
/// (`crates/app-core/src/server/client_ask.rs`).
fn approval_answer_line(covered: &Option<Prompt>, approved: bool, always: bool) -> String {
    let action = match covered {
        Some(Prompt::Approval(approval)) => approval.action.as_str(),
        Some(Prompt::Ask(_)) | None => "the call",
    };
    match (approved, always) {
        (true, true) => format!(
            "you allowed {action}: recorded once (the daemon keeps no always-rule), so the model has to call it again"
        ),
        (true, false) => format!("you allowed {action}: the model has to call it again before it runs"),
        (false, _) => format!("you denied {action}: the call will not run"),
    }
}

/// The line the transcript keeps after a question set is answered.
///
/// The ask tool waits for the answer on the connection that advertised
/// `supports_ask_requests`, so an answer releases the call rather than being
/// remembered for a retry (`crates/app-core/src/server/client_ask.rs`).
fn ask_answer_line(answers: usize) -> String {
    let counted = if answers == 1 {
        "1 answer".to_string()
    } else {
        format!("{answers} answers")
    };
    format!("you answered the ask tool ({counted}): the call resumes")
}
