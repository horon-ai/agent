//! The pickers: the model and session lists the harness owns, the search over
//! this client's own drafts, and what a chosen row of any of them means.
//!
//! The screen itself ([`crate::overlays::picker`]) knows only rows; the meaning
//! of a row is decided here, where the rest of the client state is. A session
//! switch or a new session is the one flow that resets this state: the queue, the
//! parked prompts, the turn flag and the scroll offset all belong to the session
//! that was open, so they are cleared before the stored conversation is replayed.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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
    ///
    /// The answer is also the catalog the cycle keys check `models.shortlist`
    /// against, so checking an entry costs no second request.
    pub fn show_models(&mut self, models: Vec<String>, current: Option<String>) {
        let mut rows: Vec<Row> = {
            let provider = self.provider.as_deref();
            let shortlist = &self.model_shortlist;
            models
                .iter()
                .map(|model| {
                    let marked = current.as_deref() == Some(model.as_str());
                    let preview = model_preview(model.as_str(), provider, marked, shortlist);
                    Row::new(model.as_str(), model.as_str())
                        .marked(marked)
                        .with_preview(preview)
                })
                .collect()
        };
        if let Some(current) = &current
            && !rows.iter().any(|row| row.current)
        {
            let preview = model_preview(current, self.provider.as_deref(), true, &self.model_shortlist);
            rows.insert(
                0,
                Row::new(current.clone(), current.clone())
                    .marked(true)
                    .with_preview(preview),
            );
        }
        // The answer is also the catalog the cycle keys check
        // `models.shortlist` against, so checking an entry costs no second
        // request. An empty list is the harness saying it has nothing to
        // switch to yet rather than a catalog: the next press reads it again
        // instead of refusing every entry on the strength of that answer.
        self.model_catalog = models;
        self.model_catalog_loaded = !self.model_catalog.is_empty();
        if !self.model_catalog_loaded && self.model_cycle_pending.take().is_some() {
            self.warn("the harness returned no models to cycle");
        }
        self.fill(Kind::Models, rows);
    }

    /// `app.cycle_model` and `app.cycle_model_back`: step through
    /// `models.shortlist`.
    ///
    /// The walk is the configured order, forward wrapping past the last entry
    /// and backward wrapping past the first. It starts from the model already
    /// in force, so the first press moves off it rather than onto it; a model
    /// waiting for the running turn or an answer counts as in force, which is
    /// what makes two quick presses walk two entries instead of asking for the
    /// same one twice.
    ///
    /// The catalog decides which entries can be asked for: one it does not
    /// list is skipped and named once, because a switch the harness would
    /// refuse is better said than sent. A press before the catalog has been
    /// read asks for it instead and walks once it lands, so the status line
    /// moves only when a report names the new model and a refused entry never
    /// replaces the one in force.
    pub(crate) fn cycle_model(&mut self, forward: bool) -> Option<Action> {
        if self.disconnected {
            self.warn("the harness connection is gone; the model cannot change");
            return None;
        }
        if self.model_shortlist.is_empty() {
            self.warn("models.shortlist is empty; add model ids under [models] in tui.toml to cycle them");
            return None;
        }
        // An entry is only asked for once the harness has listed its models.
        // Without that answer the client cannot tell a name the provider has
        // from one it does not, and a switch it guessed would be reported as a
        // failure the user never asked for. The press is remembered, the list
        // is read once, and the walk follows the answer in the same direction.
        if !self.model_catalog_loaded {
            self.model_cycle_pending = Some(forward);
            return Some(Action::Models);
        }
        self.walk_model(forward)
    }

    /// The walk itself, once the catalog says which entries the harness has.
    fn walk_model(&mut self, forward: bool) -> Option<Action> {
        // One list of the entries the harness lists and one of those it does
        // not, owned rather than borrowed: the notices below change this
        // state, and the walk has to read it as it was when the key arrived.
        let mut supported: Vec<String> = Vec::with_capacity(self.model_shortlist.len());
        let mut skipped: Vec<String> = Vec::new();
        for model in &self.model_shortlist {
            if self.model_catalog.iter().any(|listed| listed == model.as_str()) {
                supported.push(model.clone());
            } else {
                skipped.push(model.clone());
            }
        }
        if supported.is_empty() {
            self.warn(format!(
                "none of the models in models.shortlist is in the harness catalog: {}; open the \
                 model list to see the models it offers",
                join_names(&skipped)
            ));
            return None;
        }
        // A press that skips the same entries as the last one stays quiet: the
        // notice is about the file, and repeating it on every step would bury
        // the model the press is changing to.
        if skipped != self.model_shortlist_skipped {
            if !skipped.is_empty() {
                self.transcript.push_notice(format!(
                    "models.shortlist: the harness does not list {}; skipped",
                    join_names(&skipped)
                ));
            }
            self.model_shortlist_skipped = skipped;
        }
        // A waiting change is the newest intent, then one already sent, then
        // the model the harness last reported.
        let current = self
            .model_after_turn
            .as_deref()
            .or(self.model_requested.as_deref())
            .or(self.model.as_deref());
        let start = current.and_then(|model| supported.iter().position(|entry| entry == model));
        let next = match (start, forward) {
            (Some(index), true) => &supported[(index + 1) % supported.len()],
            (Some(index), false) => &supported[(index + supported.len() - 1) % supported.len()],
            // A model the shortlist does not hold is exactly when there is no
            // list position to step from: the walk starts at the near end.
            (None, true) => &supported[0],
            (None, false) => &supported[supported.len() - 1],
        };
        self.ask_model(next)
    }

    /// The walk a cycle press asked for while the catalog was being read.
    ///
    /// Called by the event loop once the answer landed, so the press becomes
    /// one switch in the direction it named instead of a request for a list
    /// the user never sees.
    pub(crate) fn take_model_cycle(&mut self) -> Option<Action> {
        // Between the keypress and its reply, the catalog is still unread.
        if !self.model_catalog_loaded {
            return None;
        }
        // Taken before the walk: a refused entry reports a failure, and the
        // press has already been answered by then.
        let forward = self.model_cycle_pending.take()?;
        self.walk_model(forward)
    }

    /// Read `models.shortlist` into the list the cycle keys walk.
    ///
    /// A repeated or blank entry is one mistake in a file, not a reason to
    /// refuse the other entries: it is named once, when the file changes, and
    /// dropped, so the walk never visits one model twice.
    pub(crate) fn apply_model_shortlist(&mut self, listed: &[String]) {
        if listed == self.model_shortlist_source.as_slice() {
            return;
        }
        self.model_shortlist_source = listed.to_vec();
        let mut names: Vec<String> = Vec::new();
        for raw in listed {
            let name = raw.trim();
            if name.is_empty() {
                self.warn("models.shortlist has an empty entry; ignored");
                continue;
            }
            if names.iter().any(|kept| kept == name) {
                self.warn(format!(
                    "models.shortlist names `{name}` twice; the later entry is ignored"
                ));
                continue;
            }
            names.push(name.to_string());
        }
        self.model_shortlist = names;
        self.model_shortlist_skipped.clear();
    }

    /// Fill the open session list, marking the session in force.
    ///
    /// The preview lines travel with the row: they are built from the stored
    /// history the bridge already read, so drawing the list costs one map here
    /// and no request at all. The head line is drawn but not searched: it is
    /// the directory and the time, which every row of one directory shares, so
    /// a filter on the path would otherwise keep the whole list.
    pub fn show_sessions(&mut self, sessions: Vec<SessionRow>) {
        self.fill_session_rows(Kind::Sessions, sessions);
    }

    pub(crate) fn show_archived_sessions(&mut self, sessions: Vec<SessionRow>) {
        self.fill_session_rows(Kind::ArchivedSessions, sessions);
    }

    pub(crate) fn open_archived_sessions(&mut self) -> Option<Action> {
        self.picker = Some(Picker::loading(
            Kind::ArchivedSessions,
            "Archived sessions",
            "no archived sessions in this directory",
        ));
        Some(Action::ArchivedSessions)
    }

    fn fill_session_rows(&mut self, kind: Kind, sessions: Vec<SessionRow>) {
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
        self.fill(kind, rows);
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
        if picker.kind() == Kind::Sessions && key.code == KeyCode::Char('a') && key.modifiers == KeyModifiers::CONTROL {
            let Some(row) = picker.selected_row() else {
                self.picker = Some(picker);
                return None;
            };
            if row.current {
                self.picker = Some(picker);
                self.warn("leave the current session before archiving it");
                return None;
            }
            return Some(Action::Archive(row.value.clone()));
        }
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
                Kind::ArchivedSessions => Some(Action::Restore(value)),
                // A recalled draft is an edit, not a send: the picker closes and
                // the editor holds the text, and no action leaves the client.
                Kind::History => {
                    self.recall_draft(&value);
                    None
                }
                // A boundary is a stored message id the daemon resolves; the
                // first row may instead name the session this branch came
                // from, which is an ordinary attach back.
                Kind::Forks => {
                    let label = picker.selected_label().unwrap_or(value.as_str()).to_string();
                    self.choose_fork(&value, &label)
                }
                Kind::Skills => {
                    self.draft_skill(&value);
                    None
                }
                // The chosen text leaves through the same queue the copy keys
                // use, so the list and the keys cannot carry different bytes.
                Kind::Copies => {
                    self.copy_selection(&value);
                    None
                }
                // The queue answers its own keys before this list sees them, so
                // no row is ever chosen from here; a value that arrives anyway
                // changes nothing rather than acting as some other kind of row.
                // The job list is the same shape: `delete` cancels, and the
                // generic selector never runs for it.
                Kind::Queue | Kind::Jobs => None,
                // A link is the one row that leaves the client: the reader
                // pressed enter on it, so it goes to the platform handler and
                // the list closes without an action for the bridge.
                Kind::Links => {
                    self.open_link(&value);
                    None
                }
            },
        }
    }

    /// Open one chosen link through the platform's own handler.
    ///
    /// Only an explicit enter on a row reaches this, and the URL is checked
    /// again inside: a row keeps the string it was built from, and the list may
    /// outlive the answer that supplied it.
    pub(crate) fn open_link(&mut self, url: &str) {
        match crate::links::launch(url) {
            Ok(()) => self.transcript.push_notice(format!("opening {url}")),
            Err(error) => self.warn(format!("links: {error}")),
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
        // A call id is scoped to the session that started it, so neither the
        // job rows nor an open job list may follow the client across.
        self.clear_background_jobs();
        self.todos = Default::default();
        self.session_title = session.title;
        self.new_session_pending = false;
        // The branch origin belongs to the session being left; a fork answer
        // records its own right after this, in the handler that received it.
        self.fork_origin = None;
        self.pending_turn = false;
        // A session the user switched to is a client they are watching, so an
        // outcome of the session before it is not worth a tab mark.
        self.note_user_attention();
        self.in_flight = None;
        self.queue.clear();
        self.prompt = None;
        self.waiting_prompts.clear();
        // A capture belongs to the session being left, and the consent was
        // given for that session: both go with it, so its audio cannot become
        // a draft in the session being attached.
        self.voice.forget();
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
        // The model a request named belongs to the session being left, and the
        // catalog was read for its provider: neither describes the one being
        // attached to, so the next cycle starts from this session's model and
        // the next model list is the one that fills the catalog.
        self.model_requested = None;
        self.model_catalog.clear();
        self.model_catalog_loaded = false;
        self.model_cycle_pending = None;
        // A steer still in flight is kept: its answer belongs to the session
        // being left, and its refusal is still worth showing. It stops holding
        // this session's `esc`, which reads the ids instead of clearing it.
        self.provider = None;
        self.tokens = None;
        // The spending belongs to the session left behind: its totals, its
        // route's window, and the estimate for it would all describe a
        // conversation the client is no longer following. The bridge reads the
        // new session's report as soon as it has moved.
        self.capacity = None;
        self.usage = None;
        self.cost = None;
        self.scroll = Scroll::new();
        self.slash.dismiss();
        // The candidates were read from the directory of the session being
        // left; the next Tab reads the directory this session reported.
        self.path_cycle = None;
        self.transcript.clear();
        for message in &session.history {
            match message.role.as_str() {
                "user" => self
                    .transcript
                    .push_user_stored(message.content.clone(), message.message_id.clone()),
                // A stored assistant row carries the reasoning the daemon
                // re-rendered before the answer. Reading it back with the
                // daemon's own markup helper keeps the answer row identical to
                // the one the live stream produced, which is what a note on it
                // is keyed by.
                "assistant" => {
                    let (reasoning, split) = crate::transcript::Transcript::split_rendered_assistant(&message.content);
                    if let Some(reasoning) = reasoning {
                        self.transcript.reasoning_delta(&reasoning);
                        self.transcript.reasoning_done();
                    }
                    // A history row that names its answer separately is read
                    // from that field: a raw stored row can carry reasoning and
                    // answer as one unmarked string, and `content` keeps that
                    // historical value for clients that do not know the field.
                    let answer = message.answer.clone().unwrap_or(split);
                    if !answer.is_empty() {
                        self.transcript
                            .push_assistant_stored(answer, message.message_id.clone());
                    }
                }
                // Tool rows are the daemon's record of what ran, not the
                // conversation; the answers they produced are in the assistant
                // messages that follow them.
                _ => {}
            }
        }
        // The notes are read before the notice below, so anything the file
        // said about itself is drawn before the line naming the session. A
        // note belongs to a session id, so the ones drawn from here on are
        // this session's, never the one just left.
        self.load_notes();
        if session.history.is_empty() && !started {
            self.transcript.push_notice("the stored session has no messages");
        }
        self.transcript.push_notice(if started {
            format!("started a new session {session_id}")
        } else {
            format!("resumed {session_id}")
        });
    }

    /// Point the client at the directory the attached session works in.
    ///
    /// Called with the directory that travels beside the attach answer, after
    /// the session itself is in place. This is what keeps a draft's completed
    /// paths the session's: a switch to a session in another folder moves the
    /// root, and an answer that names no folder is not the old one either, so
    /// path completion stays off until a session names one.
    pub fn apply_session_directory(&mut self, directory: Option<&str>) {
        let Some(directory) = directory.map(str::trim).filter(|directory| !directory.is_empty()) else {
            self.workspace_known = false;
            return;
        };
        self.workspace_known = true;
        if self.cwd.as_deref() == Some(directory) {
            return;
        }
        self.cwd = Some(directory.to_string());
        self.reload_commands();
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
        // A refused fork never moved, so the session it was asked from must not
        // be treated as a branch's origin, and a later answer must not do it.
        self.fork_pending = None;
        // A request that came back as a failure will never be answered, so the
        // level or model it asked for must not steer the cycle keys.
        self.effort_requested = None;
        self.model_requested = None;
        self.model_cycle_pending = None;
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
fn model_preview(model: &str, provider: Option<&str>, current: bool, shortlist: &[String]) -> Vec<String> {
    let mut lines = vec![format!("model: {model}")];
    if let Some(provider) = provider {
        lines.push(format!("provider: {provider}"));
    }
    lines.push(if current {
        "the current model of this session".to_string()
    } else {
        "not the current model".to_string()
    });
    // Where the model sits in the list the cycle keys walk, so a reader can
    // tell whether the next press reaches it. An unconfigured shortlist says
    // nothing rather than repeating one line on every row.
    if !shortlist.is_empty() {
        lines.push(match shortlist.iter().position(|entry| entry.as_str() == model) {
            Some(index) => format!("models.shortlist entry {}", index + 1),
            // The one in force is the clearest case of this: the walk cannot
            // start from a model it does not hold, so the first press starts
            // at the near end instead.
            None if current => "not in models.shortlist; the next cycle starts at its near end".to_string(),
            None => "not in models.shortlist".to_string(),
        });
    }
    lines.push(format!("enter switches this session to {model}"));
    lines
}

/// The models of a list as one readable phrase: `a`, `a and b`, `a, b and c`.
fn join_names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
