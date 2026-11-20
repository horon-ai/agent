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
        let provider = self.provider.clone();
        let mut rows: Vec<Row> = models
            .into_iter()
            .map(|model| {
                let marked = current.as_deref() == Some(model.as_str());
                let preview = model_preview(&model, provider.as_deref(), marked);
                Row::new(model.clone(), model).marked(marked).with_preview(preview)
            })
            .collect();
        if let Some(current) = &current
            && !rows.iter().any(|row| row.current)
        {
            let preview = model_preview(current, provider.as_deref(), true);
            rows.insert(
                0,
                Row::new(current.clone(), current.clone())
                    .marked(true)
                    .with_preview(preview),
            );
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
    pub(super) fn fill(&mut self, kind: Kind, rows: Vec<Row>) {
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
                // A choice made during a turn waits for it rather than freezing
                // the stream; the app decides that, not the list.
                Kind::Models => self.ask_model(&value),
                Kind::Efforts => self.ask_effort(&value),
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
                // A skill is the same: the invocation goes into the draft, with
                // the request still to be written after it.
                Kind::Skills => {
                    self.draft_skill(&value);
                    None
                }
                // The queue answers its own keys before this list sees them, so
                // no row is ever chosen from here; a value that arrives anyway
                // changes nothing rather than acting as some other kind of row.
                Kind::Queue => None,
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
        self.todos = Default::default();
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
        // The skills belong to the session too, and the bridge asks for the new
        // session's list right after this answer: until it arrives, a name the
        // session just left behind must not resolve.
        self.skills.clear();
        self.commands.set_skills(Vec::new());
        // The effort belongs to the session being attached to, so it is the
        // one the bridge read for it. Nothing of the session left behind
        // survives: a report that names no effort leaves it unknown until the
        // identity event that follows names one.
        self.reasoning_effort = session.effort;
        // A level the old session was asked for says nothing about the one being
        // attached to, and a change still waiting for the old turn has no turn
        // to wait for here.
        self.effort_requested = None;
        self.effort_after_turn = None;
        self.model_after_turn = None;
        // A steer still in flight is kept: its answer belongs to the session
        // being left, and its refusal is still worth showing. It stops holding
        // this session's `esc`, which reads the ids instead of clearing it.
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
        // A request that came back as a failure will never be answered, so the
        // level it asked for must not steer the cycle key.
        self.effort_requested = None;
        if self.picker.as_ref().is_some_and(Picker::is_loading) {
            self.picker = None;
        }
        self.warn(message);
    }
}

/// The lines the model list shows while one of its rows is selected.
///
/// The provider is the session's own: the catalog lists model ids without
/// repeating which provider serves them, and a session runs one provider at a
/// time. None of these lines is searchable — the filter matches model names,
/// and a preview says what a row means rather than being another way to find
/// it.
fn model_preview(model: &str, provider: Option<&str>, current: bool) -> Vec<String> {
    let mut lines = vec![format!("model: {model}")];
    if let Some(provider) = provider {
        lines.push(format!("provider: {provider}"));
    }
    lines.push(if current {
        "the current model of this session".to_string()
    } else {
        "not the current model".to_string()
    });
    lines.push(format!("enter switches this session to {model}"));
    lines
}
