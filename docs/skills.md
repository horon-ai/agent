# Skills

A skill is a folder with a `SKILL.md` (YAML frontmatter `name` and `description`, then instructions) and any files it refers to. The system prompt lists only skill names and one-line descriptions; the body is read when the skill is used.

## Where skills come from

Working-folder sources come before user sources; within a level the higher priority keeps the name.

| Level | Folder | Priority | On when |
| --- | --- | --- | --- |
| Working folder | `./.horon/skills` | 100 | Always |
| Working folder | `./.claude/skills` | 80 | Always |
| Working folder | `./.agents/skills` | 70 | Always |
| User | `~/.horon/skills` | 100 | Always |
| User | `~/.claude/skills` and Claude Code plugin skills | 80 | `[skills] claude_user = true` |
| User | `~/.agents/skills` | 70 | Always |
| User | `~/.codex/skills` | 70 | `[skills] codex_user = true` |
| User | `~/.horon/managed-skills` (written by the model) | 5 | `[tools] managed_skills = true` |

Two different skills with one name: the earlier source keeps `<name>`, the later becomes `<source>/<name>`. Identical copies collapse into one. A managed skill never shadows a human-written one.

## Use a skill

| Who | How |
| --- | --- |
| The model | `read skill://<name>` returns the whole `SKILL.md` in one call, no line numbers; `read skill://<name>/<file>` reads a file in its folder |
| The user | `/<name> <task>` or `/skill:<name> <task>` in the terminal client |
| A misspelled name | The error lists installed names, so the next call can use the right one |

In the terminal client a skill read shows as `✓ Read skill://<name>`.

## Install or write one

| Need | How |
| --- | --- |
| See every skill and where it came from | `horon skills list [--json]` |
| Install from a repository | `horon skills install <source> [--ref REF] [--name NAME] --approve` (`https://`, `ssh://`, `file://`, `user@host:path`, or a local folder; plain `http://` and `git://` are refused) |
| Let the model write one | `skill_manage` `create`, `update`, `delete`, `install`; these touch only `~/.horon/managed-skills` |
| A project skill for everyone in the repo | Commit `./.horon/skills/<name>/SKILL.md` |

`create`, `update` and `install` need `[tools] managed_skills = true`; plan mode refuses all writing actions. A list over 48 KiB drops descriptions first, then names; skipped skills stay usable by name.
