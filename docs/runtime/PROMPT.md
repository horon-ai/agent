# Prompts and tokens

Language: **English** | [한국어](PROMPT.ko.md)

This covers how the system prompt is assembled and how tokens are estimated.

## System prompt configuration

horon-agent builds the system prompt in several layers. Two of them are files the user edits, so agent behaviour can be tuned without a rebuild.

### Layers (in order)

1. **Base system prompt**: the built-in `crates/base/src/prompt/system_prompt.md`, replaceable by a
   file (see below).
2. Feature modules (for example the `[[stream_rules]]` guidance below).
3. `AGENTS.md`: the project's `./AGENTS.md` and the global `~/AGENTS.md`. An `AGENTS.md` in a subdirectory that this layer does not cover is attached separately to `read` results when `[tools] nested_agents_md` is on ([`docs/runtime/TOOLS.md`](TOOLS.md)).
4. Prompt overlay: `./.horon/prompt-overlay.md` and `~/.horon/prompt-overlay.md`.
5. Preferred tools: `./.horon/preferred-tools.md` and `~/.horon/preferred-tools.md`.
6. Memory and the active skill prompt (dynamic and uncached). Where the skill list comes from is in the "Skills" section below.

### Adding guidance (the most common task)

Append instructions without touching the base prompt.

- `~/.horon/prompt-overlay.md`: applies everywhere.
- `./.horon/prompt-overlay.md`: applies to one project.

Both are included when both exist. If in layers 4 to 6 the project path and the global path resolve to the same canonical path (for example when working in `$HOME` or through a symlink alias), the file is included once under the project heading. Different files are both included even when their contents match. The global `.horon` directory follows `HORON_HOME` when it is set. Layer 4 includes only the working directory and the global file; an `AGENTS.md` in a directory below is attached to `read` results rather than to the prompt (`[tools] nested_agents_md`, on by default, overridden by `HORON_AGENT_NESTED_AGENTS_MD`).

### Importing context files (`@path`)

The project's `./AGENTS.md` and the global `~/AGENTS.md` can pull in other files. When a whole line
is nothing but `@` followed by a path, that line is replaced by the referenced file's contents. Surrounding whitespace is ignored, and a line with any other character (such as `@` inside an email address) is not imported.

```markdown
# Project rules

@docs/rules/git.md
@docs/rules/rust.md
```

- A relative path resolves against the directory of the file holding the line.
- A path starting with `~/` is relative to the user's home. In a run with `HORON_HOME` set, it is rooted under `$HORON_HOME/external/` by the same rule as the global files.
- Imports chain up to 5 hops. The original file is hop 0, so an import line inside a hop-5 file would be the sixth and stays literal.
- Referencing a file already on the current import chain inserts no content and becomes the line `<!-- @import skipped (cycle): @path -->`. Cycles end there.
- A missing or unreadable file keeps the original line and logs one warning.
- A path that canonicalizes outside the project directory and the user's home is rejected and the original line stays. Paths that climb out, such as `@../../etc/passwd`, fall here.
- Lines inside a code fence (` ``` `, `~~~`) are treated as examples and are not imported.

The expanded result enters the prompt size as is. Imported content counts toward `ContextInfo` at its expanded length, not the referencing file's size, so token estimation follows what is actually injected. A file with no `@` line takes the existing path and is not copied.

### Replacing the base prompt

To replace layer 1 entirely, create one of these files.

- `./.horon/system-prompt.md` (project, highest priority)
- `~/.horon/system-prompt.md` (global)

The first non-empty file wins. Without one, the built-in default is used. An empty or whitespace-only file falls back to the default, so an empty prompt cannot be shipped by accident.

This replaces only the base prompt. AGENTS.md, overlays, skills, and memory still apply.

### Enforcing rules mid-stream ([[stream_rules]])

When the operator declares forbidden phrasing in `[[stream_rules]]`, assistant text is compared against those patterns while a response streams. On a match the response is discarded, the rule body goes in as a system reminder, and the request is issued again from the same conversation state. The model sees the rule it broke and writes the text again.

With `[features] stream_rules` off, the matcher is never created. The default is off (`crates/base/src/config/default_file.rs`).

```toml
[features]
stream_rules = true

[[stream_rules]]
id = "no-apology-opener"
pattern = "(?i)^(I'm sorry|Sorry,)"
body = "사과로 시작하지 말고 바로 결론부터 쓰세요."
```

A rule has three fields, `id`, `pattern` (Rust `regex`), and `body` (`crates/config-types/src/lib.rs`). The behaviour has four deliberate restrictions (`crates/app-core/src/agent/stream_rules.rs`).

- A rule that already matched is not retried in that turn. That keeps a model repeating the same phrasing from spending the whole injection budget.
- Only assistant text is checked. Tool arguments arrive as separate stream events and are not passed to rules.
- The scan window is the last 4 KiB (`LOOKBACK_BYTES`). A phrase spanning two chunks is caught, and rescanning the whole response per token on long output is avoided.
- At most three injections per turn (`MAX_INJECTIONS`). When a different rule matches a fourth time, the turn stops instead of continuing.

Rules narrow scope and frequency with the following optional fields. Omitting all of them keeps the original behaviour (every turn, text, retry after interruption).

| Field | Value | Meaning |
|---|---|---|
| `scope` | `text` (default), `tool`, `tool:<name>` | What is checked. Empty means `text` |
| `paths` | glob list | Applies to a tool call's `file_path`/`path` argument. Empty means every path |
| `repeat` | `turn` (default), `once`, `after-gap` | Every turn, once per session, once every `repeat_gap` turns |
| `repeat_gap` | integer (default 10) | Minimum turn gap for `after-gap` |
| `delivery` | `interrupt` (default), `passive` | Block the work and make it redo, or just append a reminder |

`tool` scope serializes the tool call arguments as compact JSON and matches the pattern against it. With `interrupt` the call is not executed and `Blocked by rule "{id}": {body}` comes back as a tool error result. With `passive` the call runs and `<system-reminder>Rule "{id}": {body}</system-reminder>` is appended to the result. A text-scope `passive` rule does not cut the response; it is merged into the next request's system reminder.

A turn in which `once` or `after-gap` actually fired is recorded per rule id in the session side state `stream_rule_ledger`, so reopening the session does not fire the same rule again.

The injection point marker is `S` (`STREAM_RULE_POINT`). It is distinct from the soft interrupt points A to D, so a client can tell which one it is without inspecting the content.

### Other agents' rule files (rulebook mode)

`[features] foreign_rules = true` reads `.cursor/rules/*.mdc`, `.cursorrules`, `.clinerules`,
Copilot instructions, Windsurf rules, and `GEMINI.md`. The default `foreign_rules_mode = "inline"`
puts every body into the system prompt.

`foreign_rules_mode = "rulebook"` inlines the body only for `alwaysApply` rules and shows the rest
as a `# Rulebook` list with just paths, descriptions, and applicable globs. The bodies of the other
rules are attached once as a `<project-rule path="…">` block when `read`, `edit`, `write`,
`hashline_edit`, or `apply_patch` first touches a file matching their glob. Rule files need not be
read in advance, and unrelated rules take no context.

### Notes

- Changes to these files apply to **new sessions**. A running session keeps the prompt it captured
  at start.
- Editing the built-in `system_prompt.md` takes a rebuild, because it is embedded with
  `include_str!`.
- Swarm model routing guidance has a similar dedicated file, `.horon/swarm-prompt.md`.
  `/swarm-prompt` edits the active project file or the global file. New agents read the latest
  content immediately, while running agents keep the prompt captured at session creation, which
  keeps tool definitions and the context cache stable.

### Direct SDK override

An SDK caller can replace **the whole assembled system prompt** when creating a session, without
writing a file.

```typescript
const session = await client.createSession({
  workingDir: process.cwd(),
  systemPrompt: "You are a concise programming tutor.",
});
```

Rust callers use `create_session_with_options(CreateSessionOptions { working_dir: None, system_prompt: Some("You are a concise programming tutor.".into()) })`. The existing `create_session(working_dir)` API still works.

Unlike `system-prompt.md`, which replaces only the base layer, this option replaces the entire
assembled prompt. Omitting the option keeps the usual HoronAgent prompting. An empty string
explicitly chooses an empty system prompt. The override belongs to that session, persists for
resume, and is inherited by forks. Attaching to an existing session does not change its prompt.
This feature requires a daemon version that supports the `system_prompt` session creation field.

## Skills

A skill is one `SKILL.md` plus the files in its folder. The "Available Skills" section of the system prompt holds only names and one-line descriptions; bodies are read when used.

### Sources and priority

Sources live in one table, `crates/base/src/skill/sources.rs`. Working-folder sources come before every user source, and within one level the higher priority takes the name.

| Level | Source id | Folder | Priority | When it is on |
| --- | --- | --- | --- | --- |
| Working folder | `horon-agent` | `./.horon/skills` | 100 | Always |
| Working folder | `claude` | `./.claude/skills` | 80 | Always |
| Working folder | `agents` | `./.agents/skills` | 70 | Always |
| User | `horon-agent` | `~/.horon/skills` | 100 | Always |
| User | `claude` | `~/.claude/skills`, skills from installed Claude Code plugins | 80 | `[skills] claude_user = true` |
| User | `agents` | `~/.agents/skills` | 70 | Always |
| User | `codex` | `~/.codex/skills` | 70 | `[skills] codex_user = true` |
| User | `managed` | `~/.horon/managed-skills` (skills the model wrote) | 5 | `[tools] managed_skills = true` |

User folders owned by other tools are not read by default. Reading them would put another agent's instructions ahead of this one's. When a folder with the switch off holds skills, one log line at start says how many are waiting. Nothing copies another tool's skills into `~/.horon/skills` on first run any more. Sources are read concurrently; only the merge order follows the table.

### Frontmatter-only reads and deferred bodies

Building the list reads only the `SKILL.md` frontmatter (YAML between `---`). It reads 4 KiB at a time and stops at the closing line, reading the whole file only when the closing line is not found within 16 KiB. A body is read when the skill is first used and is not re-read while the file keeps the same `FileStamp`.

### Working-folder skills and `stat` rechecks

Working-folder skills are merged per session from that working folder and never enter the shared user list. Sessions in other repositories cannot see each other's project skills. The merged result is kept in a per-working-folder cache (up to 64 entries), and each request checks only the source folders and each `SKILL.md`'s `metadata`. When nothing changed, no file is read; only changed files are re-read. Edits, additions, deletions, and symlink target changes show up on the next request. `reload_all` in `skill_manage` clears that working folder's cache.

On Unix, `FileStamp` looks at length, modification time in nanoseconds, change time (ctime), inode, and device number. So a file edited to the same size with its modification time restored is still caught by ctime. Windows has no ctime or inode, so only length, modification time, and creation time are checked. A file edited to the same size with its modification time restored may not show up on the next request on Windows; use `skill_manage` `reload_all` to re-read.

### Name collisions

- Two files whose contents and frontmatter match byte for byte are treated as one skill and the later one is dropped silently.
- When contents differ, both are kept. The earlier source keeps the original name and the later one becomes `<source id>/<name>`. If that is taken too, `<source id>/<name>~2`, `~3` follow in order.
- A frontmatter name containing `/` or `\` skips that skill, because `/` is used for namespaces and `skill://` paths.
- Managed skills receive no namespace. When a name collides with a human-written skill it is kept out of the list and the collision is logged.

### Reading and invoking skills

- The model reads `skill://<name>` with `read` to get the body. The result starts with `## Skill: <name>` and the skill folder (`Base directory`). `skill://<name>/<path>` reads a file inside that folder. The detailed rules are in the "Reading skills" section of [`TOOLS.md`](TOOLS.md).
- A user invokes a skill with `/<name> task` or `/skill:<name> task`. Only registered names match after `/skill:`, and when the first token looks like a file path (it contains a dot or a backslash) it is not treated as an invocation even if it equals a registered name. That protects input such as `/tmp/x.png` dropped into the terminal.
- When the list exceeds 48 KiB, descriptions are dropped first; if it still overflows, the name list is cut and the missing count is recorded. Skipped skills remain usable through `skill://` and `/skill:`.

## Token estimation

This explains the estimator that measures how much context remains and where that estimate is used. Compaction must measure conversation size before a provider reports usage, and dividing the character count by 4 went wrong outside English prose. This document records the ratio constants, the word-run model, the measured band, and the remaining error in the current tree.

### Why bytes/4 is wrong

These are values from counting the same repository files with real tokenizers (o200k_base, cl100k_base) and comparing them with the existing `bytes/4`. Measurements used tiktoken 0.14.0.

| Content | o200k error | cl100k error | What that error causes |
| --- | --- | --- | --- |
| Rust source | +13.4% | +13.5% | Overestimates and discards context that is still usable. |
| Korean documents | −7.0% | −31.4% | Underestimates and sends requests past the window. |
| JSON | −32.0% | −32.3% | Underestimates, so compaction is late and the provider rejects the request. |
| TOML | −9.9% | −9.5% | Also on the low side. |

Both directions cost. Overestimating erases a healthy conversation; underestimating sends a request past the window and gets a 400. So the estimator counts by class of character rather than by a single character count.

### Per-class ratios

Every character falls into exactly one class. No character falls into none and costs 0 tokens. The ratio constants are in `crates/token-estimate/src/lib.rs`.

| Class | o200k | cl100k | Location |
| --- | --- | --- | --- |
| Hangul | 0.78 | 1.22 | `crates/token-estimate/src/lib.rs:129` |
| Han | 0.95 | 1.90 | `crates/token-estimate/src/lib.rs:141` |
| Kana | 0.95 | 1.04 | `crates/token-estimate/src/lib.rs:151` |
| Arabic script | 0.38 | 0.88 | `crates/token-estimate/src/lib.rs:159` |
| Indic script | 0.38 | 1.20 | `crates/token-estimate/src/lib.rs:168` |
| Other alphabets | 0.30 | 0.50 | `crates/token-estimate/src/lib.rs:177` |
| Non-ASCII symbols | 0.90 | 1.00 | `crates/token-estimate/src/lib.rs:186` |
| Emoji | 2.00 | 2.00 | `crates/token-estimate/src/lib.rs:196` |
| ASCII alphanumerics | 4.87 chars/token | 4.87 chars/token | `crates/token-estimate/src/lib.rs:73` |
| ASCII punctuation | 0.55 | 0.55 | `crates/token-estimate/src/lib.rs:77` |

The two tokenizer families diverge most on Hangul. o200k merges common Korean substrings into one token, and cl100k spends nearly one token per syllable. The same sentence is therefore 44 tokens under o200k and 72 under cl100k.

### Word-run units

Characters are counted by class, but the sum is not used directly. Each whitespace-delimited run is computed separately (`crates/token-estimate/src/lib.rs:284`), because the same characters cost fewer tokens when bound inside a word. A linear model with a fixed weight per character cannot hit the Rust source and JSON benchmarks at once.

- A run is at least 1 token. An empty string also returns 1 (`crates/token-estimate/src/lib.rs:213`); returning 0 would let the caller read for free.
- ASCII punctuation is added on top of a run's body, because brackets and commas split far finer than words.
- A run containing Hangul does not add punctuation again. Hangul syllables already make up most of the run, and adding punctuation again overestimated the Korean samples.
- A run whose punctuation is half its characters or more is treated as symbol-dense. Its ASCII alphanumerics are not averaged at prose ratios; each piece between separators counts as `ceil(length / 4.87)` (minimum 1 token) (`crates/token-estimate/src/lib.rs` `SEGMENT_DENSITY_PERCENT`). Lowering the threshold to 25% or 35% overestimated a short JSON benchmark by 30%, so it was set to 50%. This rule raised `TOKEN_ESTIMATE_VERSION` to `script-aware-v2`.

### CJK merge discount

Han and Kana discount by run length (`crates/token-estimate/src/lib.rs:120`).

```
merge = max(1 - 18.0 / cjk_len, min_merge)
```

`CJK_MERGE_WINDOW` is 18.0 (`crates/token-estimate/src/lib.rs:99`). The discount floor differs per class: 0.53 for Han (`crates/token-estimate/src/lib.rs:105`) and 0.70 for Kana (`crates/token-estimate/src/lib.rs:112`).

The problem this solves is that one ratio cannot fit both prose and repetition. In prose, characters merge with neighbours and attach cheaply; repeating the same character at length leaves no neighbour to merge with, and each character costs nearly a token. Tuning the ratio for prose underestimates repetition, and tuning for repetition overestimates prose. So the ratio fits prose, and a floor keeps short runs from taking the full prose discount.

The floors come from measurement. Both classes have an o200k ratio of 0.95, so the floor is (measured per-character cost of a short run) / 0.95: 0.50 / 0.95 = 0.53 for Han and 0.667 / 0.95 = 0.70 for Kana.

### Why the inverse conversion has its own ceiling

The compaction pipeline has a step that turns a token budget back into a character budget. That inverse does not reuse the estimator; it uses a fixed ceiling of 4 (`crates/compaction-core/src/lib.rs`), named `CHARS_PER_TOKEN_UPPER_BOUND`.

Two roles, two constants. The estimator produces the estimate, and `TOKEN_ESTIMATE_VERSION` separately marks which rules built it. The inverse does not run the estimator's ratios backwards; it keeps its own ceiling. The estimator's fitted ratios have no clean inverse, and the inverse's job is to guarantee an upper bound on "characters that fit this token budget". Asking the estimator for that bound would make the bound depend on the value it is bounding.

### Recovery band and emergency truncation

When the context right after compaction crosses the trigger again, compaction repeats every turn. So the kept tail plus the summary share is held below 80% of the trigger. `COMPACTION_RECOVERY_BAND` is 0.8 (`crates/compaction-core/src/lib.rs`), and `compute_context_budget` lowers the tail allowance after it sets the base tokens.

`recent = min(window-based tail, max(1,024, floor(threshold × 0.8) - summary share))`

The summary share is `min(4,096, threshold / 4)`, since a small window cannot spend 4,096 tokens on a summary. Results per window:

| Window | Trigger | Tail |
| --- | --- | --- |
| 24k, reserve 16,384 | 6,592 | 3,625 |
| 16k, reserve 1,024 | 7,200 | 3,960 |
| 32k, reserve 1,024 | 16,000 | 8,800 |
| 200k, reserve 16,384 | 140,000 | 20,000 |

A large window such as 200k already keeps its tail inside the band, so its value does not change. `recent_tokens_for_window` remains a function that decides the tail from the window alone, and the band is a second step that cuts its result.

Emergency truncation keeps only the head and tail of a tool result when the context exceeds budget (`emergency_truncated_tool_result`). The head keeps half the budget and the tail a quarter, and from the omitted middle it carries over lines containing `error`, `warning`, `failed`, `failure`, `panic`, `exception`, `traceback`, or `fatal`, at most 8 lines, 200 bytes per line, and 600 bytes total. Words match case-insensitively in ASCII and only as whole words. When the tool result's `is_error` is true, the result starts with `"[tool error] "`, so a failed tool's truncated output cannot look successful. The omission marker keeps the phrase `chars truncated for context recovery`, and the whole result, signal lines and header included, stays inside a 4,000-byte budget. Cuts always land on UTF-8 character boundaries.

### Measured band

Seven prose samples were measured under both families. Error is against the measured value.

| Sample | Characters | o200k estimate | o200k measured | Error | cl100k estimate | cl100k measured | Error |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Han prose | 50 | 32 | 29 | +10.3% | 60 | 56 | +7.1% |
| Unspaced Han | 47 | 28 | 28 | +0.0% | 55 | 51 | +7.8% |
| Kana prose | 70 | 49 | 47 | +4.3% | 63 | 65 | −3.1% |
| Cyrillic | 125 | 35 | 32 | +9.4% | 55 | 52 | +5.8% |
| Arabic | 95 | 31 | 29 | +6.9% | 70 | 69 | +1.4% |
| Devanagari | 61 | 20 | 21 | −4.8% | 61 | 66 | −7.6% |
| Korean prose + 4 emoji | 33 | 22 | 22 | +0.0% | 30 | 32 | −6.2% |

All seven samples land within ±10.3% for both families. The table was measured at a commit before the short-run floor was added; that change only affects short runs, so the prose values are unchanged.

### Known limits

| Limit | Measurement |
| --- | --- |
| Repeating one CJK character at length is underestimated. The ratios fit prose, and a real tokenizer splits the repetition per character. | 1,000 Han characters estimated at 933 (6.7% low); 250 Kana (measured 240) estimated at 220 (8.3% low) |
| JSON-shaped content still has a large error. Symbol-dense runs make minified JSON nearly right, but indented JSON is still underestimated. | The same JSON document in `crates/token-estimate/tests/fixtures` (tiktoken 0.14.0, o200k): the indented version is −13.4% against 1,412 measured, the minified version +2.5% against 1,094 measured. The test tolerance is 14%. |

### Observed calibration

Estimator error is calibrated separately per path by the compaction manager (`crates/base/src/compaction/mod.rs`). When a request is sent its estimate is recorded (`note_request_estimate`), and when the provider reports input token usage (`update_observed_input_tokens`) one `measured / estimated` sample is produced.

- A request whose estimate is under 4,096 tokens is not used as a sample, because fixed overhead dominates the ratio.
- Each sample is clamped to 0.6–1.8 and mixed with the existing ratio as `0.7 × existing + 0.3 × sample`.
- Context computation (`effective_token_count_with`) multiplies the estimate by this ratio but never drops it below 0.75, and uses the larger of that value and the last measurement.
- The ratio is discarded when the provider, the model, or the auth path changes, since the tokenizer may differ.
- The current ratio is visible as `calibration_ratio` in the `[compaction/state]` log.