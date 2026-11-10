//! Explicit full-history serde export and small event metadata.
use super::Session;
use crate::durable_runtime::{Binding, JournalRecord, Payload, RuntimeState};
use anyhow::{Context, Result};
use horon_agent_session_store::{SessionId, SessionMetadata, SessionPaths};
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Value, json};
use std::path::PathBuf;

/// Host-local only. Replica export never carries this execution ownership.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct LocalRuntime {
    pub status: super::SessionStatus,
    pub last_pid: Option<u32>,
    pub last_active_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub working_dir: Option<String>,
    #[serde(default)]
    pub migration_epoch: Option<u64>,
    #[serde(default)]
    pub provider_session_id: Option<String>,
    /// Which version of the binding this file states.
    ///
    /// Absent in every file an older build wrote, and absent from a value that
    /// has not been published yet: the version is assigned when the file is
    /// written, from the journal row that already states it or one above both
    /// copies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<u64>,
}

/// What `local-runtime.json` states right now.
enum MirrorRead {
    Parsed(LocalRuntime),
    /// A file exists but does not parse; a publish replaces it.
    Damaged,
    Missing,
}

/// What a publish does to the mirror.
enum Publish {
    /// The file already states this binding at the newest version.
    Nothing,
    /// Write and flush: the version this publish assigns exists nowhere else.
    Flush(LocalRuntime),
    /// Write by rename only: a journal row already states this version, so a
    /// crash that loses the file loses nothing.
    Rename(LocalRuntime),
}

/// How a publish wrote the mirror, so a test can count the flushes it spent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MirrorWrite {
    Renamed,
    Flushed,
}

/// The two copies of a session binding: the mirror file and the runtime journal.
///
/// The journal states a `rev` with every binding a run row carries, so a mirror
/// that reached the disk before its row can be told apart from one a row
/// already confirms. A mirror without a `rev` is what a build before this one
/// wrote: that build never recorded one, so it may well be the newer of the two
/// and the mirror wins.
pub(super) struct DiskBinding {
    mirror: MirrorRead,
    journal: Option<Binding>,
}

impl DiskBinding {
    /// Read the mirror. The journal is passed in because a load reads it once and
    /// a commit carries the value it already holds.
    fn read(paths: &SessionPaths, id: &SessionId, journal: Option<Binding>) -> Result<Self> {
        Ok(Self {
            mirror: read_mirror(paths, id)?,
            journal,
        })
    }

    /// The journal row that states this session's binding, when one does.
    ///
    /// The loader hands it to the next publish, which may then skip its flush.
    pub(super) fn journal(&self) -> Option<&Binding> {
        self.journal.as_ref()
    }

    /// The newest binding either copy states.
    ///
    /// The higher `rev` wins, a mirror that states none wins, and a session with
    /// neither copy states nothing.
    fn latest(&self) -> Option<LocalRuntime> {
        let journal = self.journal.as_ref().map(LocalRuntime::from);
        match (&self.mirror, journal) {
            (MirrorRead::Parsed(mirror), Some(journal)) => match mirror.rev {
                Some(rev) if journal.rev.unwrap_or(0) > rev => Some(journal),
                _ => Some(mirror.clone()),
            },
            (MirrorRead::Parsed(mirror), None) => Some(mirror.clone()),
            (_, journal) => journal,
        }
    }

    /// The version a new binding takes: one above every version either copy
    /// holds, so a compaction that dropped the journal's binding never hands out
    /// a number the mirror already used.
    fn next_rev(&self) -> u64 {
        let mirror = match &self.mirror {
            MirrorRead::Parsed(mirror) => mirror.rev.unwrap_or(0),
            _ => 0,
        };
        mirror.max(self.journal.as_ref().map_or(0, |journal| journal.rev)) + 1
    }

    /// The journal row that already states `wanted`, if one does.
    fn confirmed(&self, wanted: &LocalRuntime) -> Option<&Binding> {
        self.journal
            .as_ref()
            .filter(|journal| same_binding(&LocalRuntime::from(*journal), wanted))
    }
}

impl From<&Binding> for LocalRuntime {
    fn from(binding: &Binding) -> Self {
        Self {
            status: binding.status.clone(),
            last_pid: binding.last_pid,
            last_active_at: binding.last_active_at,
            working_dir: binding.working_dir.clone(),
            migration_epoch: binding.migration_epoch,
            provider_session_id: binding.provider_session_id.clone(),
            rev: Some(binding.rev),
        }
    }
}

/// Whether two bindings state the same execution: the fields a restart reads.
///
/// `last_active_at` is not one of them. It moves with every batch and only the
/// activity clock reads it, so it never makes a binding owed on its own.
fn same_binding(left: &LocalRuntime, right: &LocalRuntime) -> bool {
    left.status == right.status
        && left.last_pid == right.last_pid
        && left.working_dir == right.working_dir
        && left.migration_epoch == right.migration_epoch
        && left.provider_session_id == right.provider_session_id
}

fn mirror_path(paths: &SessionPaths, id: &SessionId) -> Result<PathBuf> {
    Ok(paths.session_dir(id)?.join("local-runtime.json"))
}

fn read_mirror(paths: &SessionPaths, id: &SessionId) -> Result<MirrorRead> {
    match std::fs::read(mirror_path(paths, id)?) {
        Ok(bytes) => Ok(match serde_json::from_slice(&bytes) {
            Ok(mirror) => MirrorRead::Parsed(mirror),
            Err(_) => MirrorRead::Damaged,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(MirrorRead::Missing),
        Err(error) => Err(error.into()),
    }
}

/// The newest binding the runtime journal carries for one session.
///
/// Read as bytes rather than through the journal store: opening the store
/// creates its directory and takes a writer lock, and a session load must not
/// write. A row this build cannot read is skipped instead of failing the load,
/// because the journal is the copy a missing mirror is rebuilt from, not a gate
/// on opening a session.
pub(super) fn journal_binding(paths: &SessionPaths, id: &SessionId) -> Option<Binding> {
    let home = paths.root().parent()?;
    let path = home
        .join(crate::durable_runtime::JOURNAL_DIR_NAME)
        .join(id.as_str())
        .join(crate::durable_runtime::JOURNAL_FILE_NAME);
    let bytes = std::fs::read(path).ok()?;
    let mut state = RuntimeState::default();
    for line in bytes.split(|byte| *byte == b'\n') {
        let Ok(record) = JournalRecord::from_line(line) else {
            continue;
        };
        match record.payload {
            Payload::RunStarted(row) => {
                if let Some(binding) = row.binding {
                    state.observe_binding(binding);
                }
            }
            Payload::RunStopped(row) => {
                if let Some(binding) = row.binding {
                    state.observe_binding(binding);
                }
            }
            Payload::Checkpoint(row) => {
                if let Some(binding) = row.state.binding {
                    state.observe_binding(binding);
                }
            }
            _ => {}
        }
    }
    state.latest_binding().cloned()
}

impl LocalRuntime {
    pub(super) fn capture(session: &Session) -> Self {
        Self {
            status: session.status.clone(),
            last_pid: session.last_pid,
            last_active_at: session.last_active_at,
            working_dir: session.working_dir.clone(),
            migration_epoch: Some(session.migration_epoch),
            provider_session_id: session.provider_session_id.clone(),
            rev: None,
        }
    }

    /// Adopt these fields as the session's execution binding.
    fn apply(&self, session: &mut Session) {
        session.status = self.status.clone();
        session.last_pid = self.last_pid;
        session.last_active_at = self.last_active_at;
        if let Some(epoch) = self.migration_epoch {
            session.working_dir = self.working_dir.clone();
            session.migration_epoch = epoch;
            session.provider_session_id = self.provider_session_id.clone();
        }
    }

    /// Publish the runtime binding, reporting whether the commit still owes the
    /// session directory a flush.
    ///
    /// The flush of the session directory is left to the commit, which covers
    /// every local original it published with one directory flush. `journal` is
    /// the newest binding the runtime journal holds for this session, when the
    /// caller knows one: a binding a journal row already states is published by
    /// rename alone, because a crash that loses the file still reads that row.
    ///
    /// A commit that only moves the activity clock writes nothing: the binding is
    /// published when a resume-relevant field or the session status changes, so a
    /// turn stays at the state a restart reads without a flush per event batch.
    pub(super) fn publish(&self, paths: &SessionPaths, id: &SessionId, journal: Option<&Binding>) -> Result<bool> {
        let disk = DiskBinding::read(paths, id, journal.cloned())?;
        match self.plan(&disk) {
            Publish::Nothing => Ok(false),
            Publish::Rename(value) => {
                note_mirror_write(id, MirrorWrite::Renamed);
                super::event_store::publish_local_derived(&mirror_path(paths, id)?, &value)?;
                Ok(false)
            }
            Publish::Flush(value) => {
                note_mirror_write(id, MirrorWrite::Flushed);
                super::event_store::publish_local_file(&mirror_path(paths, id)?, &value, paths.sync_mode())
            }
        }
    }

    /// What this publish has to write.
    ///
    /// The value is compared against the newest binding either copy states, not
    /// against the disk file alone: when the journal is ahead of the mirror, a
    /// value that matches the mirror can still be behind what a reader resolves.
    /// The mirror's own version then decides only how the write lands. A mirror
    /// that states no rev, or one that does not parse, is flushed even when a row
    /// confirms the value: an older build may have written it after that row.
    fn plan(&self, disk: &DiskBinding) -> Publish {
        let confirmed = disk.confirmed(self);
        let mut value = self.clone();
        value.rev = Some(match confirmed {
            Some(journal) => journal.rev,
            None => disk.next_rev(),
        });
        let stated = disk.latest().is_some_and(|latest| same_binding(&latest, self));
        if stated {
            // Nothing changes in the value itself. The file still has to catch up
            // when it lags the version a row confirms, and a file that is missing
            // or does not parse still has to be replaced.
            let behind = match (&disk.mirror, confirmed) {
                (MirrorRead::Parsed(mirror), Some(journal)) => mirror.rev.is_none_or(|rev| journal.rev > rev),
                (MirrorRead::Parsed(_), None) => false,
                _ => true,
            };
            if !behind {
                return Publish::Nothing;
            }
        }
        let unversioned = matches!(&disk.mirror, MirrorRead::Parsed(mirror) if mirror.rev.is_none());
        let damaged = matches!(&disk.mirror, MirrorRead::Damaged);
        match confirmed {
            Some(_) if !unversioned && !damaged => Publish::Rename(value),
            _ => Publish::Flush(value),
        }
    }
}

/// Note how the mirror was written, for the tests that count the flushes spent.
fn note_mirror_write(id: &SessionId, write: MirrorWrite) {
    #[cfg(any(test, feature = "test-support"))]
    mirror_writes::record(id.as_str(), write);
    #[cfg(not(any(test, feature = "test-support")))]
    let _ = (id, write);
}

/// The mirror writes this process made, per session.
#[cfg(any(test, feature = "test-support"))]
pub(crate) mod mirror_writes {
    use std::sync::Mutex;

    static WRITES: Mutex<Vec<(String, super::MirrorWrite)>> = Mutex::new(Vec::new());

    pub(super) fn record(session: &str, write: super::MirrorWrite) {
        let mut writes = WRITES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        writes.push((session.to_owned(), write));
    }

    /// Take the writes this session made since the last call.
    #[cfg(test)]
    pub(crate) fn take(session: &str) -> Vec<super::MirrorWrite> {
        let mut writes = WRITES.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (taken, kept) = writes.drain(..).partition(|(id, _)| id == session);
        *writes = kept;
        taken.into_iter().map(|(_, write)| write).collect()
    }
}

impl Session {
    /// Record that the runtime journal now states `binding` for this session.
    ///
    /// Called once a run row carrying the binding is durable. The next publish of
    /// that binding then takes the row's `rev` and writes the mirror by rename
    /// alone, because the journal already answers the same value after a crash.
    pub fn note_journal_binding(&mut self, binding: Binding) {
        self.event_state.confirmed_binding = Some(binding);
    }

    /// Restore the execution binding from the newest of its two copies.
    ///
    /// Returns the copies it read, so the loader can hand the journal's binding
    /// to the next publish. A mirror that does not parse states nothing here: the
    /// journal, or the defaults below, answer instead, and the next publish
    /// replaces it.
    pub(super) fn restore_local_runtime(&mut self, paths: &SessionPaths, id: &SessionId) -> Result<DiskBinding> {
        let disk = DiskBinding::read(paths, id, journal_binding(paths, id))?;
        if let Some(latest) = disk.latest() {
            latest.apply(self);
        } else if !paths
            .session_dir(id)?
            .join("legacy-source-manifest.json")
            .try_exists()?
        {
            // An offline local migration receipt identifies local provenance.
            // Raw replica events, unlike that receipt, cannot select execution cwd/epoch.
            self.working_dir = None;
            self.migration_epoch = 0;
            self.provider_session_id = None;
        }
        Ok(disk)
    }

    pub(super) fn event_metadata(&self) -> Result<SessionMetadata> {
        let mut value = serde_json::to_value(self.journal_meta())?;
        let map = value.as_object_mut().context("metadata is not an object")?;
        for key in ["compaction", "status", "last_pid", "last_active_at", "is_debug"] {
            map.remove(key);
        }
        map.insert("created_at".into(), json!(self.created_at));
        map.insert("route_api_method".into(), json!(self.route_api_method));
        map.insert("migration_epoch".into(), json!(self.migration_epoch));
        map.insert("legacy_extensions".into(), json!(self.legacy_extensions));
        let mut metadata: SessionMetadata = serde_json::from_value(value)?;
        // Local execution bindings are not metadata edits. Preserve a remote
        // explicit NULL (and all other canonical provenance) until the caller
        // actually changes the corresponding local value.
        if let (Some(source), Some(binding)) = (&self.event_state.source_metadata, &self.event_state.local_binding) {
            if self.working_dir == binding.working_dir {
                metadata.working_dir = source.working_dir.clone();
            }
            if binding.migration_epoch == Some(self.migration_epoch) {
                metadata.migration_epoch = source.migration_epoch;
            }
            if self.provider_session_id == binding.provider_session_id {
                metadata.provider_session_id = source.provider_session_id.clone();
            }
        }
        Ok(metadata)
    }

    pub(super) fn auxiliary_value(&self) -> Value {
        let mut value = serde_json::Map::new();
        if let Some(source) = &self.event_state.fork_source {
            value.insert("fork_provenance".into(), source.clone());
        }
        if !self.persist_state.transcript_stripped {
            if !self.event_state.auxiliary_stripped {
                value.insert("env_snapshots".into(), json!(self.env_snapshots));
                value.insert("memory_injections".into(), json!(self.memory_injections));
                value.insert("replay_events".into(), json!(self.replay_events));
                value.insert("stream_rule_ledger".into(), json!(self.stream_rule_ledger));
            }
            value.insert("compaction".into(), json!(self.compaction));
        }
        if !self.persist_state.transcript_stripped
            && !self.event_state.auxiliary_stripped
            && !self.pruned_tool_results.is_empty()
        {
            value.insert("pruned_tool_results".into(), json!(self.pruned_tool_results));
        }
        // `status`, `last_pid` and `last_active_at` are the local runtime binding:
        // `local-runtime.json` is their original and the loader restores them from
        // there. Recording them here as well made every status change (the close at
        // the end of a run) a full log commit for a value no reader takes back.
        value.insert("is_debug".into(), json!(self.is_debug));
        Value::Object(value)
    }
}

impl Serialize for Session {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;
        let mut value = serde_json::to_value(self.journal_meta()).map_err(S::Error::custom)?;
        let map = value
            .as_object_mut()
            .ok_or_else(|| S::Error::custom("invalid session metadata"))?;
        map.extend(self.legacy_extensions.clone());
        map.insert("id".into(), json!(self.id));
        map.insert("created_at".into(), json!(self.created_at));
        map.insert("route_api_method".into(), json!(self.route_api_method));
        map.insert("migration_epoch".into(), json!(self.migration_epoch));
        map.insert(
            "messages".into(),
            json!(self.history_messages().map_err(S::Error::custom)?),
        );
        map.insert("env_snapshots".into(), json!(self.env_snapshots));
        map.insert("memory_injections".into(), json!(self.memory_injections));
        map.insert("replay_events".into(), json!(self.replay_events));
        value.serialize(serializer)
    }
}
