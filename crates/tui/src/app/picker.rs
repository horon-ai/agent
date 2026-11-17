//! The pickers: the model and session lists the harness owns, the search over
//! this client's own drafts, and what a chosen row of any of them means.
//!
//! The screen itself ([`crate::overlays::picker`]) knows only rows; the meaning
//! of a row is decided here, where the rest of the client state is. A session
//! switch or a new session is the one flow that resets this state: the queue, the
//! parked prompts, the turn flag and the scroll offset all belong to the session
//! that was open, so they are cleared before the stored conversation is replayed.

use crossterm::event::KeyEvent;

use super::{Action, App};
use crate::overlays::picker::{Kind, Outcome, Picker, Row};
use crate::scroll::Scroll;
use crate::sdk_bridge::{AttachedSession, SessionRow};

impl App {
    /// Open the model list; the bridge fills it when the catalog answers.
    pub(crate) fn open_models(&mut self) -> Option<Action> {
        self.picker = Some(Picker::loading(
            Kind::Models,
            "Select model",
            "the harness reported no models",
        ));
        Some(Action::Models)
    }

    /// Fill the open model list, marking the one in force.
    ///
    /// A catalog that does not name the model serving the session still gets a
    /// row for it: the list has to say what is in force, or the user cannot
    /// tell whether the top row is a change or the state they are already in.
    pub fn show_models(&mut self, models: Vec<String>, current: Option<String>) {
        let mut rows: Vec<Row> = models
            .into_iter()
            .map(|model| {
                let marked = current.as_deref() == Some(model.as_str());
                Row::new(model.clone(), model).marked(marked)
            })
            .collect();
        if let Some(current) = &current
            && !rows.iter().any(|row| row.current)
        {
            rows.insert(0, Row::new(current.clone(), current.clone()).marked(true));
        }
        self.fill(Kind::Models, rows);
    }

    /// Fill the open session list, marking the session in force.
    ///
    /// The preview lines travel with the row: they are built from the stored
    /// history the bridge already read, so drawing the list costs one map here
    /// and no request at all. The head line is drawn but not searched: it is
    /// the directory and the time, which every row of one directory shares, so
    /// a filter on the path would otherwise keep the whole list.
    pub fn show_sessions(&mut self, sessions: Vec<SessionRow>) {
        let rows = sessions
            .into_iter()
            .map(|session| {
                let preview = session.preview;
                let mut search = Vec::new();
                if let Some((_head, conversation)) = preview.split_first() {
                    search.extend_from_slice(conversation);
                }
                Row::new(session.label, session.id)
                    .with_detail(session.detail)
                    .marked(session.current)
                    .with_preview(preview)
                    .with_search(search)
            })
            .collect();
        self.fill(Kind::Sessions, rows);
    }

    /// Replace the rows of the open list, if it is the one that asked.
    fn fill(&mut self, kind: Kind, rows: Vec<Row>) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        if picker.kind() != kind {
            return;
        }
        picker.fill(rows);
    }

    /// Route one keystroke to the open list.
    ///
    /// The list is taken out of the app while the key is read, so a key that
    /// closes it cannot leave a half-updated screen behind.
    pub(crate) fn picker_key(&mut self, key: KeyEvent) -> Option<Action> {
        let mut picker = self.picker.take()?;
        match picker.on_key(key) {
            Outcome::Handled => {
                self.picker = Some(picker);
                None
            }
            Outcome::Closed => None,
            Outcome::Selected(value) => match picker.kind() {
                Kind::Models => Some(Action::Model(value)),
                Kind::Sessions => {
                    let label = picker.selected_label().unwrap_or(value.as_str()).to_string();
                    self.transcript.push_notice(format!("attaching to {label}"));
                    Some(Action::Attach(value))
                }
                // A recalled draft is an edit, not a send: the picker closes and
                // the editor holds the text, and no action leaves the client.
                Kind::History => {
                    self.recall_draft(&value);
                    None
                }
            },
        }
    }

    /// The bridge moved to another session: clear what belonged to the old one
    /// and replay the stored conversation.
    ///
    /// Nothing of the previous session survives on screen: the turn flag, the
    /// queue, the in-flight message, the parked prompts, the expanded tool call
    /// and the scroll offset would otherwise be read against a conversation they
    /// never described. The transcript keeps its display settings and gains the
    /// stored messages, which is why it is cleared rather than replaced.
    ///
    /// `started` says the session was created rather than resumed: its empty
    /// history is then the state it begins in, not a stored conversation with
    /// nothing in it, and the notice says which of the two happened.
    pub fn attach_session(&mut self, session: AttachedSession, started: bool) {
        let session_id = session.session_id;
        self.description = session.description;
        self.session_id = Some(session_id.clone());
        self.session_title = session.title;
        self.new_session_pending = false;
        self.pending_turn = false;
        self.in_flight = None;
        self.queue.clear();
        self.prompt = None;
        self.waiting_prompts.clear();
        self.resend_held = false;
        self.session_status = Some(session.status);
        self.model = session.model;
        // The effort belongs to the model that reported it, and a resumed
        // session may be served by another one: the next ModelInfo fills it in.
        self.reasoning_effort = None;
        self.provider = None;
        self.tokens = None;
        self.scroll = Scroll::new();
        self.slash.dismiss();
        self.transcript.clear();
        for message in &session.history {
            match message.role.as_str() {
                "user" => self.transcript.push_user(message.content.clone()),
                "assistant" => self.transcript.push_assistant(message.content.clone()),
                // Tool rows are the daemon's record of what ran, not the
                // conversation; the answers they produced are in the assistant
                // messages that follow them.
                _ => {}
            }
        }
        if session.history.is_empty() && !started {
            self.transcript.push_notice("the stored session has no messages");
        }
        self.transcript.push_notice(if started {
            format!("started a new session {session_id}")
        } else {
            format!("resumed {session_id}")
        });
    }

    /// A list or attach request the harness refused.
    ///
    /// The list goes with it: a picker waiting for rows that will never come
    /// would swallow every key until the user found `esc`, so the error is
    /// reported once and the screen is usable again. The client itself stays
    /// up, since a refused list says nothing about the session it is attached
    /// to. The pending-session flag goes too: a create that failed must not
    /// make the next successful attach claim it started a session.
    pub fn request_failed(&mut self, message: &str) {
        self.new_session_pending = false;
        if self.picker.as_ref().is_some_and(Picker::is_loading) {
            self.picker = None;
        }
        self.warn(message);
    }
}
