//! Micro-benchmarks for Wave 2b `gaps-base` optimization paths in `horon-agent-base`.
//!
//! Exercises `SkillRegistry::list` (with env unset and with `HORON_AGENT_SKILLS`)
//! and `workspace_dirs` (`canonical_roots`, `Session::add_additional_dir`,
//! `Session::remove_additional_dir`) under a release build with a counting allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use horon_agent_base::env;
use horon_agent_base::session::Session;
use horon_agent_base::skill::{NO_SKILLS_ENV, SKILLS_ALLOW_ENV, SkillRegistry};
use horon_agent_base::workspace_dirs::canonical_roots;

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

struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: Option<&str>) -> Self {
        let previous = std::env::var_os(key);
        match value {
            Some(val) => env::set_var(key, val),
            None => env::remove_var(key),
        }
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(val) => env::set_var(self.key, val),
            None => env::remove_var(self.key),
        }
    }
}

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

fn create_skill_fixture(root: &Path, count: usize) -> Result<(), Box<dyn Error>> {
    const PREFIXES: [&str; 10] = [
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
    ];
    let skills_root = root.join(".horon").join("skills");
    for i in 0..count {
        let prefix = PREFIXES[i % PREFIXES.len()];
        let name = format!("{prefix}-skill-{i:03}");
        let dir = skills_root.join(&name);
        fs::create_dir_all(&dir)?;
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Benchmark skill {i}\n---\n\nBody {i}\n"),
        )?;
    }
    Ok(())
}

fn create_workspace_dirs_fixture(root: &Path) -> Result<(Vec<String>, Vec<String>), Box<dyn Error>> {
    let mut unique_dirs = Vec::with_capacity(20);
    for i in 0..20 {
        let dir = root.join(format!("dir-{i:02}"));
        fs::create_dir_all(&dir)?;
        unique_dirs.push(dir.to_string_lossy().into_owned());
    }
    let sub = root.join("dir-03").join("nested");
    fs::create_dir_all(&sub)?;

    let mut raw_with_aliases = Vec::with_capacity(20);
    for dir in unique_dirs.iter().take(15) {
        raw_with_aliases.push(dir.clone());
    }
    raw_with_aliases.push(format!("{}/.", unique_dirs[0]));
    raw_with_aliases.push(format!("{}/../dir-01", unique_dirs[0]));
    raw_with_aliases.push(format!("{}/", unique_dirs[2]));
    raw_with_aliases.push(format!("{}/..", sub.display()));
    raw_with_aliases.push(format!("{}/./.", unique_dirs[4]));

    Ok((raw_with_aliases, unique_dirs))
}

fn main() -> Result<(), Box<dyn Error>> {
    let skill_fixture = tempfile::tempdir()?;
    create_skill_fixture(skill_fixture.path(), 200)?;
    let registry = SkillRegistry::load_project_overlay(Some(skill_fixture.path()))?;

    {
        let _no_skills = EnvVarGuard::set(NO_SKILLS_ENV, None);
        let _allow = EnvVarGuard::set(SKILLS_ALLOW_ENV, None);
        measure("gaps_skill_registry_list_no_env_500", || {
            let mut total = 0usize;
            for _ in 0..500 {
                let list = registry.list();
                total = total.wrapping_add(list.len());
                black_box(&list);
            }
            black_box(total);
            Ok(())
        })?;
    }

    {
        let _no_skills = EnvVarGuard::set(NO_SKILLS_ENV, None);
        let _allow = EnvVarGuard::set(SKILLS_ALLOW_ENV, Some("alpha-*,beta-*,gamma-*,delta-*"));
        measure("gaps_skill_registry_list_filtered_500", || {
            let mut total = 0usize;
            for _ in 0..500 {
                let list = registry.list();
                total = total.wrapping_add(list.len());
                black_box(&list);
            }
            black_box(total);
            Ok(())
        })?;
    }

    let dirs_fixture = tempfile::tempdir()?;
    let (raw_with_aliases, unique_dirs) = create_workspace_dirs_fixture(dirs_fixture.path())?;

    measure("gaps_workspace_dirs_canonical_roots_20", || {
        let mut total = 0usize;
        for _ in 0..50 {
            let roots = canonical_roots(&raw_with_aliases)?;
            total = total.wrapping_add(roots.len());
            black_box(roots);
        }
        black_box(total);
        Ok(())
    })?;

    let mut session = Session::create_with_id("bench-gaps-dirs".to_string(), None, None);
    measure("gaps_workspace_dirs_add_remove_20", || {
        let mut changed = 0usize;
        for dir in &unique_dirs {
            if session.add_additional_dir(dir)? {
                changed += 1;
            }
        }
        for dir in &unique_dirs {
            if session.remove_additional_dir(dir) {
                changed += 1;
            }
        }
        black_box(changed);
        Ok(())
    })?;

    Ok(())
}
