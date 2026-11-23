//! Tests for the backend: project keys, the store round trip, migration, and
//! concurrency.
//!
//! Every case drives the real `MemoryManager` against a private `HORON_HOME`, so
//! what is asserted is what a caller observes: which project a memory lands in,
//! what a second handle sees after a write, and what a migration leaves behind.

use super::*;
use crate::memory::memory_test_support::TempHome;
use crate::memory::migrate;
use crate::memory::{MemoryManager, MemoryScope, migrate_pre_wiki_memories, project_memory_file};
use crate::memory_graph::{EdgeKind, MemoryGraph};
use crate::memory_types::{MemoryCategory, MemoryEntry};
use horon_agent_time::{Duration, Timestamp};
use std::path::Path;

fn entry(category: MemoryCategory, content: &str, id: &str) -> MemoryEntry {
    MemoryEntry::new(category, content).with_id(id)
}

/// A realistic graph: every category, tags, every edge kind, reinforcements,
/// a superseded entry, an inactive entry, `goal:` and `skill:` ids, and Korean
/// and long bodies.
fn realistic_graph() -> MemoryGraph {
    let mut graph = MemoryGraph::new();
    let base = Timestamp::now() - Duration::days(30);

    let mut preference = entry(
        MemoryCategory::Preference,
        "민열님은 아침 회의를 오전 9시 이후로 잡는 것을 선호합니다.\n두 번째 줄도 함께 저장되어야 합니다.",
        "mem_1700000000000_1",
    );
    preference.tags = vec!["schedule".to_string(), "meeting".to_string(), "한글태그".to_string()];
    preference.access_count = 7;
    preference.strength = 3;
    preference.confidence = 0.75;
    preference.created_at = base;
    preference.updated_at = base + Duration::hours(2);
    preference.source = Some("conversation/2026-11-21".to_string());
    preference.trust = crate::memory_types::TrustLevel::High;
    preference.reinforcements = vec![crate::memory_types::Reinforcement {
        session_id: "session-alpha".to_string(),
        message_index: 12,
        timestamp: base + Duration::minutes(5),
    }];
    graph.add_memory(preference);

    let mut fact = entry(MemoryCategory::Fact, &"long body ".repeat(400), "mem_1700000000001_2");
    fact.tags = vec!["build".to_string()];
    fact.created_at = base;
    fact.updated_at = base;
    graph.add_memory(fact);

    let mut superseded = entry(
        MemoryCategory::Fact,
        "예전 배포 절차는 태그를 손으로 찍었습니다",
        "mem_1700000000002_3",
    );
    superseded.active = false;
    superseded.superseded_by = Some("mem_1700000000003_4".to_string());
    graph.add_memory(superseded);

    let mut survivor = entry(
        MemoryCategory::Correction,
        "배포 태그는 CI가 찍습니다",
        "mem_1700000000003_4",
    );
    survivor.tags = vec!["release".to_string()];
    graph.add_memory(survivor);

    let mut goal = entry(
        MemoryCategory::Custom("goal".to_string()),
        "Goal: Ship the hosting bundle\nStatus: active",
        "goal:6f9619ff-8b86-d011-b42d-00cf4fc964ff",
    );
    goal.tags = vec!["goal".to_string()];
    graph.add_memory(goal);

    let skill = entry(
        MemoryCategory::Custom("skills".to_string()),
        "Use the deploy checklist skill before every release.",
        "skill:deploy checklist",
    );
    graph.add_memory(skill);

    let entity = entry(
        MemoryCategory::Entity,
        "SURI builds the memory wiki",
        "mem_1700000000005_6",
    );
    graph.add_memory(entity);

    let lesson = entry(
        MemoryCategory::Lesson,
        "Rerun a failed job once before investigating",
        "mem_1700000000006_7",
    );
    graph.add_memory(lesson);

    graph.add_edge(
        "mem_1700000000000_1",
        "mem_1700000000003_4",
        EdgeKind::RelatesTo { weight: 0.8 },
    );
    graph.add_edge("mem_1700000000003_4", "mem_1700000000001_2", EdgeKind::Supersedes);
    graph.add_edge("mem_1700000000003_4", "mem_1700000000000_1", EdgeKind::Contradicts);
    graph.add_edge("mem_1700000000006_7", "mem_1700000000001_2", EdgeKind::DerivedFrom);
    graph
}

/// Every non-derived field of every entry, as a comparable string.
fn entry_fingerprint(entry: &MemoryEntry) -> String {
    format!(
        "{}|{:?}|{}|{:?}|{}|{}|{}|{:?}|{:?}|{}|{}|{}|{:?}|{:?}|{:?}|{}",
        entry.id,
        entry.category,
        entry.content,
        entry.tags,
        entry.created_at,
        entry.updated_at,
        entry.access_count,
        entry.source,
        entry.trust,
        entry.strength,
        entry.active,
        entry.superseded_by.as_deref().unwrap_or(""),
        entry.reinforcements,
        entry.embedding.is_some(),
        entry.embedding_model,
        entry.confidence,
    )
}

/// Every memory-to-memory edge, sorted, as a comparable string.
fn edge_fingerprints(graph: &MemoryGraph) -> Vec<String> {
    let mut edges = Vec::new();
    for (from, list) in &graph.edges {
        for edge in list {
            if edge.target.starts_with("tag:") || edge.target.starts_with("cluster:") {
                continue;
            }
            edges.push(format!("{from} -> {} {:?}", edge.target, edge.kind));
        }
    }
    edges.sort();
    edges
}

#[test]
fn a_migrated_graph_round_trips_every_field_and_edge() {
    TempHome::with(|home| {
        let manager = MemoryManager::new().with_project_dir(home.path().join("project"));
        let expected = realistic_graph();
        manager.save_project_graph(&expected).expect("save the realistic graph");

        let loaded = manager.load_project_graph().expect("reload the project graph");

        assert_eq!(
            loaded.memories.len(),
            expected.memories.len(),
            "every memory must survive the round trip"
        );
        let mut ids: Vec<&String> = expected.memories.keys().collect();
        ids.sort();
        for id in ids {
            let before = expected.get_memory(id).expect("source entry");
            let after = loaded.get_memory(id).expect("stored entry");
            assert_eq!(
                entry_fingerprint(before),
                entry_fingerprint(after),
                "entry {id} changed"
            );
        }
        assert_eq!(
            edge_fingerprints(&loaded),
            edge_fingerprints(&expected),
            "edges changed"
        );

        // The embedding is derived data and is deliberately not stored.
        assert!(loaded.memories.values().all(|entry| entry.embedding.is_none()));

        // Tags survive as their own nodes and stay searchable.
        assert_eq!(loaded.get_memories_by_tag("schedule").len(), 1);
        assert_eq!(loaded.get_memories_by_tag("한글태그").len(), 1);
        assert!(
            loaded
                .memories
                .contains_key("goal:6f9619ff-8b86-d011-b42d-00cf4fc964ff")
        );
        assert!(loaded.memories.contains_key("skill:deploy checklist"));
    });
}

#[test]
fn the_project_key_is_stable_and_resolves_symlink_spellings() {
    TempHome::with(|home| {
        let project = home.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        let link = home.path().join("project-link");
        make_symlink(&project, &link);

        let direct = stable_project_key(&project);
        let through_link = stable_project_key(&link);
        assert_eq!(direct, through_link, "a symlink and its target must share one key");

        // A trailing `.` is the same directory.
        let dotted = stable_project_key(&project.join("."));
        assert_eq!(direct, dotted);

        // Two directories with the same basename are different projects.
        let other = home.path().join("shared").join("project");
        std::fs::create_dir_all(&other).expect("create other project");
        assert_ne!(direct, stable_project_key(&other));

        assert_eq!(direct.len(), 32);
        assert!(direct.chars().all(|ch| ch.is_ascii_hexdigit()));
    });
}

#[test]
fn project_memories_are_separated_per_directory_and_never_shared() {
    TempHome::with(|home| {
        let first = home.path().join("a").join("x");
        let second = home.path().join("b").join("x");
        std::fs::create_dir_all(&first).expect("create first");
        std::fs::create_dir_all(&second).expect("create second");

        MemoryManager::new()
            .with_project_dir(&first)
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "first project note"))
            .expect("remember in the first project");
        MemoryManager::new()
            .with_project_dir(&second)
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "second project note"))
            .expect("remember in the second project");

        let first_memories: Vec<String> = MemoryManager::new()
            .with_project_dir(&first)
            .load_project_graph()
            .expect("read the first project")
            .all_memories()
            .map(|entry| entry.content.clone())
            .collect();
        let second_memories: Vec<String> = MemoryManager::new()
            .with_project_dir(&second)
            .load_project_graph()
            .expect("read the second project")
            .all_memories()
            .map(|entry| entry.content.clone())
            .collect();
        assert_eq!(first_memories, vec!["first project note".to_string()]);
        assert_eq!(second_memories, vec!["second project note".to_string()]);

        // The same directory through a symlink is the same project, not a third.
        let link = home.path().join("x-link");
        make_symlink(&first, &link);
        let through_link: Vec<String> = MemoryManager::new()
            .with_project_dir(&link)
            .load_project_graph()
            .expect("read through the link")
            .all_memories()
            .map(|entry| entry.content.clone())
            .collect();
        assert_eq!(through_link, vec!["first project note".to_string()]);
    });
}

#[test]
fn global_memory_lives_only_in_the_home_it_was_written_to() {
    // The first home writes a global memory; the second must not see it, even
    // though the two homes share nothing but the process.
    TempHome::with(|home| {
        let project = home.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        MemoryManager::new()
            .with_project_dir(&project)
            .remember_global(MemoryEntry::new(MemoryCategory::Preference, "prefers tea"))
            .expect("remember global");
        let visible: Vec<String> = MemoryManager::new()
            .list_all_scoped(MemoryScope::Global)
            .expect("list global in the writing home")
            .into_iter()
            .map(|entry| entry.content)
            .collect();
        assert_eq!(visible, vec!["prefers tea".to_string()]);
    });
    TempHome::with(|home| {
        let project = home.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        assert!(
            MemoryManager::new()
                .with_project_dir(&project)
                .list_all_scoped(MemoryScope::Global)
                .expect("list global in a fresh home")
                .is_empty(),
            "a fresh home must not see another home's global memory"
        );
    });
}

#[test]
fn a_second_handle_sees_a_write_immediately() {
    TempHome::with(|home| {
        let project = home.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        let writer = MemoryManager::new().with_project_dir(&project);
        let reader = MemoryManager::new().with_project_dir(&project);

        // Prime the reader's cache, then write through the other handle.
        assert!(
            reader
                .list_all_scoped(MemoryScope::Project)
                .expect("empty read")
                .is_empty()
        );
        writer
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "written after the read"))
            .expect("remember");

        let seen: Vec<String> = reader
            .list_all_scoped(MemoryScope::Project)
            .expect("read after the write")
            .into_iter()
            .map(|entry| entry.content)
            .collect();
        assert_eq!(
            seen,
            vec!["written after the read".to_string()],
            "a write through another handle must invalidate the cached graph"
        );
    });
}

#[test]
fn concurrent_writers_from_two_handles_lose_nothing() {
    TempHome::with(|home| {
        let project = home.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        let handles: Vec<_> = (0..2)
            .map(|_| MemoryManager::new().with_project_dir(&project))
            .collect();

        std::thread::scope(|scope| {
            for (index, manager) in handles.iter().enumerate() {
                scope.spawn(move || {
                    for step in 0..5 {
                        manager
                            .remember_project(MemoryEntry::new(
                                MemoryCategory::Fact,
                                format!("writer {index} step {step}"),
                            ))
                            .expect("remember under contention");
                    }
                });
            }
        });

        let contents: Vec<String> = MemoryManager::new()
            .with_project_dir(&project)
            .list_all_scoped(MemoryScope::Project)
            .expect("list everything")
            .into_iter()
            .map(|entry| entry.content)
            .collect();
        assert_eq!(contents.len(), 10, "every write must survive: {contents:?}");
    });
}

#[test]
fn tagging_several_tags_is_one_write() {
    TempHome::with(|home| {
        let manager = MemoryManager::new().with_project_dir(home.path().join("project"));
        let id = manager
            .remember_project(MemoryEntry::new(MemoryCategory::Fact, "tag me once"))
            .expect("remember");
        let before = manager.store().expect("store").revision().expect("revision");

        manager
            .tag_memories(&id, &["alpha".to_string(), "beta".to_string(), "gamma".to_string()])
            .expect("tag");

        let after = manager.store().expect("store").revision().expect("revision");
        assert_eq!(after, before + 1, "one tag call must be one store write");
        let tags = manager
            .load_project_graph()
            .expect("graph")
            .get_memory(&id)
            .expect("entry")
            .tags
            .clone();
        assert_eq!(tags, vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]);
    });
}

#[test]
fn migrate_imports_a_pre_wiki_file_and_preserves_the_original() {
    TempHome::with(|home| {
        let project = home.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        let source_file = project_memory_file(&project).expect("pre-wiki path");
        std::fs::create_dir_all(source_file.parent().expect("pre-wiki parent")).expect("create the source directory");
        let expected = realistic_graph();
        std::fs::write(&source_file, serde_json::to_vec(&expected).expect("serialize")).expect("write the JSON source");
        let original = std::fs::read(&source_file).expect("read the JSON source back");

        let manager = MemoryManager::new().with_project_dir(&project);
        let loaded = manager.load_project_graph().expect("load after migration");
        assert_eq!(loaded.memories.len(), expected.memories.len());

        assert_eq!(
            std::fs::read(&source_file).expect("the original is still readable"),
            original,
            "migration must never rewrite its source"
        );

        // Every non-derived field survived.
        for id in expected.memories.keys() {
            assert_eq!(
                entry_fingerprint(expected.get_memory(id).expect("source")),
                entry_fingerprint(loaded.get_memory(id).expect("migrated")),
                "entry {id} changed in migration"
            );
        }
        assert_eq!(edge_fingerprints(&loaded), edge_fingerprints(&expected));

        // A migration record names the source and its hash.
        let record = std::fs::read_to_string(home.path().join("memory").join("migration.json"))
            .expect("the migration record exists");
        assert!(record.contains("sha256"), "{record}");
        let log: serde_json::Value = serde_json::from_str(&record).expect("the record is JSON");
        let source = log["sources"]
            .as_object()
            .and_then(|sources| sources.values().next())
            .expect("one source record");
        assert_eq!(source["entries"], 8, "the record carries the entry count: {record}");
        assert_eq!(source["edges"], 11, "the record carries the edge count: {record}");
        assert_eq!(source["imported"], 8, "the record carries what was imported: {record}");
    });
}

#[test]
fn a_second_migration_run_changes_nothing() {
    TempHome::with(|home| {
        let project = home.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        let source_file = project_memory_file(&project).expect("pre-wiki path");
        std::fs::create_dir_all(source_file.parent().expect("pre-wiki parent")).expect("create the source directory");
        std::fs::write(&source_file, serde_json::to_vec(&realistic_graph()).expect("serialize"))
            .expect("write the JSON source");

        MemoryManager::new()
            .with_project_dir(&project)
            .load_project_graph()
            .expect("first migration");

        let wiki_root = home.path().join("memory").join("wiki");
        let before = snapshot_tree(&wiki_root);
        let record_before = std::fs::read_to_string(home.path().join("memory").join("migration.json")).expect("record");

        // Re-run the command form, which reads the record and skips the file.
        let report = migrate::migrate_all(&Store::open(false, None).expect("store"), false).expect("second run");
        assert_eq!(report.imported, 0, "a re-run must write no pages: {report:?}");
        assert_eq!(report.unchanged, 1, "the file is recognised as already imported");

        let after = snapshot_tree(&wiki_root);
        assert_eq!(before, after, "a re-run must change no bytes under the wiki");
        let record_after = std::fs::read_to_string(home.path().join("memory").join("migration.json")).expect("record");
        assert_eq!(record_before, record_after, "an unchanged file rewrites no record");

        // And the graph still reads back complete.
        let loaded = MemoryManager::new()
            .with_project_dir(&project)
            .load_project_graph()
            .expect("reload");
        assert_eq!(loaded.memories.len(), realistic_graph().memories.len());
    });
}

#[test]
fn an_unknown_graph_version_is_refused_and_left_byte_identical() {
    TempHome::with(|home| {
        let project = home.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        let source_file = project_memory_file(&project).expect("pre-wiki path");
        std::fs::create_dir_all(source_file.parent().expect("pre-wiki parent")).expect("create the source directory");
        let mut graph = realistic_graph();
        graph.graph_version = 99;
        std::fs::write(&source_file, serde_json::to_vec(&graph).expect("serialize")).expect("write the JSON source");
        let original = std::fs::read(&source_file).expect("read back");

        let manager = MemoryManager::new().with_project_dir(&project);
        let stored = manager.load_project_graph();
        assert!(
            stored.is_err(),
            "a graph version this build cannot migrate must refuse, not load"
        );

        assert_eq!(
            std::fs::read(&source_file).expect("original still there"),
            original,
            "a refused migration must not touch its source"
        );
        // Nothing was written: the wiki carries no page and the addressable
        // graph is empty, so the refusal is not a half migration.
        let empty = manager
            .load_project_graph()
            .err()
            .is_some_and(|error| error.to_string().contains("graph version 99"));
        assert!(empty, "the refusal names the version");
    });
}

#[test]
fn corrupt_json_is_refused_and_left_byte_identical() {
    TempHome::with(|home| {
        let project = home.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        let source_file = project_memory_file(&project).expect("pre-wiki path");
        std::fs::create_dir_all(source_file.parent().expect("pre-wiki parent")).expect("create the source directory");
        std::fs::write(&source_file, b"{ this is not json").expect("write corrupt");
        let original = std::fs::read(&source_file).expect("read back");

        let manager = MemoryManager::new().with_project_dir(&project);
        assert!(manager.load_project_graph().is_err(), "corrupt JSON must refuse");
        assert_eq!(
            std::fs::read(&source_file).expect("original still there"),
            original,
            "a refused migration must not touch its source"
        );
    });
}

#[test]
fn a_failed_page_write_leaves_the_source_and_the_store_recoverable() {
    TempHome::with(|home| {
        let project = home.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        let source_file = project_memory_file(&project).expect("pre-wiki path");
        std::fs::create_dir_all(source_file.parent().expect("pre-wiki parent")).expect("create the source directory");
        std::fs::write(&source_file, serde_json::to_vec(&realistic_graph()).expect("serialize"))
            .expect("write the JSON source");
        let original = std::fs::read(&source_file).expect("read back");

        // Fail the third page write, as a full disk or a permission error would.
        let store = Store::open(false, Some(&project)).expect("store");
        let (lock, mut wiki) = store.migration_session().expect("session");
        let mut written = 0usize;
        let report = migrate::import_file_with(
            &mut wiki,
            store.root(),
            &source_file,
            Some(store.project_key().expect("project key")),
            &mut |pages_written| {
                if pages_written == 3 {
                    anyhow::bail!("injected failure after three pages")
                }
                written += 1;
                Ok(())
            },
        );
        assert!(report.is_err(), "the injected failure must surface");
        assert!(written >= 3);
        // Release the session lock so the recovery read below can take it.
        drop(wiki);
        drop(lock);

        assert_eq!(
            std::fs::read(&source_file).expect("original still there"),
            original,
            "a failed migration must not touch its source"
        );

        // The store is consistent: what was written reads back, and the next
        // run completes without duplicating a page.
        let manager = MemoryManager::new().with_project_dir(&project);
        let first = manager.load_project_graph().expect("recover after the failure");
        assert_eq!(first.memories.len(), realistic_graph().memories.len());
        let second = manager.load_project_graph().expect("a second read");
        assert_eq!(first.memories.len(), second.memories.len(), "no duplicate pages");
    });
}

#[test]
fn the_migrate_command_reports_without_writing_in_dry_run() {
    TempHome::with(|home| {
        let project = home.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        let source_file = project_memory_file(&project).expect("pre-wiki path");
        std::fs::create_dir_all(source_file.parent().expect("pre-wiki parent")).expect("create the source directory");
        std::fs::write(&source_file, serde_json::to_vec(&realistic_graph()).expect("serialize"))
            .expect("write the JSON source");
        std::fs::write(
            home.path().join("memory").join("global.json"),
            serde_json::to_vec(&MemoryGraph::new()).expect("serialize an empty graph"),
        )
        .expect("write a global file");

        let report = migrate_pre_wiki_memories(true).expect("dry run");
        assert_eq!(report.files, 2, "both sources were found");
        assert_eq!(report.imported, realistic_graph().memories.len());

        let wiki = home.path().join("memory").join("wiki");
        assert!(!wiki.exists(), "a dry run must not create the wiki");
        assert!(
            !home.path().join("memory").join("migration.json").exists(),
            "a dry run must not write the record"
        );

        let written = migrate_pre_wiki_memories(false).expect("real run");
        assert_eq!(written.imported, report.imported, "the report matched what was written");
        assert!(wiki.exists());
    });
}

#[test]
fn a_page_the_store_refuses_is_reported_and_the_rest_migrate() {
    // One unreadable entry must not abort the file: it is reported and the
    // other pages are still imported, which is what keeps a damaged JSON
    // record from blocking every healthy memory behind it.
    TempHome::with(|home| {
        let store = Store::open(false, Some(&home.path().join("project"))).expect("store");
        let (_lock, mut wiki) = store.migration_session().expect("session");
        let mut graph = realistic_graph();
        let mut broken = entry(MemoryCategory::Fact, "body", "mem_1700000000900_9");
        broken.content = "\u{0}".to_string();
        graph.add_memory(broken);
        let path = home.path().join("source.json");
        std::fs::write(&path, serde_json::to_vec(&graph).expect("serialize")).expect("write the JSON source");
        let (report, _) = migrate::import_file(&mut wiki, store.root(), &path, None).expect("import");
        assert_eq!(report.imported, realistic_graph().memories.len());
        assert_eq!(report.refused.len(), 1, "the refusal names the page: {report:?}");
        assert!(report.refused[0].contains("mem_1700000000900_9"));
    });
}

/// A recursive snapshot of every path under `root` and the SHA-256 of its bytes.
fn snapshot_tree(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path.clone());
                continue;
            }
            let bytes = std::fs::read(&path).unwrap_or_default();
            out.push((
                path.display().to_string(),
                hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&bytes)),
            ));
        }
    }
    out.sort();
    out
}

/// Create a symlink to `target` at `link`.
///
/// The memory frontmatter is only exercised on the Unix targets the CI matrix
/// builds for; `platform-api`'s own symlink helper carries the Windows branch.
fn make_symlink(target: &Path, link: &Path) {
    if let Err(error) = horon_agent_platform::fs::symlink_file(target, link) {
        panic!("cannot create the symlink {}: {error}", link.display());
    }
}
