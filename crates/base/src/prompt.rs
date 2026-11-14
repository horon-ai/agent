//! System prompt management

use std::path::Path;

mod git_context;
mod hardware;
mod static_sources;

use git_context::get_git_info;
use hardware::hardware_context;
#[cfg(test)]
pub(crate) use static_sources::load_agents_md_files_from_dirs;
pub use static_sources::{StaticPromptSources, load_agents_md_files_from_dir, load_base_system_prompt};
use static_sources::{
    base_system_prompt_parts, load_preferred_tools_files_from_dir, load_prompt_overlay_files_from_dir,
};

/// Default system prompt for horon-agent (embedded at compile time)
pub const DEFAULT_SYSTEM_PROMPT: &str = include_str!("prompt/system_prompt.md");

/// Tool-selection hint for sessions that can call `todo`.
///
/// Appended by the agent's prompt builder only when the session's tool
/// selection allows `todo`, so profiles without the tool (`core`) never see it.
pub const TODO_HINT: &str = "Track multi-step work with the todo tool; editors show it as the plan.";

/// Harness capabilities that conditionally contribute prompt modules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PromptCapabilities {
    pub foreign_rules: bool,
}

impl PromptCapabilities {
    fn current() -> Self {
        Self {
            foreign_rules: crate::config::config().features.foreign_rules,
        }
    }
}

/// Imported foreign agent rules (`.cursorrules`, Copilot instructions, ...),
/// rendered with their provenance labels. `None` when the feature is off or no
/// rule file was found.
fn foreign_rules_section(capabilities: PromptCapabilities, working_dir: Option<&Path>) -> Option<(String, usize)> {
    if !capabilities.foreign_rules {
        return None;
    }
    let root = working_dir.unwrap_or(Path::new("."));
    let mode = crate::config::config().features.foreign_rules_mode;
    let section = crate::foreign_rules::load_with_mode(root, mode).section?;
    let chars = section.len();
    Some((section, chars))
}

/// Built-in default swarm prompt: model-routing guidance for spawned swarm
/// agents (which model/effort to pick per task kind). Users can override it by
/// creating `~/.horon/swarm-prompt.md` (global) or `./.horon/swarm-prompt.md`
/// (project). See [`load_swarm_prompt`].
pub const DEFAULT_SWARM_PROMPT: &str = include_str!("prompt/swarm_prompt.md");

/// Load the swarm prompt used to steer swarm model routing. Precedence:
/// project `./.horon/swarm-prompt.md`, then global `~/.horon/swarm-prompt.md`,
/// then the built-in [`DEFAULT_SWARM_PROMPT`].
pub fn load_swarm_prompt(working_dir: Option<&Path>) -> String {
    let project_dir = working_dir.unwrap_or(Path::new("."));
    let candidates = [
        Some(project_dir.join(".horon").join("swarm-prompt.md")),
        crate::storage::horon_agent_dir()
            .ok()
            .map(|dir| dir.join("swarm-prompt.md")),
    ];
    for path in candidates.into_iter().flatten() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    DEFAULT_SWARM_PROMPT.trim().to_string()
}

/// Reasoning-effort sentinel that enables swarm orchestration. Providers
/// translate this to the configured root effort (maximum by default) when
/// building API requests, while the UI/session keep the literal `swarm` marker
/// so the agent knows to
/// inject [`SWARM_EFFORT_DIRECTIVE`].
pub const SWARM_EFFORT: &str = "swarm";

/// Reasoning-effort sentinel for the **deep task graph** mode: configured root
/// reasoning AND the comprehensive DAG-first swarm workflow (decompose into a
/// validated task graph, critique/verify gates, typed artifact handoffs). Sits
/// one rung above [`SWARM_EFFORT`] on the effort ladder: `... xhigh`, `swarm`
/// (light fan-out), `swarm-deep` (deep task graph). Providers translate this to
/// the configured root effort, while the UI/session keep the literal marker so
/// the agent knows to inject [`SWARM_DEEP_EFFORT_DIRECTIVE`].
pub const SWARM_DEEP_EFFORT: &str = "swarm-deep";

/// System-prompt directive injected when the active reasoning effort is
/// [`SWARM_EFFORT`]. Instructs the agent to lean on the swarm tooling.
pub const SWARM_EFFORT_DIRECTIVE: &str = "# Swarm Effort\n\nSwarm orchestration is enabled. Your root reasoning effort is configured independently from worker effort. For any non-trivial task, decompose the work and use the `swarm` tool to spawn and coordinate parallel agents (spawn workers with concrete prompts, assign tasks, and collect their reports) instead of doing everything yourself in one thread. Prefer parallelizing independent subtasks across swarm members, and use a coordinator/plan when the work has multiple stages. Only skip the swarm for trivial, single-step requests.";

/// System-prompt directive injected when the active reasoning effort is
/// [`SWARM_DEEP_EFFORT`]. Instructs the agent to run the comprehensive DAG-first
/// task-graph workflow.
pub const SWARM_DEEP_EFFORT_DIRECTIVE: &str = "# Deep Task Graph\n\nThe deep task-graph swarm workflow is enabled. Your root reasoning effort is configured independently from worker effort. Treat the task DAG as the primary object, not ad hoc agent chat. Workflow:\n\n1. Seed a graph with `swarm task_graph` using `mode: \"deep\"`: lay out nodes (kind explore|implement|verify|fix|synthesize) and `depends_on` edges instead of answering directly. (At this effort the server already defaults the plan to deep, but pass `mode: \"deep\"` explicitly anyway.) The engine auto-inserts a plan-wide root gate over your seed: the plan cannot finish until a final adversarial audit passes, and that audit can inject new top-level work.\n2. For any node that is too big, `swarm expand_node` to decompose it into a child sub-DAG (you become its planner/integrator). In deep mode a critique/verify gate is auto-inserted before a composite node can close. The graph is EXPECTED to outgrow its seed, often by several times: growth (expansions and gate-injected gaps) is the system working, not scope creep. plan_status reports seeded-vs-grown counts.\n3. Finish each node with `swarm complete_node` and a typed artifact: `findings`, `evidence` (file:line / commit refs), `validation`, `open_questions`, a required `confidence` (low|medium|high; report low honestly, it routes follow-up work to shore up that scope), and an honest `what_i_did_not_check`. Downstream nodes are hydrated with these artifacts automatically. There is no other way to close a deep node: a turn ending without expand_node/complete_node re-queues the node to a fresh worker and fails it on repeat.\n4. When a critique/verify gate finds gaps or failures, use `swarm inject_gap` to add new nodes; the parent cannot close until they drain. A passing gate artifact must account for EVERY node it audited by id (the server rejects rubber stamps), and cannot pass over a low-confidence sibling without addressing it explicitly, so treat low-confidence siblings as priority probe targets.\n5. Use `swarm run_plan` to drive the graph to completion. It returns immediately and drives the plan as a background task (progress card + wake on completion), so keep working or answer the user while it runs; check `swarm plan_status` or `bg` for progress. Deep mode fans out wide automatically (many workers run in parallel, bounded only by the swarm member cap), so prefer decomposing into MANY independent sibling nodes rather than a few serial ones: keep the ready set wide so run_plan can dispatch lots of agents at once. Only add `depends_on` edges for real data dependencies.\n\nComprehensiveness is structural: prefer decomposition + gates over a single thorough answer, so it is very unlikely any nook or cranny is missed.";

/// Returns true when `effort` is either swarm sentinel (light or deep),
/// case-insensitive. Providers resolve their configured root reasoning level.
pub fn is_swarm_effort(effort: &str) -> bool {
    let trimmed = effort.trim();
    trimmed.eq_ignore_ascii_case(SWARM_EFFORT) || trimmed.eq_ignore_ascii_case(SWARM_DEEP_EFFORT)
}

/// Returns true when `effort` is specifically the deep task-graph sentinel.
pub fn is_deep_swarm_effort(effort: &str) -> bool {
    effort.trim().eq_ignore_ascii_case(SWARM_DEEP_EFFORT)
}

/// Configured root reasoning level for an orchestration sentinel. Providers
/// translate this real level to their supported range while retaining the
/// sentinel in session state. Ordinary reasoning efforts are left untouched.
pub fn swarm_root_reasoning_effort(effort: &str) -> Option<&'static str> {
    if !is_swarm_effort(effort) {
        return None;
    }
    Some(
        crate::config::config()
            .agents
            .root_effort_for_swarm(is_deep_swarm_effort(effort)),
    )
}

/// The user-facing "general effort" ladder is one list, but each rung is one of
/// two internal kinds: a plain reasoning level (mapped straight to the provider
/// wire effort) or a swarm orchestration mode (with independently configured
/// root reasoning). [`EffortKind`] is the single classifier all consumers use so the
/// UI, providers, and scheduler never disagree about what a rung means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortKind {
    /// A plain reasoning level (none/low/medium/high/xhigh/max).
    Reasoning,
    /// Light swarm mode: configured root reasoning + parallel fan-out.
    SwarmLight,
    /// Deep swarm mode: configured root reasoning + DAG-first task graph.
    SwarmDeep,
}

impl EffortKind {
    /// True when this rung is a swarm orchestration mode rather than a plain
    /// reasoning level. Such rungs must not be treated as per-model effort
    /// variants (e.g. they should not generate `model (effort)` picker rows).
    pub fn is_swarm_mode(self) -> bool {
        matches!(self, EffortKind::SwarmLight | EffortKind::SwarmDeep)
    }
}

/// Classify a general-effort rung string into its [`EffortKind`].
pub fn classify_effort(effort: &str) -> EffortKind {
    let trimmed = effort.trim();
    if trimmed.eq_ignore_ascii_case(SWARM_DEEP_EFFORT) {
        EffortKind::SwarmDeep
    } else if trimmed.eq_ignore_ascii_case(SWARM_EFFORT) {
        EffortKind::SwarmLight
    } else {
        EffortKind::Reasoning
    }
}

/// True when an effort rung is a swarm orchestration mode (light or deep) rather
/// than a plain reasoning level. Convenience wrapper over [`classify_effort`].
pub fn is_swarm_mode_effort(effort: &str) -> bool {
    classify_effort(effort).is_swarm_mode()
}

/// Append the appropriate swarm directive to a split prompt's dynamic part when
/// the active reasoning effort is a swarm sentinel. The deep sentinel injects the
/// DAG-first task-graph directive; the light sentinel injects the fan-out
/// directive. No-op otherwise.
pub fn append_swarm_effort_directive(split: &mut SplitSystemPrompt, effort: Option<&str>) {
    let directive = match effort {
        Some(effort) if is_deep_swarm_effort(effort) => SWARM_DEEP_EFFORT_DIRECTIVE,
        Some(effort) if is_swarm_effort(effort) => SWARM_EFFORT_DIRECTIVE,
        _ => return,
    };
    if !split.dynamic_part.is_empty() {
        split.dynamic_part.push_str("\n\n");
    }
    split.dynamic_part.push_str(directive);
}
/// Split system prompt for efficient caching
/// Static content is cached, dynamic content is not
#[derive(Debug, Clone, Default)]
pub struct SplitSystemPrompt {
    /// Static content that should be cached (instruction files, base prompt, skills)
    pub static_part: String,
    /// Dynamic turn context that changes per request (memory, active skill, reminders)
    pub dynamic_part: String,
}

impl SplitSystemPrompt {
    pub fn chars(&self) -> usize {
        match (self.static_part.is_empty(), self.dynamic_part.is_empty()) {
            (true, true) => 0,
            (false, true) => self.static_part.len(),
            (true, false) => self.dynamic_part.len(),
            (false, false) => self.static_part.len() + 2 + self.dynamic_part.len(),
        }
    }

    pub fn estimated_tokens(&self) -> usize {
        crate::util::estimate_tokens(&if self.static_part.is_empty() {
            self.dynamic_part.clone()
        } else if self.dynamic_part.is_empty() {
            self.static_part.clone()
        } else {
            format!("{}\n\n{}", self.static_part, self.dynamic_part)
        })
    }
}

/// Skill info for system prompt
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
}

const SKILL_DESC_MAX_CHARS: usize = 120;

/// Byte budget for the rendered "Available Skills" system-prompt section.
///
/// The catalog grows with every installed skill, and the section sits in the
/// cacheable prompt prefix, so an unbounded list makes every request pay for
/// skills the model will never invoke. Past this budget the section drops the
/// one-line descriptions first, then truncates the name list, and states how
/// many skills it left out. Omitted skills stay reachable: `read skill://name`
/// and a `/skill:name` invocation both resolve against the registry, never
/// against this rendered text.
pub const SKILLS_SECTION_MAX_BYTES: usize = 48 * 1024;

const SKILLS_SECTION_HEADER: &str = "# Available Skills\n\nYou can read a skill with the read tool at `skill://<name>`, \
     or the user can invoke it with `/skill:<name>` or `/<name>`:\n";
const SKILLS_SECTION_FOOTER: &str =
    "\n\nWhen a user asks about available skills or capabilities, mention these skills.";

fn clip_skill_description(description: &str) -> String {
    let one_line = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= SKILL_DESC_MAX_CHARS {
        return one_line;
    }

    let mut clipped: String = one_line.chars().take(SKILL_DESC_MAX_CHARS.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

/// Render the "Available Skills" system-prompt section, bounded by
/// [`SKILLS_SECTION_MAX_BYTES`].
///
/// Returns `None` for an empty list. The list is sorted by name first, so the
/// output depends only on the skill set and never on insertion order.
pub fn build_available_skills_section(available_skills: &[SkillInfo]) -> Option<String> {
    if available_skills.is_empty() {
        return None;
    }

    // Name order is a prompt-cache invariant, not a caller convention: two
    // snapshots of the same skill set must serialize to the same bytes or the
    // cached prefix is silently invalidated. Sorting here also makes the
    // truncation below deterministic (the alphabetically last skills are the
    // ones left out).
    let mut available_skills: Vec<&SkillInfo> = available_skills.iter().collect();
    available_skills.sort_by(|a, b| a.name.cmp(&b.name));

    // A list that fits renders exactly as it always has: header, one
    // description line per skill, footer. Anything smaller than the budget must
    // stay byte-identical so the cached prompt prefix does not churn.
    let full = render_skills_section(&available_skills, true, 0, false);
    if full.len() <= SKILLS_SECTION_MAX_BYTES {
        return Some(full);
    }

    // Over budget: drop the descriptions first, since the names are what the
    // model needs to invoke anything at all.
    let name_only = render_skills_section(&available_skills, false, 0, true);
    if name_only.len() <= SKILLS_SECTION_MAX_BYTES {
        return Some(name_only);
    }

    // Still over: truncate the name list. Entries are whole skills, so a cut
    // never splits a multi-byte character; the omitted count goes in the
    // notice, which the budget below accounts for. The kept count is found by
    // binary search, so the loop is bounded by log2(skills).
    let total = available_skills.len();
    let render = |keep: usize| render_skills_section(&available_skills[..keep], false, total - keep, true);
    let mut fits = 0usize;
    let mut over = total;
    while over - fits > 1 {
        let middle = fits + (over - fits) / 2;
        if render(middle).len() <= SKILLS_SECTION_MAX_BYTES {
            fits = middle;
        } else {
            over = middle;
        }
    }
    // Only the header, notice, and footer remain at `fits == 0`; clip on a
    // character boundary so the budget holds even with an absurd notice.
    Some(clip_section_to_budget(render(fits)))
}

fn render_skills_section(
    available_skills: &[&SkillInfo],
    include_descriptions: bool,
    omitted: usize,
    descriptions_dropped: bool,
) -> String {
    let mut section = String::from(SKILLS_SECTION_HEADER);
    for skill in available_skills {
        section.push_str("\n- `/");
        section.push_str(&skill.name);
        section.push_str(" `");
        if include_descriptions {
            section.push_str(" - ");
            section.push_str(&clip_skill_description(&skill.description));
        }
    }
    section.push_str(&skills_section_notice(omitted, descriptions_dropped));
    section.push_str(SKILLS_SECTION_FOOTER);
    section
}

/// Notice text appended before the footer when the list had to be shortened.
/// Empty for a list that fits, which keeps that case byte-identical.
fn skills_section_notice(omitted: usize, descriptions_dropped: bool) -> String {
    let mut notice = String::new();
    if descriptions_dropped {
        notice.push_str(
            "\n\nDescriptions are omitted here to keep this list within its size budget; \
             read `skill://<name>` for a skill's instructions.",
        );
    }
    if omitted > 0 {
        notice.push_str(&format!(
            "\n{} more skills are installed but not listed here. They still work: \
             read one with `skill://<name>` or invoke it with `/skill:<name>`.",
            omitted
        ));
    }
    notice
}

/// Number of skills the rendered section left out, for tests that report the
/// budget outcome.
#[cfg(test)]
pub(crate) fn omitted_skill_count(available_skills: &[SkillInfo]) -> usize {
    let Some(section) = build_available_skills_section(available_skills) else {
        return 0;
    };
    // Every listed skill is one "- `/<name> ...`" line; the notices and the
    // footer never start that way.
    let listed = section.lines().filter(|line| line.starts_with("- `/")).count();
    available_skills.len().saturating_sub(listed)
}

/// Last-resort clip that never splits a UTF-8 character.
fn clip_section_to_budget(mut section: String) -> String {
    if section.len() <= SKILLS_SECTION_MAX_BYTES {
        return section;
    }
    let mut end = SKILLS_SECTION_MAX_BYTES;
    while end > 0 && !section.is_char_boundary(end) {
        end -= 1;
    }
    section.truncate(end);
    section
}

/// Longest per-tool description a one-line form prints.
///
/// The same line reaches the model through the system-prompt index and through
/// `toolbox list`, and both sit in a cached or repeatedly sent surface, so a
/// verbose description is clipped once, here, instead of in each caller. A
/// tool's first sentence says what it is for; the rest expands on it inside the
/// tool's own schema, which only a `describe` call pays for.
pub const ONE_LINE_DESCRIPTION_MAX_CHARS: usize = 60;

/// First non-empty line of `description`, cut at its first sentence and clipped
/// to [`ONE_LINE_DESCRIPTION_MAX_CHARS`].
///
/// A tool description is written for its own schema, where blank lines and
/// indentation are free. A list entry gets one line, so everything after the
/// first non-empty one is dropped, the line ends where its first sentence does,
/// and what is left is bounded.
pub fn one_line_description(description: &str) -> String {
    let line = description
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let line = first_sentence(line);
    if line.chars().count() > ONE_LINE_DESCRIPTION_MAX_CHARS {
        format!(
            "{}...",
            line.chars()
                .take(ONE_LINE_DESCRIPTION_MAX_CHARS - 3)
                .collect::<String>()
        )
    } else {
        line.to_string()
    }
}

/// `line` up to and including its first sentence terminator.
///
/// A terminator counts only when the line ends right after it or whitespace
/// follows, so a `.` inside a path or a version never cuts the sentence short.
/// An abbreviation followed by a space does, which costs one word of a clipped
/// description and never a wrong one.
fn first_sentence(line: &str) -> &str {
    let bytes = line.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if !matches!(byte, b'.' | b'!' | b'?') {
            continue;
        }
        match bytes.get(index + 1) {
            None => return &line[..=index],
            Some(next) if next.is_ascii_whitespace() => return &line[..=index],
            Some(_) => {}
        }
    }
    line
}

const ADDITIONAL_TOOLS_HEADER: &str = "# Additional tools";
const ADDITIONAL_TOOLS_INSTRUCTION: &str = "Call one with toolbox: action \"run\", tool \"<name>\", args {}.";

/// Render the `# Additional tools` system-prompt index of listed tools.
///
/// Returns `None` for an empty list, so a session that lists nothing adds no
/// bytes to its cached prefix. The caller supplies one row per listed tool as
/// `(name, one-line description)`; rows are sorted by name here, so the output
/// depends only on the tool set and never on registry insertion order. The
/// index is what a provider without native deferred loading has instead of the
/// deferred schemas it drops: the names stay reachable through `toolbox`.
pub fn build_additional_tools_section(rows: &[(String, String)]) -> Option<String> {
    if rows.is_empty() {
        return None;
    }

    let mut rows: Vec<&(String, String)> = rows.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));

    let mut section = String::from(ADDITIONAL_TOOLS_HEADER);
    section.push_str("\n\n");
    section.push_str(ADDITIONAL_TOOLS_INSTRUCTION);
    for (name, description) in rows {
        section.push_str("\n- ");
        section.push_str(name);
        section.push_str(": ");
        section.push_str(description);
    }
    Some(section)
}

/// Information about what's loaded in the context window
#[derive(Debug, Clone, Default)]
pub struct ContextInfo {
    // === Static (System Prompt) ===
    /// Base system prompt size (chars)
    pub system_prompt_chars: usize,
    /// Immutable session context size (chars), when persisted in transcript history.
    pub session_context_chars: usize,
    /// Whether project AGENTS.md was loaded
    pub has_project_agents_md: bool,
    /// Project AGENTS.md size (chars)
    pub project_agents_md_chars: usize,
    /// Imported foreign rule files size (chars)
    pub foreign_rules_chars: usize,
    /// Whether global ~/AGENTS.md was loaded
    pub has_global_agents_md: bool,
    /// Global AGENTS.md size (chars)
    pub global_agents_md_chars: usize,
    /// Skills section size (chars)
    pub skills_chars: usize,
    /// Memory section size (chars)
    pub memory_chars: usize,
    /// Prompt overlay section size (chars)
    pub prompt_overlay_chars: usize,
    /// Preferred tools section size (chars)
    pub preferred_tools_chars: usize,
    // === Dynamic (Conversation) ===
    /// Tool definitions sent to API (chars)
    pub tool_defs_chars: usize,
    /// Number of tool definitions
    pub tool_defs_count: usize,
    /// User messages total size (chars)
    pub user_messages_chars: usize,
    /// Number of user messages
    pub user_messages_count: usize,
    /// Assistant messages total size (chars)
    pub assistant_messages_chars: usize,
    /// Number of assistant messages
    pub assistant_messages_count: usize,
    /// Tool calls size (chars)
    pub tool_calls_chars: usize,
    /// Number of tool calls
    pub tool_calls_count: usize,
    /// Tool results size (chars)
    pub tool_results_chars: usize,
    /// Number of tool results
    pub tool_results_count: usize,

    /// Total system prompt size (chars)
    pub total_chars: usize,
}

impl ContextInfo {
    /// Rough estimate of tokens (chars / 4 is a common approximation)
    pub fn estimated_tokens(&self) -> usize {
        self.total_chars / 4
    }

    pub fn prompt_prefix_chars(&self) -> usize {
        self.system_prompt_chars
            + self.session_context_chars
            + self.project_agents_md_chars
            + self.foreign_rules_chars
            + self.global_agents_md_chars
            + self.skills_chars
            + self.memory_chars
            + self.prompt_overlay_chars
            + self.preferred_tools_chars
            + self.tool_defs_chars
    }

    pub fn prompt_prefix_tokens(&self) -> usize {
        self.prompt_prefix_chars() / 4
    }

    pub fn tool_definition_tokens(&self) -> usize {
        self.tool_defs_chars / 4
    }

    /// Get breakdown as (label, chars, icon) tuples for display
    pub fn breakdown(&self) -> Vec<(&'static str, usize, &'static str)> {
        let mut parts = vec![
            ("sys", self.system_prompt_chars, "⚙"),
            ("session", self.session_context_chars, "🌍"),
        ];
        if self.has_project_agents_md {
            parts.push(("agents", self.project_agents_md_chars, "📋"));
        }
        if self.foreign_rules_chars > 0 {
            parts.push(("foreign", self.foreign_rules_chars, "🌐"));
        }
        if self.has_global_agents_md {
            parts.push(("~agents", self.global_agents_md_chars, "📋"));
        }
        if self.skills_chars > 0 {
            parts.push(("skills", self.skills_chars, "🔧"));
        }
        if self.memory_chars > 0 {
            parts.push(("mem", self.memory_chars, "🧠"));
        }
        if self.prompt_overlay_chars > 0 {
            parts.push(("overlay", self.prompt_overlay_chars, "🧩"));
        }
        if self.preferred_tools_chars > 0 {
            parts.push(("tools", self.preferred_tools_chars, "🧰"));
        }
        parts
    }
}

/// Build the full system prompt with static context.
pub fn build_system_prompt(skill_prompt: Option<&str>, available_skills: &[SkillInfo]) -> String {
    let (prompt, _) = build_system_prompt_with_context(skill_prompt, available_skills);
    prompt
}

/// Build the full system prompt and return context info about what was loaded
pub fn build_system_prompt_with_context(
    skill_prompt: Option<&str>,
    available_skills: &[SkillInfo],
) -> (String, ContextInfo) {
    build_system_prompt_with_context_and_memory(skill_prompt, available_skills, None)
}

/// Build the full system prompt with optional memory section and return context info
pub fn build_system_prompt_with_context_and_memory(
    skill_prompt: Option<&str>,
    available_skills: &[SkillInfo],
    memory_prompt: Option<&str>,
) -> (String, ContextInfo) {
    build_system_prompt_full(skill_prompt, available_skills, memory_prompt, None)
}

/// Build the full system prompt with working directory support for loading context files
pub fn build_system_prompt_full(
    skill_prompt: Option<&str>,
    available_skills: &[SkillInfo],
    memory_prompt: Option<&str>,
    working_dir: Option<&Path>,
) -> (String, ContextInfo) {
    build_system_prompt_full_with_capabilities(
        skill_prompt,
        available_skills,
        memory_prompt,
        working_dir,
        PromptCapabilities::current(),
    )
}

pub fn build_system_prompt_full_with_capabilities(
    skill_prompt: Option<&str>,
    available_skills: &[SkillInfo],
    memory_prompt: Option<&str>,
    working_dir: Option<&Path>,
    capabilities: PromptCapabilities,
) -> (String, ContextInfo) {
    let mut parts = base_system_prompt_parts(working_dir);
    let mut info = ContextInfo {
        system_prompt_chars: parts.join("\n\n").len(),
        ..Default::default()
    };

    // Add AGENTS.md instructions with tracking (from working_dir or cwd)
    let (md_content, md_info) = load_agents_md_files_from_dir(working_dir);
    if let Some(content) = md_content {
        parts.push(content);
    }
    // Merge file info
    info.has_project_agents_md = md_info.has_project_agents_md;
    info.project_agents_md_chars = md_info.project_agents_md_chars;
    info.has_global_agents_md = md_info.has_global_agents_md;
    info.global_agents_md_chars = md_info.global_agents_md_chars;

    // Imported rules from other agent tools, after the native AGENTS.md path.
    if let Some((foreign_content, foreign_chars)) = foreign_rules_section(capabilities, working_dir) {
        info.foreign_rules_chars = foreign_chars;
        parts.push(foreign_content);
    }

    // Add optional prompt overlays from ~/.horon/ and ./.horon/
    let (overlay_content, overlay_chars) = load_prompt_overlay_files_from_dir(working_dir);
    if let Some(content) = overlay_content {
        info.prompt_overlay_chars = overlay_chars;
        parts.push(content);
    }

    // Add optional preferred-tool guidance from ~/.horon/ and ./.horon/
    let (preferred_tools_content, preferred_tools_chars) = load_preferred_tools_files_from_dir(working_dir);
    if let Some(content) = preferred_tools_content {
        info.preferred_tools_chars = preferred_tools_chars;
        parts.push(content);
    }

    if let Some(memory) = memory_prompt {
        info.memory_chars = memory.len();
        parts.push(memory.to_string());
    }

    // Add available skills list
    if let Some(skills_section) = build_available_skills_section(available_skills) {
        info.skills_chars = skills_section.len();
        parts.push(skills_section);
    }

    // Add active skill prompt
    if let Some(skill) = skill_prompt {
        parts.push(format!("# Active Skill\n\n{}", skill));
    }

    let prompt = parts.join("\n\n");
    info.total_chars = prompt.len();

    (prompt, info)
}

/// Build system prompt split into static (cacheable) and dynamic parts
/// This improves cache hit rate by keeping frequently-changing content separate
pub fn build_system_prompt_split(
    skill_prompt: Option<&str>,
    available_skills: &[SkillInfo],
    memory_prompt: Option<&str>,
    working_dir: Option<&Path>,
) -> (SplitSystemPrompt, ContextInfo) {
    build_system_prompt_split_with_capabilities(
        skill_prompt,
        available_skills,
        memory_prompt,
        working_dir,
        PromptCapabilities::current(),
    )
}

pub fn build_system_prompt_split_with_capabilities(
    skill_prompt: Option<&str>,
    available_skills: &[SkillInfo],
    memory_prompt: Option<&str>,
    working_dir: Option<&Path>,
    capabilities: PromptCapabilities,
) -> (SplitSystemPrompt, ContextInfo) {
    let agents_md = load_agents_md_files_from_dir(working_dir);
    let skills_section = build_available_skills_section(available_skills);
    build_system_prompt_split_with_capabilities_and_agents_md(
        skill_prompt,
        skills_section.as_deref(),
        memory_prompt,
        working_dir,
        capabilities,
        &agents_md,
        &StaticPromptSources::load(working_dir),
    )
}

/// Build a split prompt using an already captured AGENTS.md snapshot.
///
/// Only AGENTS.md comes from the caller: the base prompt, the overlay and the
/// preferred-tools guidance are read from disk on every call. A long-lived
/// agent uses [`build_system_prompt_split_with_snapshot`] instead, so that no
/// request of a running session goes to disk for them.
pub fn build_system_prompt_split_with_agents_md(
    skill_prompt: Option<&str>,
    available_skills: &[SkillInfo],
    memory_prompt: Option<&str>,
    working_dir: Option<&Path>,
    agents_md: (Option<String>, ContextInfo),
) -> (SplitSystemPrompt, ContextInfo) {
    let skills_section = build_available_skills_section(available_skills);
    build_system_prompt_split_with_capabilities_and_agents_md(
        skill_prompt,
        skills_section.as_deref(),
        memory_prompt,
        working_dir,
        PromptCapabilities::current(),
        &agents_md,
        &StaticPromptSources::load(working_dir),
    )
}

/// Build a split prompt from sources captured earlier: the AGENTS.md snapshot,
/// the base prompt, overlay and preferred-tools snapshot, and the skills
/// section already rendered by [`build_available_skills_section`].
///
/// This is the per-request path of a long-lived agent. It reads no prompt
/// source file and copies each captured section once, straight into the
/// static part, so the static part stays byte-identical until the agent
/// captures its sources again at a session boundary.
pub fn build_system_prompt_split_with_snapshot(
    skill_prompt: Option<&str>,
    skills_section: Option<&str>,
    memory_prompt: Option<&str>,
    working_dir: Option<&Path>,
    agents_md: &(Option<String>, ContextInfo),
    sources: &StaticPromptSources,
) -> (SplitSystemPrompt, ContextInfo) {
    build_system_prompt_split_with_capabilities_and_agents_md(
        skill_prompt,
        skills_section,
        memory_prompt,
        working_dir,
        PromptCapabilities::current(),
        agents_md,
        sources,
    )
}

/// Append one static section after the ones already written, with the same
/// `\n\n` separator a join of the parts would put between them.
fn push_static_section(static_part: &mut String, section: &str) {
    static_part.push_str("\n\n");
    static_part.push_str(section);
}

fn build_system_prompt_split_with_capabilities_and_agents_md(
    skill_prompt: Option<&str>,
    skills_section: Option<&str>,
    memory_prompt: Option<&str>,
    working_dir: Option<&Path>,
    capabilities: PromptCapabilities,
    agents_md: &(Option<String>, ContextInfo),
    sources: &StaticPromptSources,
) -> (SplitSystemPrompt, ContextInfo) {
    let (md_content, md_info) = agents_md;
    let captured_len = [
        md_content.as_deref(),
        sources.overlay.0.as_deref(),
        sources.preferred_tools.0.as_deref(),
        skills_section,
    ]
    .iter()
    .map(|section| section.map_or(0, |text| text.len() + 2))
    .sum::<usize>();
    let mut static_part = String::with_capacity(sources.base.len() + captured_len);
    static_part.push_str(&sources.base);
    let mut info = ContextInfo {
        system_prompt_chars: sources.base.len(),
        ..Default::default()
    };

    // === STATIC CONTENT (cacheable) ===

    // Add AGENTS.md instructions (static per project)
    if let Some(content) = md_content {
        push_static_section(&mut static_part, content);
    }
    info.has_project_agents_md = md_info.has_project_agents_md;
    info.project_agents_md_chars = md_info.project_agents_md_chars;
    info.has_global_agents_md = md_info.has_global_agents_md;
    info.global_agents_md_chars = md_info.global_agents_md_chars;

    // Imported rules from other agent tools, after the native AGENTS.md path.
    if let Some((foreign_content, foreign_chars)) = foreign_rules_section(capabilities, working_dir) {
        info.foreign_rules_chars = foreign_chars;
        push_static_section(&mut static_part, &foreign_content);
    }

    // Add optional prompt overlays from ~/.horon/ and ./.horon/
    if let Some(content) = &sources.overlay.0 {
        info.prompt_overlay_chars = sources.overlay.1;
        push_static_section(&mut static_part, content);
    }

    // Add optional preferred-tool guidance (static per project/user)
    if let Some(content) = &sources.preferred_tools.0 {
        info.preferred_tools_chars = sources.preferred_tools.1;
        push_static_section(&mut static_part, content);
    }

    // Add available skills list (fairly static)
    if let Some(section) = skills_section {
        info.skills_chars = section.len();
        push_static_section(&mut static_part, section);
    }

    // === TURN CONTEXT (not cached) ===

    let mut dynamic_part = String::new();

    // Memory prompt (changes per conversation)
    if let Some(memory) = memory_prompt {
        info.memory_chars = memory.len();
        dynamic_part.push_str(memory);
    }

    // Active skill prompt (changes per skill invocation)
    if let Some(skill) = skill_prompt {
        if memory_prompt.is_some() {
            dynamic_part.push_str("\n\n");
        }
        dynamic_part.push_str("# Active Skill\n\n");
        dynamic_part.push_str(skill);
    }

    info.total_chars = static_part.len() + dynamic_part.len();

    (
        SplitSystemPrompt {
            static_part,
            dynamic_part,
        },
        info,
    )
}

/// The one line that tells the model which extra directories this session may
/// work in.
///
/// The session context carries it once, and a change after that capture is
/// injected as a system reminder for exactly one prompt, so both surfaces read
/// the same way.
pub fn additional_directories_line(directories: &[String]) -> String {
    if directories.is_empty() {
        "Additional directories: (none)".to_string()
    } else {
        format!("Additional directories: {}", directories.join(", "))
    }
}

/// Build immutable session context captured once per session.
pub fn build_session_context(working_dir: Option<&Path>, additional_directories: &[String]) -> String {
    let mut lines = vec!["# Session Context".to_string()];

    lines.extend(session_datetime_lines());
    lines.push(format!("OS: {}", std::env::consts::OS));
    lines.push(format!("Architecture: {}", std::env::consts::ARCH));
    lines.push(format!(
        "HoronAgent version: {} ({})",
        horon_agent_build_meta::version(),
        horon_agent_build_meta::git_hash()
    ));

    if let Some(hardware) = hardware_context() {
        lines.push(hardware);
    }

    let cwd = working_dir.map(Path::to_path_buf);
    if let Some(cwd) = cwd.as_deref() {
        lines.push(format!("Working directory: {}", cwd.display()));
        if !additional_directories.is_empty() {
            lines.push(additional_directories_line(additional_directories));
        }
        if let Some(git_info) = get_git_info(Some(cwd)) {
            lines.push(git_info);
        }
    } else if !additional_directories.is_empty() {
        // An ACP session always binds a working directory, so this only keeps a
        // client-less session's context from silently dropping the list.
        lines.push(additional_directories_line(additional_directories));
    }

    lines.join("\n")
}

fn session_datetime_lines() -> [String; 3] {
    std::panic::catch_unwind(|| format_session_datetime(chrono::Local::now()))
        .unwrap_or_else(|_| format_session_datetime(chrono::Utc::now()))
}

fn format_session_datetime<Tz>(now: chrono::DateTime<Tz>) -> [String; 3]
where
    Tz: chrono::TimeZone,
    Tz::Offset: std::fmt::Display,
{
    [
        format!("Date: {}", now.format("%Y-%m-%d")),
        format!("Time: {}", now.format("%H:%M:%S")),
        format!("Timezone: {}", now.format("%Z")),
    ]
}

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod prompt_tests;

#[cfg(test)]
mod session_context_tests;
