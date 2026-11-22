# The memory wiki

Language: **English** | [한국어](MEMORY.ko.md)

The memory wiki is the per-user storage engine behind memory: one markdown file per page as the source of truth, and a SQLite index that can always be rebuilt from those files. It stores no embeddings and needs no network round trip. This page describes the layout, the page format, the index schema, the search signals, deletion and rebuild semantics, the Korean evaluation, and the current limits. The `memory` tool itself still reads the JSON store; wiring this engine into the tool is tracked separately, so nothing here changes today's tool behavior yet.

## Layout

Everything lives under `$HORON_HOME/memory` (or `~/.horon/memory` without `HORON_HOME`):

```text
memory/
  wiki/
    preferences/<id>.md
    people/<id>.md          # kind person
    entities/<id>.md
    facts/<id>.md
    procedures/<id>.md
    corrections/<id>.md
    lessons/<id>.md
    custom/<id>.md
    events/<YYYY>/<MM>/<id>.md
    preferences/<project>/<id>.md   # a project-scoped page
  index.sqlite
  index.sqlite-wal
  index.sqlite-shm
```

The directory above `wiki/` is the store root; the directory above that is `$HORON_HOME`. A user volume therefore contains exactly its own memory, and a container mounts only that user's volume. The wiki directory is created `0700`, every page file `0600`.

### Page id to file name

The id is not the file name. Only a plain id — lowercase ASCII letters, digits, `.`, `_`, `-`, starting with a letter or digit — is used verbatim. Every other id (`goal:6f96…`, `skill:deploy checklist`, an uppercase or non-ASCII id) is stored as `~` followed by the lowercase hex of its UTF-8 bytes. `~` is outside the plain alphabet, so the mapping is reversible and unambiguous; lowercase-only plain names mean a case-insensitive filesystem (macOS, Windows) cannot collide two ids onto one file. Validation refuses an empty id, an id over 256 bytes, a leading `.` (so `.` and `..` never name a file), a path separator, and any control character. Reads reject file stems that are not in the canonical form, so a hand-copied alias never maps two files to one page.

## Page format

A page is one frontmatter block followed by the memory body:

```text
---
schema: 1
id: "mem_1732000000000_42"
title: "민열님은 아침 회의를 선호"
kind: "preference"
category: "preference"
project: "horon-agent"
tags: ["schedule", "meeting"]
entities: ["민열", "SURI"]
links: [{kind: relates_to, dst: "mem_2_b", weight: 0.5}]
valid_from: "2026-11-01"
valid_to: null
confidence: 0.8
source: "conversation/2026-11-21"
created_at: "2026-11-21T10:00:00Z"
updated_at: "2026-11-21T10:00:00Z"
access_count: 3
trust: "medium"
strength: 2
active: true
superseded_by: null
reinforcements: [{session: "session-1", message_index: 4, timestamp: "2026-11-21T10:00:30Z"}]
---
body text
```

The dialect is hand-written and strict, not YAML. Only the fields above are accepted; an unknown field is refused, so a typo cannot silently drop a value. The rules:

- The file starts with a `---` line and the block ends at the next `---` line; everything after it is the body, verbatim. A `---` line inside the body is just body text.
- One field per line as `key: value`. Keys are lowercase ASCII identifiers and may not repeat. At most 64 fields, at most 8 KiB per line.
- A string is double-quoted. `"`, `\`, newline, carriage return, tab and other control characters are escaped (`\"`, `\\`, `\n`, `\r`, `\t`, `\uXXXX`). An unknown escape, a bare control character, an unclosed string, a misspelled literal and trailing junk are all refused.
- A number is an integer or a decimal; `true`, `false` and `null` are the other scalars.
- A list is `[a, b]`; a list of maps is `[{k: v}, {k: v}]`. `links` and `reinforcements` use the map form.

The writer's output is canonical: writing a page, reading it back and writing it again produces byte-identical text. That is what makes a file safe to hand-edit and diff.

### Fields

| Field | Type | Meaning |
| --- | --- | --- |
| `schema` | integer | Page schema; this build writes `1` and refuses any other value. Absent means 1. |
| `id` | string | Page id. Absent on a hand-written page, which then takes the id from its file stem. |
| `title` | string | Required. Human label, at most 400 characters, no NUL. |
| `kind` | string | Required. `preference`, `person`, `event`, `fact`, `procedure`, `correction`, `lesson`, `entity` or `custom`. Decides the directory and the ranking authority. |
| `category` | string | The memory category the page stands for: `fact`, `preference`, `entity`, `correction`, `lesson` or a custom lowercase name. Absent means the kind's default, which is how a hand-written page still has a category. |
| `project` | string or null | Project key the page belongs to. One safe path component: lowercase letters, digits, `_`, `-`, at most 64 characters. `null` is user-global. |
| `tags` | string list | Free-form tags. At most 64, each at most 64 characters. |
| `entities` | string list | Named people, organizations and projects. At most 128, each at most 128 characters. |
| `links` | map list | `{kind, dst, weight?}` where kind is `relates_to`, `supersedes`, `contradicts` or `derived_from`. `relates_to` requires a weight in `0..=1`; the other kinds carry none. A page may not link to itself or repeat a link. |
| `valid_from`, `valid_to` | date or null | `YYYY-MM-DD` window; an absent bound is open. |
| `confidence` | number | `0..=1`. |
| `source` | string or null | Where the page came from. |
| `created_at`, `updated_at` | timestamp | RFC 3339 in UTC. A hand-written page without them takes its file modification time. |
| `access_count`, `strength` | integer | Retrieval count and consolidation strength. |
| `trust` | string | `high`, `medium` or `low`. |
| `active` | boolean | Whether the page is current. A page that is `active: true` may not name a `superseded_by`. |
| `superseded_by` | string or null | Id of the page that replaced this one. |
| `reinforcements` | map list | `{session, message_index, timestamp}` breadcrumbs. At most 256. |

Everything the existing `MemoryEntry` carries survives this format except the embedding, which is derived data and is deliberately not stored. `MemoryPage::from_entry` and `MemoryPage::to_entry` map the two, including `access_count`, `trust`, `strength`, `active`, `superseded_by`, `reinforcements` and `confidence`; the category is kept in its own field so a custom category name round-trips. Page links map to `EdgeKind` (`relates_to` keeps its weight; tag and cluster edges are not page links).

Kind and authority, highest first: `preference`, `person` and `correction` (5); `procedure`, `lesson` and `entity` (4); `fact` and `custom` (3); `event` (1). The weight is what lets a preference outrank an event of the same relevance.

## Index schema

`index.sqlite` is derived data. `PRAGMA user_version` is the schema version (1 today). The database runs in WAL mode with `secure_delete=ON`, `trusted_schema=OFF` and a five-second busy timeout.

| Object | Contents |
| --- | --- |
| `pages` | One row per page: id, path, kind, category, project, title, body, tags JSON, entities JSON, `updated_ms`, `created_ms`, `active`, validity window, source. |
| `tags` | `(page_id, tag)`, for tag filters. |
| `links` | `(src_id, dst_id, kind, weight)`. |
| `entities` | `(page_id, name, name_norm)`; `name_norm` is the lowercased alphanumeric form used for matching. |
| `versions` | `(page_id, rev, body, created_at)`; `rev` starts at 1. |
| `files` | `(path, page_id, mtime_ns, size)`: the reconciliation uses it to skip unchanged files; the stamp is at nanosecond precision so an update in the same millisecond as the last index commit is not skipped. |
| `pages_fts` | `fts5(unicode61 remove_diacritics 2)` over title, body, tags and entities. |
| `pages_bigram` | `fts5(trigram)` over title and body, for a substring of three or more characters. |
| `pages_grams` | `fts5(unicode61)` over the overlapping-bigram expansion of title and body, for Korean and CJK queries written without spaces. |
| `meta` | Key/value bookkeeping. |

All three full-text tables set the FTS5 `secure-delete` option, so a deleted term is overwritten instead of left in a free page.

## Search

`MemoryWiki::search` builds candidates from four text streams and one link stream, then fuses them by reciprocal-rank fusion with constant 60:

| Stream | Finds | Weight |
| --- | --- | --- |
| `pages_fts` | query words, through the token index | 1.0 |
| `pages_bigram` | any substring of three or more characters | 1.0 |
| `pages_grams` | CJK bigrams, so `아침회의선호` reaches `아침 … 회의를 선호` | 1.0 |
| entities | a query that contains an entity name from the frontmatter | 1.0 |
| links | one-hop neighbours of the three best text hits | 0.8 |
| recency | candidate order by last write, then first write | 1.0 |
| authority | candidate order by kind authority | 1.5 |

Recency and authority rank only the candidate set: they reorder what text or links found, so an unrelated page never appears just because it is recent. Within one stream, equal evidence gets the same rank (dense ranks), so identical bodies are separated by recency and authority rather than by the alphabetical order of ids. The final order is score, then last write, then id, all explicit, so the same index answers the same query the same way.

Each hit carries the rank it received in every stream, which is what makes an odd order explainable.

Queries are sanitised before they reach SQLite: every term is a quoted FTS5 phrase with an embedded quote doubled, so user text cannot inject FTS5 syntax. A query with no letters or digits returns nothing. A CJK substring shorter than three characters is served by the bigram stream, not the trigram table. Filters narrow the search by kind, project, tag and a validity date (`YYYY-MM-DD`), and the same filters are applied to link neighbours, so a neighbour cannot slip past them.

## Writing, versions and deletion

`MemoryWiki::write_page` writes the file first — temp file in the same directory, fsync, owner-only permissions, rename, parent-directory fsync — and then commits one `BEGIN IMMEDIATE` index transaction that upserts the page, its three full-text rows, tags, entities, links and a new `versions` row. If the process dies between the two, the file is the truth and the next open indexes it. A kind or project change moves the page and removes the previous file after the new one is indexed.

Every write adds one version row holding the new body. Versions are index data: they are deleted with the page, and a full rebuild recreates only the current body because the markdown is the only source of truth.

`MemoryWiki::delete_page` removes the markdown file first, then every index row for the page in one transaction, then sweeps duplicate files that claim the same id (the index can only hold one page per id, so a hand-copied file would otherwise become the survivor), then runs `PRAGMA secure_delete=ON`, the FTS5 `optimize` command, `VACUUM` and `wal_checkpoint(TRUNCATE)`. The order is the contract: a crash after the unlink leaves only a ghost row, and the next open prunes it, while the other order would let a rebuild resurrect the page.

The limit of that guarantee is stated rather than implied: SQLite can zero what it allocated and reclaim free pages, but filesystem journals, SSD wear leveling and any backup snapshot are outside its control. Key destruction at the volume layer is the final measure, and that is the volume lifecycle's job, not this store's.

## Rebuild

`MemoryWiki::open` reconciles the index with the files every time. Files whose modification time (at nanosecond precision) and size match the recorded stamp are skipped; changed files are re-read and re-indexed; index rows whose file is gone are pruned, which is what makes a delete final. The index is rebuilt from scratch when it is absent, carries another schema version, or is a corrupt database (corruption is detected and the file plus its `-wal`/`-shm` sidecars are removed before recreating). A locked index is not reset: deleting a file another writer is using would lose that writer's work.

One malformed page never fails the open and is never deleted. It is reported in the store's last reconcile report with its path and the reason, its previous index rows are left as they were, and the rest of the wiki is indexed. Two files that claim the same page id: the first path in sorted order is indexed and the second is reported as a duplicate.

`MemoryWiki::reindex` clears every derived table and re-reads every file. Because pages are read in path order and the fusion's tie-breaks are explicit, the same files produce the same results before and after, which the tests check by deleting `index.sqlite` and comparing result lists.

Symlinks are never followed: a symlink inside the wiki is not a page, a page path recorded in the index is refused if it escapes the wiki root (`..`, an absolute path, a Windows prefix), and reading a page refuses a path that is a symlink.

## Korean evaluation

The store carries a curated fixture of synthetic Korean assistant memories and the keyword, entity and unspaced forms a Korean-speaking user types (`crates/base/src/memory/store/korean_eval.json`). It is not a public benchmark and no public number is cited here.

- Pages: 14 (preferences, two people, two events, facts, a procedure, a lesson, a correction, an entity).
- Queries: 15, including unspaced forms (`아침회의선호`, `테스트격리`, `빌드잠금`), particle-bearing forms (`디자인 리뷰 일정`, `배포는 언제 하나`) and entity-only forms (`김서연`, `박준호`).
- Method: build the store from the fixture, run each query, count a query as a hit when one of its expected page ids is in the top k.
- Result measured on 2026-11-23: recall@1 15/15, recall@3 15/15, recall@5 15/15. The test pins those numbers, so a ranking change that loses a query fails instead of quietly lowering the bar.

## Limits

- No embeddings and no vector search. Retrieval is lexical, entity and link based; a paraphrase that shares no word, entity or link is not found.
- `pages_bigram` serves substrings of three or more characters; shorter CJK substrings rely on the bigram expansion and on tokens, so a two-syllable query matches only where a bigram or a word lines up.
- Versions are index data. Deleting `index.sqlite` loses the history, keeping the current body.
- No git history inside a user volume, by design: a deletion request must not leave old versions in a repository.
- Physical erasure stops at what SQLite itself allocated; see the deletion section.
- The store is not wired into the `memory` tool yet, and export, import and whole-volume deletion belong to the neighbouring work. Until then the JSON store remains the tool's backing store.
