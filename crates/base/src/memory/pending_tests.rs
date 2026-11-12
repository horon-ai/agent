use super::*;
use crate::memory::{MemoryCategory, MemoryEntry, MemoryManager, format_relevant_prompt};
use crate::memory_graph::MemoryGraph;
use std::time::Duration;

struct HomeGuard {
    previous: Option<std::ffi::OsString>,
    _dir: tempfile::TempDir,
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        clear_all_pending_memory();
        match self.previous.take() {
            Some(home) => crate::env::set_var("HORON_HOME", home),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

/// Isolated home plus both global-state locks: pending memory is process-wide
/// state keyed only by session ID, so tests must not overlap each other.
fn fixture(test: impl FnOnce()) {
    let _env = crate::storage::lock_test_env();
    let _pending = super::super::tests::PENDING_MEMORY_TEST_LOCK
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let home = HomeGuard {
        previous: std::env::var_os("HORON_HOME"),
        _dir: tempfile::tempdir().expect("isolated memory home"),
    };
    crate::env::set_var("HORON_HOME", home._dir.path());
    clear_all_pending_memory();
    test();
}

fn manager(project: Option<&str>) -> MemoryManager {
    match project {
        Some(project) => MemoryManager::new().with_project_dir(project),
        None => MemoryManager::new(),
    }
}

fn fact(content: &str) -> MemoryEntry {
    MemoryEntry::new(MemoryCategory::Fact, content)
}

fn save(project: Option<&str>, global: bool, entries: &[MemoryEntry]) {
    let mut graph = MemoryGraph::new();
    for entry in entries {
        graph.add_memory(entry.clone());
    }
    let manager = manager(project);
    if global {
        manager.save_global_graph(&graph).unwrap();
    } else {
        manager.save_project_graph(&graph).unwrap();
    }
}

/// Publish the canonical prompt for `entries`. Publication validates both the
/// prompt text and each entry's stored metadata against disk.
fn publish(session: &str, project: Option<&str>, entries: &[MemoryEntry]) {
    set_pending_memory(
        session,
        format_relevant_prompt(entries, entries.len()).unwrap(),
        entries.len(),
        entries.iter().map(|entry| entry.id.clone()).collect(),
        None,
        project,
    );
}

#[test]
fn project_switch_drops_payload_before_injected_bookkeeping() {
    fixture(|| {
        let entry = fact("Project-specific build command");
        save(Some("/project/a"), false, std::slice::from_ref(&entry));
        // Even an identical ID/content in another project cannot authorize
        // a result selected against the previous project's context.
        save(Some("/project/b"), false, std::slice::from_ref(&entry));
        publish("switch", Some("/project/a"), std::slice::from_ref(&entry));
        assert!(take_pending_memory("switch", Some("/project/b")).is_none());
        assert!(!is_memory_injected("switch", &entry.id));
        assert!(!has_pending_memory("switch"));
        publish("switch", Some("/project/b"), std::slice::from_ref(&entry));
        assert!(take_pending_memory("switch", Some("/project/b")).is_some());
        assert!(is_memory_injected("switch", &entry.id));
    });
}

#[test]
fn new_project_publication_replaces_same_prompt_from_old_project() {
    fixture(|| {
        let entry = fact("A globally stored preference");
        save(None, true, std::slice::from_ref(&entry));
        publish("replace", Some("/project/a"), std::slice::from_ref(&entry));
        publish("replace", Some("/project/b"), std::slice::from_ref(&entry));
        assert!(take_pending_memory("replace", Some("/project/b")).is_some());
    });
}

#[test]
fn forgotten_after_selection_drops_whole_payload() {
    fixture(|| {
        let first = fact("First selected fact");
        let second = fact("Second selected fact");
        let entries = [first.clone(), second.clone()];
        save(Some("/project/a"), false, &entries);
        publish("forgotten", Some("/project/a"), &entries);
        assert!(manager(Some("/project/a")).forget(&first.id).unwrap());
        assert!(take_pending_memory("forgotten", Some("/project/a")).is_none());
        assert!(!is_memory_injected("forgotten", &first.id));
        assert!(!is_memory_injected("forgotten", &second.id));
    });
}

#[test]
fn inactive_or_superseded_memory_is_not_consumed() {
    fixture(|| {
        for superseded in [false, true] {
            let mut entry = fact("Old database port");
            save(None, true, std::slice::from_ref(&entry));
            publish("inactive", None, std::slice::from_ref(&entry));
            if superseded {
                entry.superseded_by = Some("replacement-id".into());
            } else {
                entry.active = false;
            }
            save(None, true, std::slice::from_ref(&entry));
            assert!(take_pending_memory("inactive", None).is_none());
            assert!(!is_memory_injected("inactive", &entry.id));
        }
    });
}

#[test]
fn updated_fact_is_rejected_even_when_disk_mtime_matches_cache() {
    fixture(|| {
        let mut entry = fact("Database port is 5432");
        save(Some("/project/a"), false, std::slice::from_ref(&entry));
        publish("updated", Some("/project/a"), std::slice::from_ref(&entry));
        let path = manager(Some("/project/a")).project_memory_path().unwrap().unwrap();
        let old_mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        entry.content = "Database port is 6432".into();
        let mut graph = MemoryGraph::new();
        graph.add_memory(entry.clone());
        // Rewrite outside MemoryManager, preserving mtime, so a cached graph
        // would still show the old content. Scoped validation must not use it.
        std::fs::write(&path, serde_json::to_vec(&graph).unwrap()).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old_mtime))
            .unwrap();
        assert!(take_pending_memory("updated", Some("/project/a")).is_none());
        assert!(!is_memory_injected("updated", &entry.id));
        publish("updated", Some("/project/a"), std::slice::from_ref(&entry));
        let fresh = take_pending_memory("updated", Some("/project/a")).unwrap();
        assert!(fresh.prompt.contains("6432"));
    });
}

#[test]
fn changed_before_publication_cannot_bind_new_signature_to_old_prompt() {
    fixture(|| {
        let old = fact("Database port is 5432");
        let mut current = old.clone();
        current.content = "Database port is 6432".into();
        save(None, true, std::slice::from_ref(&current));
        publish("publish-race", None, std::slice::from_ref(&old));
        assert!(!has_pending_memory("publish-race"));
        assert!(!is_memory_injected("publish-race", &old.id));
        publish("publish-race", None, std::slice::from_ref(&current));
        assert!(take_pending_memory("publish-race", None).is_some());
    });
}

#[test]
fn tags_only_change_during_selection_is_rejected_before_publication() {
    fixture(|| {
        let mut selected = fact("Use the documented database configuration");
        selected.tags = vec!["postgres".into()];
        let mut current = selected.clone();
        current.tags = vec!["sqlite".into()];
        // Keep content, category and timestamps identical: canonical prompt
        // comparison cannot detect this changed input to the Jev decision.
        let prompt = format_relevant_prompt(std::slice::from_ref(&selected), 1).unwrap();
        assert_eq!(
            Some(prompt.clone()),
            format_relevant_prompt(std::slice::from_ref(&current), 1)
        );
        save(Some("/project/a"), false, std::slice::from_ref(&current));
        set_pending_memory_with_selection(
            "tags-race",
            prompt.clone(),
            1,
            std::slice::from_ref(&selected),
            None,
            Some("/project/a"),
        );
        assert!(!has_pending_memory("tags-race"));
        assert!(!is_memory_injected("tags-race", &selected.id));

        // An evaluation performed against the new metadata remains usable.
        set_pending_memory_with_selection(
            "tags-race",
            prompt,
            1,
            std::slice::from_ref(&current),
            None,
            Some("/project/a"),
        );
        assert!(take_pending_memory("tags-race", Some("/project/a")).is_some());
    });
}

#[test]
fn global_only_binding_never_becomes_an_implicit_project() {
    fixture(|| {
        let global = fact("Prefer concise messages");
        let local = fact("Local-only database setting");
        save(None, true, std::slice::from_ref(&global));
        save(Some("/project/a"), false, std::slice::from_ref(&local));
        publish("global", None, std::slice::from_ref(&global));
        assert!(take_pending_memory("global", None).is_some());
        publish("local-without-scope", None, std::slice::from_ref(&local));
        assert!(!has_pending_memory("local-without-scope"));
        publish("global-switched", None, std::slice::from_ref(&global));
        assert!(take_pending_memory("global-switched", Some("/project/a")).is_none());
        assert!(!is_memory_injected("global-switched", &global.id));
    });
}

#[test]
fn distinct_sessions_keep_independent_pending_and_injected_state() {
    fixture(|| {
        let entry = fact("Shared project fact");
        save(Some("/project/a"), false, std::slice::from_ref(&entry));
        for sid in ["session-a", "session-b"] {
            publish(sid, Some("/project/a"), std::slice::from_ref(&entry));
        }
        assert!(take_pending_memory("session-a", Some("/project/a")).is_some());
        assert!(has_pending_memory("session-b"));
        assert!(!is_memory_injected("session-b", &entry.id));
        assert!(take_pending_memory("session-b", Some("/project/a")).is_some());
    });
}

#[test]
fn graph_load_error_fails_closed_even_for_selected_global_memory() {
    fixture(|| {
        let entry = fact("Globally stored selected fact");
        save(None, true, std::slice::from_ref(&entry));
        save(Some("/project/a"), false, &[]);
        publish("corrupt", Some("/project/a"), std::slice::from_ref(&entry));
        let path = manager(Some("/project/a")).project_memory_path().unwrap().unwrap();
        std::fs::write(path, b"not a memory graph").unwrap();
        assert!(take_pending_memory("corrupt", Some("/project/a")).is_none());
        assert!(!is_memory_injected("corrupt", &entry.id));
        publish("corrupt-publish", Some("/project/a"), std::slice::from_ref(&entry));
        assert!(!has_pending_memory("corrupt-publish"));
    });
}

#[test]
fn same_id_in_project_and_global_is_ambiguous_and_rejected() {
    fixture(|| {
        let entry = fact("Ambiguous stored fact");
        save(Some("/project/a"), false, std::slice::from_ref(&entry));
        save(None, true, std::slice::from_ref(&entry));
        publish("ambiguous", Some("/project/a"), std::slice::from_ref(&entry));
        assert!(!has_pending_memory("ambiguous"));
        assert!(!is_memory_injected("ambiguous", &entry.id));
    });
}

#[test]
fn publication_and_validation_preserve_selected_id_order() {
    fixture(|| {
        let first = fact("First selected fact");
        let second = fact("Second selected fact");
        let entries = [second.clone(), first.clone()];
        save(None, true, &entries);
        publish("ordered", None, &entries);
        let pending = take_pending_memory("ordered", None).unwrap();
        assert_eq!(pending.memory_ids, [second.id, first.id]);
        assert!(pending.prompt.find("Second selected").unwrap() < pending.prompt.find("First selected").unwrap());
    });
}

#[test]
fn pending_memory_freshness_and_clear() {
    fixture(|| {
        // Distinct entries: the second payload must be rejected for age alone,
        // not because the first one already marked its memory injected.
        let fresh = fact("Freshness is measured from publication");
        let stale = fact("An old result is dropped instead of injected");
        save(None, true, &[fresh.clone(), stale.clone()]);
        let sid = "session-freshness";

        publish(sid, None, std::slice::from_ref(&fresh));
        assert!(has_pending_memory(sid));
        let pending = take_pending_memory(sid, None).expect("pending memory");
        assert_eq!(pending.count, 1);
        assert_eq!(pending.memory_ids, [fresh.id.clone()]);
        assert!(!has_pending_memory(sid));

        publish(sid, None, std::slice::from_ref(&stale));
        backdate_pending_memory_for_test(sid, Duration::from_secs(121));
        assert!(
            take_pending_memory(sid, None).is_none(),
            "a payload older than the freshness window must not inject"
        );
        assert!(!has_pending_memory(sid));
        assert!(!is_memory_injected(sid, &stale.id));
    });
}

#[test]
fn pending_memory_suppresses_immediate_duplicate_payloads() {
    fixture(|| {
        let entry = fact("Duplicate suppression keeps one injection per window");
        save(None, true, std::slice::from_ref(&entry));
        let sid = "session-duplicate";

        publish(sid, None, std::slice::from_ref(&entry));
        assert!(take_pending_memory(sid, None).is_some());

        publish(sid, None, std::slice::from_ref(&entry));
        assert!(
            take_pending_memory(sid, None).is_none(),
            "identical payload should be suppressed when repeated immediately"
        );
    });
}

#[test]
fn pending_memory_suppresses_overlapping_memory_sets() {
    fixture(|| {
        let entries: Vec<MemoryEntry> = (0..5).map(|index| fact(&format!("Overlap fact {index}"))).collect();
        save(None, true, &entries);
        let sid = "session-overlap";

        publish(sid, None, &entries);
        assert!(take_pending_memory(sid, None).is_some());

        // Four of the five injected memories again: 0.8 overlap, distinct text.
        publish(sid, None, &entries[..4]);
        assert!(
            take_pending_memory(sid, None).is_none(),
            "same memory set should be suppressed even if prompt text differs"
        );
    });
}

#[test]
fn pending_memory_keeps_existing_similar_payload_instead_of_replacing_it() {
    fixture(|| {
        let entries: Vec<MemoryEntry> = (0..5).map(|index| fact(&format!("Queued fact {index}"))).collect();
        save(None, true, &entries);
        let sid = "session-queued-overlap";

        publish(sid, None, &entries);
        publish(sid, None, &entries[..4]);

        let pending = take_pending_memory(sid, None).expect("existing pending payload should remain");
        assert_eq!(pending.memory_ids.len(), 5);
        assert!(pending.prompt.contains("Queued fact 4"));
    });
}

#[test]
fn pending_memory_suppresses_payload_when_all_ids_already_known() {
    fixture(|| {
        let known = fact("Already injected known fact");
        let fresh = fact("Never injected fact");
        save(None, true, &[known.clone(), fresh.clone()]);
        let sid = "session-known";

        mark_memories_known(sid, std::slice::from_ref(&known.id), "test");
        publish(sid, None, std::slice::from_ref(&known));
        assert!(
            take_pending_memory(sid, None).is_none(),
            "payload made entirely of already-known memories must not inject"
        );

        publish(sid, None, &[known.clone(), fresh.clone()]);
        assert!(
            take_pending_memory(sid, None).is_some(),
            "payload containing an unknown memory should inject"
        );
    });
}
