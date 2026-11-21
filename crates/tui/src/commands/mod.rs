//! Slash commands: the built-in UI commands, the harness's runtime commands,
//! and the markdown command files a project or a user keeps.
//!
//! A typed line is resolved against three sources, in this order:
//!
//! 1. Built-in UI commands ([`Local`]) are owned by the client: `/quit`,
//!    `/exit`, `/help`, `/artifact`, `/archived`, `/copy`, `/keys`, `/clear`, `/clear-context`,
//!    `/compact`, `/jobs`, `/model`, `/effort`, `/mcp`, `/new`, `/note`, `/pin`,
//!    `/rename`, `/settings`, `/side`, `/resume`, `/skills`, `/switch`, `/unpin`,
//!    `/links`, `/queue`, `/record`, `/replay`, `/swarm` and `/voice`.
//! 2. Command files outrank the harness's own commands. `/review src` must run
//!    the project's `.horon/commands/review.md`, which is why a file wins over
//!    `RuntimeCommand::Review`: a project that writes a command file has said
//!    what that name means for that project.
//! 3. Every [`RuntimeCommand`] answers under its own `snake_case` name, so the
//!    list cannot drift from the protocol. The names are read out of serde
//!    rather than written down twice. The harness's `skills`, `mcp` and `swarm`
//!    commands are the exceptions: the built-in `/skills`, `/mcp` and `/swarm`
//!    own those names, and a skill itself answers as `/skill:<name>`.
//!
//! A file whose name a built-in UI command already owns is shadowed: `/help`
//! lists it with that label and it never runs, because the built-in wins.
//!
//! `//text` sends the literal message `/text`, which is the escape for prose
//! that starts with a slash. A line that starts with a slash and names nothing
//! becomes an error line instead of a message, so a typo never reaches the
//! model as prose.
//!
//! `!command` and `!!command` are the local shell. They are resolved before the
//! slash forms, because they begin with a different character, and they run in
//! the client's own working directory instead of the agent session: the
//! command text comes from the submitted line and from nowhere else, and
//! neither form sends anything to the harness. `!` keeps the output in the
//! transcript; `!!` keeps neither the command nor its output.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime};

use horon_agent_sdk::RuntimeCommand;

mod parse;

pub(crate) mod shell;

#[cfg(test)]
mod builtin_tests;

#[cfg(test)]
mod shell_tests;

#[cfg(test)]
mod skills_tests;

#[cfg(test)]
mod tests;

pub use parse::{ParsedCommand, fallback_description, substitute};

/// Changes to the model context of the current session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextCommand {
    Clear,
    Compact,
}

impl ContextCommand {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Clear => "clear-context",
            Self::Compact => "compact",
        }
    }
}

/// A command the client answers itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Local {
    /// Leave the interface.
    Quit,
    /// Leave the interface, under the name a shell user reaches for.
    Exit,
    /// List every command.
    Help,
    /// Open a retained artifact of this session in the editor.
    Artifact,
    /// List the keys in force and what each does.
    Keys,
    /// Clear the transcript pane.
    Clear,
    /// Clear or compact model context on the daemon.
    Context(ContextCommand),
    /// Copy the draft, an answer or a code block to the terminal clipboard.
    Copy,
    /// Open the model picker, or switch to the model named.
    Model,
    /// Set the reasoning effort, or pick one from the ladder.
    Effort,
    /// Start a new session in this working directory.
    New,
    /// Open the settings screen.
    Settings,
    /// Ask a read-only child of this conversation a side question.
    Side,
    /// Rename the attached session.
    Rename,
    /// Resume a session stored for this working directory.
    Resume,
    /// Browse archived sessions in this working directory.
    Archived,
    /// List the skills the harness reports, and put one in the draft.
    Skills,
    /// List trusted MCP servers or explicitly probe one by name.
    Mcp,
    /// Switch to another session, by id or from the list.
    Switch,
    /// Attach, edit, list or remove a local note on a transcript message.
    Note,
    /// Keep a session at the top of the session list, or the attached one.
    Pin,
    /// Take a session out of the pinned group.
    Unpin,
    /// Run `!command` in this client, keeping its output in the transcript.
    Shell,
    /// Run `!!command` in this client, keeping neither the command nor its output.
    ShellExcluded,
    /// List the http(s) links in the newest assistant answers and open one.
    Links,
    /// List the background work this session is running, and cancel one.
    Jobs,
    /// Record the visible conversation to a local file, or stop.
    Record,
    /// Replay a recorded conversation, or stop the replay.
    Replay,
    /// List the messages waiting for the running turn to end.
    Queue,
    /// List the swarm children of this session and the parent it reports back
    /// to, and attach to one.
    ///
    /// The bare name opens the list; an argument still reaches the harness's
    /// own `swarm` report, which is why the runtime command no longer answers
    /// under its own listing.
    Swarm,
    /// Record, recognize or speak with on-device programs; nothing leaves the
    /// client.
    Voice,
}

impl Local {
    /// The command a name means, if a built-in owns that name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "quit" => Some(Self::Quit),
            "exit" => Some(Self::Exit),
            "help" => Some(Self::Help),
            "artifact" => Some(Self::Artifact),
            "keys" => Some(Self::Keys),
            "clear" => Some(Self::Clear),
            "clear-context" => Some(Self::Context(ContextCommand::Clear)),
            "compact" => Some(Self::Context(ContextCommand::Compact)),
            "copy" => Some(Self::Copy),
            "model" => Some(Self::Model),
            "effort" => Some(Self::Effort),
            "new" => Some(Self::New),
            "settings" => Some(Self::Settings),
            // `/btw` is the name the workflow is known by elsewhere; both ask
            // the same question and open the same screen.
            "side" | "btw" => Some(Self::Side),
            "rename" => Some(Self::Rename),
            "resume" => Some(Self::Resume),
            "archived" => Some(Self::Archived),
            "skills" => Some(Self::Skills),
            "mcp" => Some(Self::Mcp),
            "switch" => Some(Self::Switch),
            "note" => Some(Self::Note),
            "pin" => Some(Self::Pin),
            "unpin" => Some(Self::Unpin),
            "links" => Some(Self::Links),
            "jobs" => Some(Self::Jobs),
            "record" => Some(Self::Record),
            "replay" => Some(Self::Replay),
            "queue" => Some(Self::Queue),
            "swarm" => Some(Self::Swarm),
            "voice" => Some(Self::Voice),
            _ => None,
        }
    }

    /// The name the user types after the slash.
    pub fn name(self) -> &'static str {
        match self {
            Self::Quit => "quit",
            Self::Exit => "exit",
            Self::Help => "help",
            Self::Artifact => "artifact",
            Self::Keys => "keys",
            Self::Clear => "clear",
            Self::Context(command) => command.name(),
            Self::Copy => "copy",
            Self::Model => "model",
            Self::Effort => "effort",
            Self::New => "new",
            Self::Settings => "settings",
            Self::Side => "side",
            Self::Rename => "rename",
            Self::Resume => "resume",
            Self::Archived => "archived",
            Self::Skills => "skills",
            Self::Mcp => "mcp",
            Self::Switch => "switch",
            Self::Note => "note",
            Self::Pin => "pin",
            Self::Unpin => "unpin",
            // The shell forms are typed with a `!` prefix, not a slash, so
            // these names are only what a listing would call them. They are
            // deliberately absent from `LOCAL_NAMES`.
            Self::Shell => "shell",
            Self::ShellExcluded => "shell",
            Self::Links => "links",
            Self::Jobs => "jobs",
            Self::Record => "record",
            Self::Replay => "replay",
            Self::Queue => "queue",
            Self::Swarm => "swarm",
            Self::Voice => "voice",
        }
    }
}

/// Where a command file was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `$HORON_HOME/commands`, shared by every project.
    User,
    /// `<cwd>/.horon/commands`, owned by the project.
    Project,
}

/// One markdown command file, after its frontmatter was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCommand {
    /// The file stem, which is the name typed after the slash.
    pub name: String,
    /// The frontmatter description, or the body's first line.
    pub description: String,
    /// The frontmatter `argument-hint`, drawn in `/help`.
    pub argument_hint: Option<String>,
    /// The body, before `$ARGUMENTS` substitution.
    pub body: String,
    /// Which directory the file came from.
    pub source: Source,
    /// A built-in UI command already owns this name, so the file cannot run.
    pub shadowed: bool,
}

/// Where a command in a listing comes from.
///
/// The listing prints one section per group, so a reader can tell a command the
/// client answers from one the harness runs without reading the description.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// A command the client answers itself ([`Local`]).
    Builtin,
    /// A runtime command the harness answers.
    Harness,
    /// A markdown command file.
    File,
    /// A skill the harness reported, reached as `/skill:<name>`.
    Skill,
}

/// One command as `/help` and the slash popup list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The name typed after the slash.
    pub name: String,
    /// The frontmatter argument hint, when the command takes arguments.
    pub argument_hint: Option<String>,
    /// One line saying what the command does; empty when nothing says.
    pub description: String,
    /// Where the command comes from.
    pub group: Group,
    /// A built-in owns the name, so the file never runs.
    pub shadowed: bool,
}

impl Entry {
    /// The name column: `/name` and its argument hint.
    pub fn label(&self) -> String {
        match &self.argument_hint {
            Some(hint) => format!("/{} {hint}", self.name),
            None => format!("/{}", self.name),
        }
    }
}

/// One skill the harness reported, as the command list needs it.
///
/// The list the picker draws keeps the rest of the report (the file it was read
/// from and the tools it may use); the resolver only needs what decides whether
/// a typed name is a skill and what the line expands to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// The name the harness knows it by.
    pub name: String,
    /// The description the harness gives it, on one line.
    pub description: String,
}

/// What one typed line asks the interface to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Invocation {
    /// Ordinary prose: send it as a user message.
    Message(String),
    /// A command file: show `display` as the user's line, send `text`.
    Expanded { display: String, text: String },
    /// A command the client answers itself.
    Local { command: Local, arguments: String },
    /// A command the harness answers.
    Server {
        command: RuntimeCommand,
        arguments: Vec<String>,
    },
    /// The line named nothing runnable; show it and send nothing.
    Error(String),
}

/// What Tab should do with the line being typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The names that match the typed prefix, for the hint under the input.
    pub matches: Vec<String>,
    /// The whole line to put in the input: the unique match, or the longest
    /// common prefix of the matches.
    pub insert: String,
}

/// How often [`Registry::tick`] is willing to look at the directories.
const TICK: Duration = Duration::from_secs(1);

/// The commands in force, and the files they came from.
#[derive(Debug, Clone)]
pub struct Registry {
    user: Option<PathBuf>,
    project: Option<PathBuf>,
    files: Vec<FileCommand>,
    /// The skills the harness reported for the session in force.
    ///
    /// They are the harness's list, not a directory this client read: a
    /// session change replaces them, and an empty list is a session whose
    /// harness reports none.
    skills: Vec<Skill>,
    stamps: [Option<Vec<FileStamp>>; 2],
    last_check: Instant,
    warnings: Vec<String>,
}

impl Registry {
    /// A registry reading `user_commands`, with no project directory yet.
    pub fn new(user_commands: PathBuf) -> Self {
        Self {
            user: Some(user_commands),
            project: None,
            files: Vec::new(),
            skills: Vec::new(),
            stamps: [None, None],
            last_check: Instant::now(),
            warnings: Vec::new(),
        }
    }

    /// Read every command file now.
    pub fn load(&mut self) {
        self.reload();
    }

    /// Point the registry at a project and read the files again.
    ///
    /// Returns whether anything was read, so the caller only reports warnings
    /// when the directories actually changed.
    pub fn sync(&mut self, cwd: Option<&Path>) -> bool {
        let project = cwd.map(|dir| dir.join(".horon").join("commands"));
        if project == self.project {
            return self.check_now();
        }
        self.project = project;
        self.reload();
        true
    }

    /// Read the files again when one of the directories changed.
    ///
    /// The check itself runs at most once a second, the same cadence the
    /// settings, theme and key files are re-read on; between checks a tick
    /// costs one clock read.
    pub fn tick(&mut self) -> bool {
        if self.last_check.elapsed() < TICK {
            return false;
        }
        self.check_now()
    }

    /// Check the directories immediately, ignoring the interval.
    ///
    /// This is what the tests and a `:reload` style command would use, so
    /// neither has to sleep. Only the file stamps are compared, so an idle
    /// check lists the two directories and reads none of their files.
    pub fn check_now(&mut self) -> bool {
        self.last_check = Instant::now();
        let stamps = [stamp(self.user.as_deref()), stamp(self.project.as_deref())];
        if stamps == self.stamps {
            return false;
        }
        self.reload();
        true
    }

    /// The command files in force, project first on a name collision.
    pub fn files(&self) -> &[FileCommand] {
        &self.files
    }

    /// Replace the skills the harness reported for the session in force.
    ///
    /// The whole list is replaced rather than merged: it belongs to one
    /// session, and a name the session that was left behind offered must stop
    /// resolving the moment the client moves.
    pub fn set_skills(&mut self, skills: Vec<Skill>) {
        self.skills = skills;
    }

    /// Every command in force, as `/help` and the slash popup list it.
    ///
    /// The order is the order the sections print in: built-ins first, then the
    /// harness's own names, then the command files, then the skills the harness
    /// reported.
    pub fn entries(&self) -> Vec<Entry> {
        let mut entries: Vec<Entry> = LOCAL_NAMES
            .iter()
            .map(|name| Entry {
                name: (*name).to_string(),
                argument_hint: None,
                description: crate::help::local_description(name).to_string(),
                group: Group::Builtin,
                shadowed: false,
            })
            .collect();
        let mut names: Vec<&str> = server_commands().iter().map(|(name, _)| name.as_str()).collect();
        names.sort_unstable();
        entries.extend(names.into_iter().map(|name| Entry {
            name: name.to_string(),
            argument_hint: None,
            description: crate::help::harness_description(name).to_string(),
            group: Group::Harness,
            shadowed: false,
        }));
        entries.extend(self.files.iter().map(|file| Entry {
            name: file.name.clone(),
            argument_hint: file.argument_hint.clone(),
            description: file.description.clone(),
            group: Group::File,
            shadowed: file.shadowed,
        }));
        entries.extend(self.skills.iter().map(|skill| Entry {
            name: format!("skill:{}", skill.name),
            argument_hint: None,
            description: skill.description.clone(),
            group: Group::Skill,
            shadowed: false,
        }));
        entries
    }

    /// Take the problems the last read produced.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    /// The skill the harness listed under this name, if it listed one.
    fn skill(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|skill| skill.name == name)
    }

    /// The listed skill a `skill:` address names, with the request after it.
    ///
    /// The longest listed name the remainder starts with wins, and it must end
    /// at a word boundary: a skill may be called `demo review`, so ending at
    /// the first word would look up `demo` for `/skill:demo review`. This is
    /// the rule the harness applies to `skill://<name>`, so both ends read one
    /// address the same way.
    fn addressed_skill<'a>(&'a self, rest: &'a str) -> Option<(&'a Skill, &'a str)> {
        self.skills
            .iter()
            .filter(|skill| !skill.name.is_empty())
            .filter_map(|skill| {
                let tail = rest.strip_prefix(skill.name.as_str())?;
                (tail.is_empty() || tail.starts_with(char::is_whitespace)).then_some((skill, tail.trim()))
            })
            .max_by_key(|(skill, _)| skill.name.len())
    }

    /// Resolve one typed line.
    ///
    /// The local shell is read first: `!!name` is excluded from the transcript,
    /// `!name` keeps its output, and either with no command is an error rather
    /// than prose. Only the submitted line is read here; the command text is
    /// never taken from a transcript item, a tool result or a model message.
    pub fn resolve(&self, text: &str) -> Invocation {
        let trimmed = text.trim();
        if let Some(rest) = trimmed.strip_prefix("!!") {
            return shell_invocation(rest, Local::ShellExcluded);
        }
        if let Some(rest) = trimmed.strip_prefix('!') {
            return shell_invocation(rest, Local::Shell);
        }
        let Some(rest) = trimmed.strip_prefix('/') else {
            return Invocation::Message(text.to_string());
        };
        if let Some(literal) = rest.strip_prefix('/') {
            return Invocation::Message(format!("/{literal}"));
        }
        let (word, arguments) = match rest.split_once(char::is_whitespace) {
            Some((word, arguments)) => (word, arguments.trim()),
            None => (rest, ""),
        };
        if word.is_empty() {
            return Invocation::Message(text.to_string());
        }
        if let Some(command) = Local::from_name(word) {
            return Invocation::Local {
                command,
                arguments: arguments.to_string(),
            };
        }
        // A skill keeps its own address, `/skill:<name>`, which no other source
        // spells: a name this line does not find is an error rather than a
        // message, because the line reads as a command and sending it as prose
        // would ask the model to run something the client never listed. The
        // address is the whole remainder after `skill:` and not its first
        // word, because a listed name may contain a space.
        if let Some(after) = rest.strip_prefix("skill:") {
            if let Some((skill, arguments)) = self.addressed_skill(after) {
                return expand_skill(skill, trimmed, arguments);
            }
            // No listed name matches. The error names the first word after
            // `skill:`, and an address with no name at all falls through to
            // the other sources as it did before.
            if let Some(name) = after.split_whitespace().next() {
                return Invocation::Error(unknown_skill(name));
            }
        }
        if let Some(file) = self.files.iter().find(|file| file.name == word && !file.shadowed) {
            let expanded = substitute(&file.body, arguments);
            if expanded.trim().is_empty() {
                return Invocation::Error(format!("`/{word}` has an empty body"));
            }
            return Invocation::Expanded {
                display: trimmed.to_string(),
                text: expanded,
            };
        }
        if let Some((_, command)) = server_commands().iter().find(|(name, _)| name == word) {
            return Invocation::Server {
                command: *command,
                arguments: arguments.split_whitespace().map(str::to_string).collect(),
            };
        }
        // A skill also answers to its bare name, but only where no built-in,
        // harness command or file already owns that name: those are the
        // addresses the rest of the client was written against, and a skill
        // could otherwise take one over just by being installed.
        if plain_skill_name(word)
            && let Some(skill) = self.skill(word)
        {
            return expand_skill(skill, trimmed, arguments);
        }
        Invocation::Error(crate::help::unknown(word))
    }

    /// The names matching the line being typed, and what Tab should insert.
    ///
    /// Only a bare prefix completes: once the line has a space the argument
    /// position is the user's to fill.
    pub fn complete(&self, text: &str) -> Option<Completion> {
        let rest = text.trim_start().strip_prefix('/')?;
        if rest.contains(char::is_whitespace) {
            return None;
        }
        let mut names: Vec<&str> = LOCAL_NAMES.to_vec();
        names.extend(server_commands().iter().map(|(name, _)| name.as_str()));
        names.extend(
            self.files
                .iter()
                .filter(|file| !file.shadowed)
                .map(|file| file.name.as_str()),
        );
        names.sort_unstable();
        names.dedup();
        let matches: Vec<String> = names
            .iter()
            .filter(|name| name.starts_with(rest))
            .map(|name| (*name).to_string())
            .collect();
        if matches.is_empty() {
            return None;
        }
        let insert = match matches.len() {
            1 => matches[0].clone(),
            _ => common_prefix(&matches),
        };
        Some(Completion {
            matches,
            insert: format!("/{insert}"),
        })
    }

    /// The `/help` text: every command with its summary, then the keys.
    ///
    /// The keys come from the default map, which is what a caller with no app
    /// around it means; the client passes the map it loaded.
    pub fn help(&self) -> String {
        self.help_with(crate::keys::default_bindings())
    }

    /// The `/help` text against the key map in force.
    pub fn help_with(&self, bindings: &crate::keys::Bindings) -> String {
        crate::help::text(&self.entries(), bindings)
    }

    /// Re-read both directories and rebuild the file list.
    fn reload(&mut self) {
        self.stamps = [stamp(self.user.as_deref()), stamp(self.project.as_deref())];
        self.warnings = server_catalog().warnings.clone();
        let user = self.user.clone();
        let project = self.project.clone();
        let mut files: Vec<FileCommand> = Vec::new();
        for (source, directory) in [(Source::User, user), (Source::Project, project)] {
            let Some(directory) = directory else {
                continue;
            };
            for file in self.read_directory(&directory, source) {
                files.retain(|existing| existing.name != file.name);
                files.push(file);
            }
        }
        for file in &mut files {
            file.shadowed = Local::from_name(&file.name).is_some();
        }
        files.sort_by(|left, right| left.name.cmp(&right.name));
        self.files = files;
    }

    /// Every readable markdown file in one command directory.
    fn read_directory(&mut self, directory: &Path, source: Source) -> Vec<FileCommand> {
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                self.warnings
                    .push(format!("cannot read {}: {error}", directory.display()));
                return Vec::new();
            }
        };
        let mut paths: Vec<PathBuf> = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => paths.push(entry.path()),
                Err(error) => self
                    .warnings
                    .push(format!("cannot read an entry of {}: {error}", directory.display())),
            }
        }
        paths.sort();
        let mut commands = Vec::new();
        for path in paths {
            if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
                continue;
            }
            let name = match path.file_stem().and_then(|stem| stem.to_str()) {
                Some(name) if !name.is_empty() && !name.contains(char::is_whitespace) => name.to_string(),
                _ => {
                    self.warnings
                        .push(format!("ignoring {}: the name is not a command", path.display()));
                    continue;
                }
            };
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) => {
                    self.warnings.push(format!("cannot read {}: {error}", path.display()));
                    continue;
                }
            };
            let parsed = parse::parse(&text);
            commands.push(FileCommand {
                name,
                description: match parsed.description {
                    Some(description) => description,
                    None => fallback_description(&parsed.body),
                },
                argument_hint: parsed.argument_hint,
                body: parsed.body,
                source,
                shadowed: false,
            });
        }
        commands
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new(horon_agent_sdk::user_horon_agent_home().join("commands"))
    }
}

/// The built-in names, for completion and `/help`.
const LOCAL_NAMES: [&str; 30] = [
    "archived",
    "artifact",
    "clear",
    "clear-context",
    "compact",
    "copy",
    "effort",
    "exit",
    "help",
    "jobs",
    "keys",
    "links",
    "mcp",
    "model",
    "new",
    "note",
    "pin",
    "queue",
    "quit",
    "record",
    "rename",
    "replay",
    "resume",
    "settings",
    "side",
    "skills",
    "swarm",
    "switch",
    "unpin",
    "voice",
];

/// Every runtime command, in declaration order.
const SERVER_VARIANTS: &[RuntimeCommand] = &[
    RuntimeCommand::Skills,
    RuntimeCommand::Context,
    RuntimeCommand::Info,
    RuntimeCommand::Memory,
    RuntimeCommand::Swarm,
    RuntimeCommand::Agents,
    RuntimeCommand::SubagentModel,
    RuntimeCommand::Autoreview,
    RuntimeCommand::Autojudge,
    RuntimeCommand::Transport,
    RuntimeCommand::Fast,
    RuntimeCommand::Overnight,
    RuntimeCommand::Productivity,
    RuntimeCommand::Initiatives,
    RuntimeCommand::RefreshModelList,
    RuntimeCommand::Continue,
    RuntimeCommand::ResumeAll,
    RuntimeCommand::Remote,
    RuntimeCommand::ServerReload,
    RuntimeCommand::Review,
    RuntimeCommand::Judge,
    RuntimeCommand::Subagent,
    RuntimeCommand::Auth,
    RuntimeCommand::Logout,
    RuntimeCommand::Plan,
    RuntimeCommand::Mcp,
];

/// Every runtime command under the name the protocol serialises it to.
struct ServerCatalog {
    names: Vec<(String, RuntimeCommand)>,
    warnings: Vec<String>,
}

/// The catalog, read out of serde once.
///
/// Reading the names instead of writing them down means a renamed variant
/// renames its command too, and there is no second list to keep in step.
fn server_catalog() -> &'static ServerCatalog {
    static CATALOG: LazyLock<ServerCatalog> = LazyLock::new(|| {
        let mut catalog = ServerCatalog {
            names: Vec::with_capacity(SERVER_VARIANTS.len()),
            warnings: Vec::new(),
        };
        for command in SERVER_VARIANTS {
            // The built-in `/skills`, `/mcp` and `/swarm` own those names, so
            // the harness's own commands are not offered beside them: a listing
            // with the same name twice would name one command and run another.
            // Each built-in still sends its own typed request, and `/swarm
            // status` forwards what is typed after it.
            if matches!(
                command,
                RuntimeCommand::Skills | RuntimeCommand::Mcp | RuntimeCommand::Swarm
            ) {
                continue;
            }
            match serde_json::to_value(command) {
                Ok(serde_json::Value::String(name)) => catalog.names.push((name, *command)),
                Ok(other) => catalog
                    .warnings
                    .push(format!("cannot name {command:?}: it serialises to {other}")),
                Err(error) => catalog.warnings.push(format!("cannot name {command:?}: {error}")),
            }
        }
        catalog
    });
    &CATALOG
}

/// The runtime commands, under the names the protocol gives them.
fn server_commands() -> &'static [(String, RuntimeCommand)] {
    &server_catalog().names
}

/// What a listed skill expands to, with the request typed after it.
///
/// The first clause is the same with and without a request, so a stored line
/// reads one way whether or not the user added words to it. `display` is the
/// line the user typed: the transcript shows the invocation, not the sentence
/// the harness receives.
fn expand_skill(skill: &Skill, display: &str, arguments: &str) -> Invocation {
    let text = if arguments.is_empty() {
        format!("Read skill://{} and follow its instructions.", skill.name)
    } else {
        format!(
            "Read skill://{} and follow its instructions for this request: {arguments}",
            skill.name
        )
    };
    Invocation::Expanded {
        display: display.to_string(),
        text,
    }
}

/// The line shown for `/skill:<name>` when the harness listed no such skill.
fn unknown_skill(name: &str) -> String {
    format!("unknown skill `{name}`; /skills lists them")
}

/// The invocation a `!` line asks for, or the error an empty one earns.
fn shell_invocation(command: &str, form: Local) -> Invocation {
    let command = command.trim();
    if command.is_empty() {
        return Invocation::Error(shell_usage());
    }
    Invocation::Local {
        command: form,
        arguments: command.to_string(),
    }
}

/// The line an empty `!` or `!!` leaves instead of running anything.
pub(crate) fn shell_usage() -> String {
    "a local shell command needs text: `!command` runs it, `!!command` runs it without keeping the command or its output".to_string()
}

/// Whether a name can be a skill's bare address.
///
/// The same shape the harness reads as an identifier: a skill called `demo
/// review` or `dir/notes` is listed and reachable as `/skill:<name>`, but as a
/// bare word it would need quoting the resolver does not offer.
fn plain_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

/// The longest prefix every name shares.
fn common_prefix(names: &[String]) -> String {
    let Some(first) = names.first() else {
        return String::new();
    };
    let mut length = first.chars().count();
    for name in names.iter().skip(1) {
        let shared = first
            .chars()
            .zip(name.chars())
            .take_while(|(left, right)| left == right)
            .count();
        length = length.min(shared);
    }
    first.chars().take(length).collect()
}

/// One command file as last seen: its path, length and modification time.
type FileStamp = (PathBuf, u64, SystemTime);

/// The markdown files a command directory holds, if it can be listed.
///
/// A new, removed, renamed or rewritten file changes the list. The directory's
/// own modification time is not enough: it stays put when a file is edited in
/// place, and a file created in the same clock tick as the last check shares
/// it. A filesystem that cannot report a modification time reports the epoch,
/// as the key and theme stamps do.
fn stamp(directory: Option<&Path>) -> Option<Vec<FileStamp>> {
    let Ok(entries) = std::fs::read_dir(directory?) else {
        return None;
    };
    let mut files = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        files.push((path, metadata.len(), modified));
    }
    files.sort();
    Some(files)
}
