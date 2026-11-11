//! Slash commands: the built-in UI commands, the harness's runtime commands,
//! and the markdown command files a project or a user keeps.
//!
//! A typed line is resolved against three sources, in this order:
//!
//! 1. Built-in UI commands ([`Local`]) never leave the client: `/quit`,
//!    `/help`, `/clear` and `/model`.
//! 2. Command files outrank the harness's own commands. `/review src` must run
//!    the project's `.horon/commands/review.md`, which is why a file wins over
//!    `RuntimeCommand::Review`: a project that writes a command file has said
//!    what that name means for that project.
//! 3. Every [`RuntimeCommand`] answers under its own `snake_case` name, so the
//!    list cannot drift from the protocol. The names are read out of serde
//!    rather than written down twice.
//!
//! A file whose name a built-in UI command already owns is shadowed: `/help`
//! lists it with that label and it never runs, because the built-in wins.
//!
//! `//text` sends the literal message `/text`, which is the escape for prose
//! that starts with a slash. A line that starts with a slash and names nothing
//! becomes an error line instead of a message, so a typo never reaches the
//! model as prose.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime};

use horon_agent_sdk::RuntimeCommand;

mod parse;

#[cfg(test)]
mod tests;

pub use parse::{ParsedCommand, fallback_description, substitute};

/// A command the client answers itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Local {
    /// Leave the interface.
    Quit,
    /// List every command.
    Help,
    /// Clear the transcript pane.
    Clear,
    /// List the models, or switch to the one named.
    Model,
}

impl Local {
    /// The command a name means, if a built-in owns that name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "quit" => Some(Self::Quit),
            "help" => Some(Self::Help),
            "clear" => Some(Self::Clear),
            "model" => Some(Self::Model),
            _ => None,
        }
    }

    /// The name the user types after the slash.
    pub fn name(self) -> &'static str {
        match self {
            Self::Quit => "quit",
            Self::Help => "help",
            Self::Clear => "clear",
            Self::Model => "model",
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
    stamps: [Option<SystemTime>; 2],
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
    /// neither has to sleep. Only the directory stamps are compared, so an
    /// idle check costs two `stat` calls and no directory listing.
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

    /// Take the problems the last read produced.
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    /// Resolve one typed line.
    pub fn resolve(&self, text: &str) -> Invocation {
        let trimmed = text.trim();
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
        Invocation::Error(format!("unknown command `/{word}`; /help lists the commands"))
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

    /// The `/help` text: every command, where it comes from, and its summary.
    pub fn help(&self) -> String {
        let mut lines = vec![
            "built-in commands:".to_string(),
            "  /clear   clear the transcript".to_string(),
            "  /help    list commands".to_string(),
            "  /model   list models, or switch with /model <name>".to_string(),
            "  /quit    leave".to_string(),
            "harness commands:".to_string(),
        ];
        let mut names: Vec<&str> = server_commands().iter().map(|(name, _)| name.as_str()).collect();
        names.sort_unstable();
        for chunk in names.chunks(6) {
            lines.push(format!("  /{}", chunk.join(" /")));
        }
        if self.files.is_empty() {
            lines.push(
                "no command files; add .horon/commands/<name>.md to a project or \
                 $HORON_HOME/commands/<name>.md for every project"
                    .to_string(),
            );
            return lines.join("\n");
        }
        lines.push("command files:".to_string());
        for file in &self.files {
            let mut line = format!("  /{}", file.name);
            if let Some(hint) = &file.argument_hint {
                line.push(' ');
                line.push_str(hint);
            }
            if !file.description.is_empty() {
                line.push_str("  ");
                line.push_str(&file.description);
            }
            if file.shadowed {
                line.push_str("  (shadowed by the built-in)");
            }
            lines.push(line);
        }
        lines.join("\n")
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
const LOCAL_NAMES: [&str; 4] = ["clear", "help", "model", "quit"];

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

/// When a command directory last changed, if it exists.
fn stamp(directory: Option<&Path>) -> Option<SystemTime> {
    let Ok(modified) = std::fs::metadata(directory?).and_then(|metadata| metadata.modified()) else {
        return None;
    };
    Some(modified)
}
