//! Micro-benchmarks for round-3 memory optimization paths in `horon-agent-base`.
//!
//! Exercises `MemoryWiki::open`, `MemoryWiki::reindex`, `MemoryWiki::write_page`,
//! `MemoryWiki::list`, `MemoryWiki::search`, and `MemoryWiki::delete_page` under
//! the release bench profile with a deterministic counting allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::error::Error;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use horon_agent_base::memory::store::{ListFilter, MAX_BODY_BYTES, MemoryWiki, SearchQuery};
use horon_agent_memory_types::page::{PageId, PageKind, PageLink, PageLinkKind};
use horon_agent_memory_types::{MemoryCategory, MemoryPage};
use horon_agent_time::Timestamp;

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
    println!(
        "BENCH {name}: runs={RUNS} min_us={} med_us={} max_us={} allocs={last_allocs} alloc_bytes={last_bytes}",
        times_us[0],
        times_us[RUNS / 2],
        times_us[RUNS - 1]
    );
    Ok(())
}

fn ts(seconds: i64) -> Result<Timestamp, Box<dyn Error>> {
    Timestamp::from_timestamp_millis(seconds * 1_000).ok_or_else(|| "timestamp out of range".into())
}

fn build_page(idx: usize, base_sec: i64, body_repeat: usize) -> Result<MemoryPage, Box<dyn Error>> {
    let kind = match idx % 6 {
        0 => PageKind::Preference,
        1 => PageKind::Fact,
        2 => PageKind::Person,
        3 => PageKind::Entity,
        4 => PageKind::Procedure,
        _ => PageKind::Event,
    };
    let category = match kind {
        PageKind::Preference => MemoryCategory::Preference,
        PageKind::Person | PageKind::Entity => MemoryCategory::Entity,
        PageKind::Procedure => MemoryCategory::Lesson,
        PageKind::Fact | PageKind::Event => MemoryCategory::Fact,
        _ => MemoryCategory::Fact,
    };
    let id_str = format!("mem_{idx:04}_b");
    let id = PageId::parse(&id_str)?;
    let now = ts(base_sec + i64::try_from(idx).unwrap_or(0) * 60)?;
    let title = format!("아침 회의와 배포 일정 #{idx} release checklist");
    let snippet = format!(
        "아침 9시 이전 회의를 선호한다. 김서연 팀장과 박준호 매니저가 테스트 격리와 빌드 잠금을 확인한다. \
         Deploy release checklist item {idx} uses isolated home and storage sync.\n"
    );
    let body = snippet.repeat(body_repeat);
    let mut page = MemoryPage::draft(id, kind, category, title, body, now);
    page.project = if idx.is_multiple_of(3) {
        Some("alpha".to_string())
    } else if idx % 3 == 1 {
        Some("beta".to_string())
    } else {
        None
    };
    page.tags = vec![
        format!("tag{}", idx % 4),
        format!("tag{}", (idx + 1) % 4),
        "shared".to_string(),
    ];
    page.entities = vec!["김서연".to_string(), "SURI".to_string(), format!("Project{}", idx % 5)];
    if idx > 0 {
        let prev = PageId::parse(&format!("mem_{:04}_b", idx - 1))?;
        page.links.push(PageLink::relates_to(prev, 0.75));
    }
    if idx > 1 {
        let prev2 = PageId::parse(&format!("mem_{:04}_b", idx - 2))?;
        page.links.push(PageLink::new(PageLinkKind::DerivedFrom, prev2));
    }
    page.active = idx % 10 != 9;
    Ok(page)
}

fn seed_wiki(root: &Path, count: usize, include_oversized: bool) -> Result<(), Box<dyn Error>> {
    let mut wiki = MemoryWiki::open(root)?;
    for idx in 0..count {
        let page = build_page(idx, 1_700_000_000, 4)?;
        wiki.write_page(&page)?;
    }
    if include_oversized {
        let oversized_path = wiki.wiki_root().join("facts").join("oversized_bench.md");
        if let Some(parent) = oversized_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let header = "---\nschema: 1\nid: \"oversized_bench\"\ntitle: \"oversized\"\nkind: \"fact\"\n---\n";
        let mut payload = Vec::with_capacity(MAX_BODY_BYTES + 128 * 1024);
        payload.extend_from_slice(header.as_bytes());
        payload.resize(MAX_BODY_BYTES + 128 * 1024, b'a');
        fs::write(&oversized_path, payload)?;
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let open_dir = tempfile::tempdir()?;
    seed_wiki(open_dir.path(), 300, true)?;

    measure("mem_open_unchanged_300", || {
        let wiki = MemoryWiki::open(open_dir.path())?;
        black_box(wiki.last_report().unchanged);
        Ok(())
    })?;

    let wiki = MemoryWiki::open(open_dir.path())?;
    let list_filter = ListFilter {
        kinds: vec![PageKind::Fact, PageKind::Preference],
        project: Some("alpha".to_string()),
        tags: vec!["tag0".to_string(), "tag1".to_string()],
        include_inactive: false,
        limit: Some(10),
    };
    measure("mem_list_tagged_300x50", || {
        let mut total = 0usize;
        for _ in 0..50 {
            let rows = wiki.list(&list_filter)?;
            total = total.wrapping_add(rows.len());
        }
        black_box(total);
        Ok(())
    })?;

    let queries = [
        SearchQuery {
            text: "아침회의선호".to_string(),
            limit: Some(10),
            kinds: vec![PageKind::Preference, PageKind::Fact],
            project: None,
            tags: vec!["tag0".to_string()],
            at: None,
        },
        SearchQuery {
            text: "김서연 배포".to_string(),
            limit: Some(10),
            kinds: vec![PageKind::Preference, PageKind::Fact, PageKind::Person],
            project: None,
            tags: vec!["shared".to_string()],
            at: None,
        },
        SearchQuery {
            text: "release checklist".to_string(),
            limit: Some(10),
            kinds: Vec::new(),
            project: Some("alpha".to_string()),
            tags: vec!["tag0".to_string()],
            at: None,
        },
        SearchQuery {
            text: "테스트격리 빌드잠금".to_string(),
            limit: Some(10),
            kinds: Vec::new(),
            project: None,
            tags: Vec::new(),
            at: None,
        },
    ];
    measure("mem_search_tagged_cjk_20", || {
        let mut total = 0usize;
        for step in 0..20 {
            let res = wiki.search(&queries[step % queries.len()])?;
            total = total.wrapping_add(res.hits.len());
        }
        black_box(total);
        Ok(())
    })?;
    drop(wiki);

    let reindex_dir = tempfile::tempdir()?;
    seed_wiki(reindex_dir.path(), 250, false)?;
    let mut reindex_wiki = MemoryWiki::open(reindex_dir.path())?;
    measure("mem_reindex_250", || {
        let report = reindex_wiki.reindex()?;
        black_box(report.indexed);
        Ok(())
    })?;

    let write_dir = tempfile::tempdir()?;
    let mut write_wiki = MemoryWiki::open(write_dir.path())?;
    let mut large_page = build_page(1, 1_700_000_000, 180)?;
    let mut write_tick = 0i64;
    measure("mem_write_page_large_5", || {
        let mut revs = 0i64;
        for _ in 0..5 {
            write_tick += 1;
            large_page.updated_at = ts(1_700_100_000 + write_tick)?;
            let outcome = write_wiki.write_page(&large_page)?;
            revs = revs.wrapping_add(outcome.revision);
        }
        black_box(revs);
        Ok(())
    })?;

    let delete_dir = tempfile::tempdir()?;
    seed_wiki(delete_dir.path(), 250, false)?;
    let mut delete_wiki = MemoryWiki::open(delete_dir.path())?;
    let mut delete_idx = 0usize;
    measure("mem_delete_page_250", || {
        let target = PageId::parse(&format!("mem_{delete_idx:04}_b"))?;
        delete_idx += 1;
        let outcome = delete_wiki.delete_page(&target)?;
        black_box(outcome.file_removed);
        Ok(())
    })?;

    Ok(())
}
