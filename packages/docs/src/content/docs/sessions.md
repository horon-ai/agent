---
title: Sessions
description: Append-only event-log storage, derived caches, and the commands that inspect and repair it.
section: Using
order: 2
---

There is one store root: `$HORON_HOME/sessions` (normally `~/.horon/sessions`).
Every file is owner-only, every accessor re-validates the session id, and
accessors reject symbolic links, non-directory components and components owned by
another user. A well-formed id therefore cannot point outside the store.

## What is on disk

```
sessions/
  <id>/
    events.jsonl             append-only canonical event log (the original)
    blobs/<sha256>.json      externalized payloads, named by their own hash
    local-head.json          explicit head selection (original)
    local-runtime.json       resume binding: working_dir, migration_epoch, provider_session_id, status
    checkpoint.json          derived cache: head, metadata, compaction, tail
    sparse-index.jsonl       derived cache: every 128th event plus message offsets
    summary.json             derived cache: one catalog row for this session
  <id>.json                  legacy snapshot (preserved, never rewritten)
  .catalog.index.jsonl       derived cache: one row per session, for listing
  .origin-id                 this HOME's writer identity
```

The originals of session content are `events.jsonl` and `blobs/`. Archive,
compaction and rewind append records and never delete an original. Derived caches
can be rebuilt, so a crash never costs content: only the caches are discarded on
load when their stamp does not match.

The runtime journal holds a second copy of the resume binding. Every
`run.started` and `run.stopped` row may carry `binding` with the same fields plus
a `rev`, and the highest `rev` wins on read, so the older of the two originals is
never overwritten.

## Inspecting and repairing

```sh
horon session inspect <id>     # layout, heads, archive state, damage (read-only)
horon session doctor [<id>]    # validate originals without repairing
horon session migrate          # migrate legacy originals (dry run unless --apply)
horon session export <id>      # Markdown transcript; secrets redacted
horon session restore <id>     # restore an archived session
```

`horon export <session> --out file.md` writes the same transcript to a file. Tool
results over 4 KiB are truncated in the export, matching the
`export_session_markdown` harness API reply.

## Runtime state

The durable journal records every accepted input, so a run that stopped mid-turn
can be inspected and settled from outside:

```sh
horon runtime-state show --session <id>
horon runtime-state resolve-input --session <id> --input <id> --action retry
```

`show` lists the live inputs and which outcomes are undetermined. `resolve-input`
takes `retry` (run the message again) or `cancel` (drop it).

## Replication

Two instances replicate whole records and blobs; inbound chunks stage under
`.blob-staging/<sha256>/` and whole-record staging under `.record-transfer/`, so
an interrupted transfer resumes rather than restarting. Every accessor re-checks
the digest, and a mismatched value installs nothing.
