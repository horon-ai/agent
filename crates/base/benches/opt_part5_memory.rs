//! Micro-benchmarks for chunk-5 optimization paths in `horon-agent-base`,
//! `horon-agent-todo-policy`, and `horon-agent-agent-runtime`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::error::Error;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use horon_agent_agent_runtime::{
    RunIdentity, RunStop, StopProvenance, begin_run, outcome_for_stop_reason, provider_failure_provenance, record_stop,
};
use horon_agent_base::env;
use horon_agent_base::memory::export::VolumeLayout;
use horon_agent_base::memory::export::archive::{export as export_archive, import as import_archive};
use horon_agent_base::memory::store::MemoryWiki;
use horon_agent_base::memory::{MemoryManager, MemoryScope, migrate_pre_wiki_memories};
use horon_agent_base::memory_graph::MemoryGraph;
use horon_agent_base::memory_types::page::{PageId, PageKind};
use horon_agent_base::memory_types::{MemoryCategory, MemoryEntry, MemoryPage};
use horon_agent_time::{Duration as TimeDelta, Timestamp};
use horon_agent_todo_policy::TodoItem;
use horon_agent_todo_policy::store::{apply_todo_status_edit, load_snapshot, load_todos_with_revision};

static ALLOCS: AtomicU64 = AtomicU64::new(0);
static ALLOC_BYTES: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
}

struct CountingAlloc;

impl CountingAlloc {
    fn record(bytes: usize) {
        if COUNTING.try_with(Cell::get).unwrap_or(false) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
            ALLOC_BYTES.fetch_add(u64::try_from(bytes).unwrap_or(u64::MAX), Ordering::Relaxed);
        }
    }
}

// SAFETY: forwards every call to `System` and updates only atomics/thread-local bool.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        Self::record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        Self::record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        Self::record(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

const RUNS: usize = 7;

fn measure<F: FnMut() -> Result<(), Box<dyn Error>>>(name: &str, mut work: F) -> Result<(), Box<dyn Error>> {
    // Warmup run outside timing.
    work()?;
    let mut times_us = Vec::with_capacity(RUNS);
    let mut last_allocs = 0u64;
    let mut last_bytes = 0u64;
    for _ in 0..RUNS {
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
        COUNTING.with(|c| c.set(true));
        let start = Instant::now();
        let res = work();
        let elapsed = start.elapsed();
        COUNTING.with(|c| c.set(false));
        res?;
        last_allocs = ALLOCS.load(Ordering::Relaxed) - a0;
        last_bytes = ALLOC_BYTES.load(Ordering::Relaxed) - b0;
        times_us.push(elapsed.as_micros());
    }
    times_us.sort_unstable();
    let min = times_us[0];
    let med = times_us[RUNS / 2];
    let max = times_us[RUNS - 1];
    println!(
        "BENCH {name}: runs={RUNS} min_us={min} med_us={med} max_us={max} allocs={last_allocs} alloc_bytes={last_bytes}"
    );
    Ok(())
}

fn sample_graph(prefix: &str, count: usize) -> MemoryGraph {
    let base = Timestamp::now() - TimeDelta::days(10);
    let mut graph = MemoryGraph::new();
    for i in 0..count {
        let category = match i % 4 {
            0 => MemoryCategory::Preference,
            1 => MemoryCategory::Correction,
            2 => MemoryCategory::Lesson,
            _ => MemoryCategory::Fact,
        };
        let mut entry = MemoryEntry::new(
            category,
            format!("deploy verification note {prefix} {i}: keep atomic writes durable and cache warm for query {prefix}_{i}"),
        )
        .with_id(format!("mem_{prefix}_{i:04}"));
        entry.tags = vec!["deploy".to_string(), "cache".to_string(), format!("tag_{}", i % 5)];
        entry.created_at = base;
        entry.updated_at = base + TimeDelta::minutes(i64::try_from(i).unwrap_or(0));
        entry.access_count = u32::try_from(i % 7).unwrap_or(0);
        entry.refresh_search_text();
        graph.add_memory(entry);
    }
    graph
}

fn seed_wiki(root: &Path, count: usize) -> Result<(), Box<dyn Error>> {
    let mut wiki = MemoryWiki::open(root)?;
    let now = Timestamp::now();
    for i in 0..count {
        let id = PageId::parse(&format!("mem_w_{i:04}"))?;
        let kind = match i % 3 {
            0 => PageKind::Preference,
            1 => PageKind::Lesson,
            _ => PageKind::Fact,
        };
        let category = kind.default_category();
        let mut page = MemoryPage::draft(
            id,
            kind,
            category,
            format!("Release cache rule {i}"),
            format!("Verification item {i} for cache isolation, deploy ordering, and memory index hygiene."),
            now,
        );
        page.project = if i % 2 == 0 { Some("alpha".to_string()) } else { None };
        page.tags = vec!["deploy".to_string(), format!("group_{}", i % 4)];
        wiki.write_page(&page)?;
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    // 1. Memory read paths on 200-entry project + global store.
    let mem_home = tempfile::tempdir()?;
    let proj_dir = mem_home.path().join("workspace");
    fs::create_dir_all(&proj_dir)?;
    env::set_var("HORON_HOME", mem_home.path());
    let manager = MemoryManager::new().with_project_dir(&proj_dir);
    manager.save_project_graph(&sample_graph("proj", 100))?;
    manager.save_global_graph(&sample_graph("glob", 100))?;
    // Warm up cache once before measuring warm cached reads.
    let _ = manager.list_all_scoped(MemoryScope::All)?;

    measure("mem_read_cached_scopes_200", || {
        env::set_var("HORON_HOME", mem_home.path());
        let found = manager.search_scoped("deploy verification", MemoryScope::All)?;
        let prompt = manager.get_prompt_memories_scoped(12, MemoryScope::All);
        let all = manager.list_all_scoped(MemoryScope::All)?;
        let kw = manager.get_relevant_keywords(&["deploy", "cache"], 10)?;
        let stats = manager.graph_stats()?;
        let related = manager.get_related("mem_proj_0010", 1)?;
        black_box((found.len(), prompt.is_some(), all.len(), kw.len(), stats, related.len()));
        Ok(())
    })?;

    // 2. Incremental mutations on a 200-entry project store.
    let inc_home = tempfile::tempdir()?;
    let inc_proj = inc_home.path().join("workspace");
    fs::create_dir_all(&inc_proj)?;
    env::set_var("HORON_HOME", inc_home.path());
    let inc_manager = MemoryManager::new().with_project_dir(&inc_proj);
    inc_manager.save_project_graph(&sample_graph("inc", 200))?;
    let mut toggle = 0usize;
    measure("mem_remember_incremental_on_200", || {
        env::set_var("HORON_HOME", inc_home.path());
        let mut ids = 0usize;
        for step in 0..4 {
            let idx = (toggle + step) % 4;
            let entry = MemoryEntry::new(
                MemoryCategory::Preference,
                format!(
                    "deploy verification note inc {idx}: keep atomic writes durable and cache warm for query inc_{idx}"
                ),
            );
            let id = inc_manager.remember_project(entry)?;
            ids += id.len();
        }
        toggle = (toggle + 1) % 4;
        black_box(ids);
        Ok(())
    })?;

    // 3. Scope page loading on 300-page MemoryWiki.
    let wiki_dir = tempfile::tempdir()?;
    seed_wiki(wiki_dir.path(), 300)?;
    let wiki = MemoryWiki::open(wiki_dir.path())?;
    measure("mem_scope_pages_cold_300", || {
        let proj_pages = wiki.scope_pages(Some("alpha"))?;
        let glob_pages = wiki.scope_pages(None)?;
        black_box((proj_pages.len(), glob_pages.len()));
        Ok(())
    })?;

    // 4. Volume export & import of a 120-page wiki.
    let export_home = tempfile::tempdir()?;
    let src_layout = VolumeLayout::for_home(export_home.path());
    seed_wiki(&src_layout.store_root(), 120)?;
    let archive_path = export_home.path().join("memory-export.tar");
    let mut import_targets = Vec::with_capacity(RUNS + 1);
    for _ in 0..=RUNS {
        import_targets.push(tempfile::tempdir()?);
    }
    let mut import_idx = 0usize;
    measure("mem_volume_export_import_120", || {
        let dst = &import_targets[import_idx];
        import_idx += 1;
        let exp = export_archive(&src_layout, &archive_path)?;
        let dst_layout = VolumeLayout::for_home(dst.path());
        let imp = import_archive(&dst_layout, &archive_path)?;
        black_box((exp.pages, imp.pages));
        Ok(())
    })?;

    // 5. Pre-wiki JSON migration dry run (200 entries).
    let mig_home = tempfile::tempdir()?;
    let mig_dir = mig_home.path().join("memory");
    fs::create_dir_all(&mig_dir)?;
    let mig_bytes = serde_json::to_vec(&sample_graph("mig", 200))?;
    fs::write(mig_dir.join("global.json"), mig_bytes)?;
    measure("mem_migrate_dry_run_200", || {
        env::set_var("HORON_HOME", mig_home.path());
        let report = migrate_pre_wiki_memories(true)?;
        black_box((report.imported, report.files));
        Ok(())
    })?;

    // 6. Todo snapshot loads & revision-checked edits.
    let todo_home = tempfile::tempdir()?;
    let todo_dir = todo_home.path().join("todos");
    fs::create_dir_all(&todo_dir)?;
    let todos: Vec<TodoItem> = (0..30)
        .map(|i| TodoItem {
            id: format!("t{i}"),
            content: format!("Verify milestone {i}"),
            status: if i == 0 {
                "in_progress".to_string()
            } else {
                "pending".to_string()
            },
            priority: "high".to_string(),
            ..TodoItem::default()
        })
        .collect();
    fs::write(todo_dir.join("bench-todo.json"), serde_json::to_vec(&todos)?)?;
    let mut todo_toggle = false;
    measure("todo_load_snapshot_and_edit_50", || {
        let mut total_todos = 0usize;
        for _ in 0..50 {
            let snap = load_snapshot(todo_home.path(), "bench-todo");
            total_todos += snap.todos.len();
        }
        let (_, rev) = load_todos_with_revision(todo_home.path(), "bench-todo");
        let rev = rev.ok_or("missing revision")?;
        let next_status = if todo_toggle { "in_progress" } else { "pending" };
        todo_toggle = !todo_toggle;
        let outcome = apply_todo_status_edit(todo_home.path(), "bench-todo", &rev, "t0", next_status)?;
        black_box((total_todos, outcome.revision.len()));
        Ok(())
    })?;

    // 7. Turn ledger begin/stop + provenance/outcome classification.
    measure("turn_ledger_begin_and_stop_200", || {
        let mut hits = 0usize;
        for i in 0..200usize {
            let run_id = format!("run-{i}");
            begin_run("bench-session", RunIdentity::new(&run_id), true);
            let msg = if i % 2 == 0 {
                "HTTP request Timed Out after 30s"
            } else {
                "Upstream HTTP 502 Bad Gateway"
            };
            let prov = provider_failure_provenance(msg);
            let reason = if i % 2 == 0 {
                Some("max_output_tokens")
            } else {
                Some("end_turn")
            };
            let outcome = outcome_for_stop_reason(reason);
            let _ = record_stop("bench-session", RunStop::capture(run_id, prov));
            if prov == StopProvenance::Timeout {
                hits += 1;
            }
            black_box(outcome);
        }
        black_box(hits);
        Ok(())
    })?;

    env::remove_var("HORON_HOME");
    Ok(())
}
