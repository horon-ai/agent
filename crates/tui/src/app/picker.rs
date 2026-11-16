//! The model and session pickers: opening them, filling them from what the
//! bridge reported, and turning a chosen row into the action it means.
//!
//! The screen itself ([`crate::overlays::picker`]) knows only rows; the meaning
//! of a row is decided here, where the rest of the client state is. A session
//! switch is the one flow that resets this state: the queue, the parked prompts,
//! the turn flag and the scroll offset all belong to the session that was open,
//! so they are cleared before the stored conversation is replayed.

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

    /// Open the session list for this directory.
    ///
    /// Refused while a turn is running: attaching elsewhere would leave the
    /// running turn's events arriving for a session the client no longer shows,
    /// and the queue it is still working through belongs to that turn.
    pub(crate) fn open_sessions(&mut self) -> Option<Action> {
        if self.pending_turn {
            self.transcript
                .push_notice("the turn is still running; /resume works once it ends");
            return None;
        }
        if self.disconnected {
            self.warn("the harness connection is gone; there is nothing to resume");
            return None;
        }
        self.picker = Some(Picker::loading(
            Kind::Sessions,
            "Resume session",
            "no sessions in this directory",
        ));
        Some(Action::Sessions)
    }

    /// Fill the open session list, marking the session in force.
    pub fn show_sessions(&mut self, sessions: Vec<SessionRow>) {
        let rows = sessions
            .into_iter()
            .map(|session| {
                Row::new(session.label, session.id)
                    .with_detail(session.detail)
                    .marked(session.current)
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
                Kind::Models => Some(Action::Model(Some(value))),
                Kind::Sessions => {
                    let label = picker.selected_label().unwrap_or(value.as_str()).to_string();
                    self.transcript.push_notice(format!("attaching to {label}"));
                    Some(Action::Attach(value))
                }
            },
        }
    }

    /// The bridge moved to another session: clear what belonged to the old one
    /// and replay the stored conversation.
    ///
    /// Nothing of the previous session survives on screen: the turn flag, the
    /// queue, the in-flight message, the parked prompts and the scroll offset
    /// would otherwise be read against a conversation they never described.
    /// The transcript keeps its display settings and gains the stored messages,
    /// which is why it is cleared rather than replaced.
    pub fn attach_session(&mut self, session: AttachedSession) {
        let session_id = session.session_id;
        self.description = session.description;
        self.session_id = Some(session_id.clone());
        self.pending_turn = false;
        self.in_flight = None;
        self.queue.clear();
        self.prompt = None;
        self.waiting_prompts.clear();
        self.resend_held = false;
        self.session_status = Some(session.status);
        self.model = session.model;
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
        if session.history.is_empty() {
            self.transcript.push_notice("the stored session has no messages");
        }
        self.transcript.push_notice(format!("resumed {session_id}"));
    }

    /// A list or attach request the harness refused.
    ///
    /// The list goes with it: a picker waiting for rows that will never come
    /// would swallow every key until the user found `esc`, so the error is
    /// reported once and the screen is usable again. The client itself stays
    /// up, since a refused list says nothing about the session it is attached
    /// to.
    pub fn request_failed(&mut self, message: &str) {
        if self.picker.as_ref().is_some_and(Picker::is_loading) {
            self.picker = None;
        }
        self.warn(message);
    }
}
