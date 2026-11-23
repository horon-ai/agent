//! Micro-benchmarks for round-2 optimization paths in `horon-agent-base`.
//!
//! Exercises public `PackageStore` installation/verification and
//! `session::split_rendered_reasoning` under a release build with a counting allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::error::Error;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use horon_agent_base::packages::{InstallRequest, PackageScope, PackageStore};
use horon_agent_base::session::{reasoning_line_markup, split_rendered_reasoning};

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

fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => "x86_64-unknown-linux-gnu",
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

fn create_plugin_fixture(root: &Path, resource_files: usize) -> Result<(), Box<dyn Error>> {
    let ext_dir = root.join("native");
    fs::create_dir_all(ext_dir.join("bin"))?;
    let bin_name = if std::env::consts::OS == "windows" {
        "bin/child.exe"
    } else {
        "bin/child"
    };
    fs::copy(std::env::current_exe()?, ext_dir.join(bin_name))?;
    fs::write(
        ext_dir.join("extension.toml"),
        format!(
            "schema_version = 1\nid = \"bench-pkg\"\nversion = \"1.0.0\"\nname = \"Bench\"\nprotocol = \"1.0\"\n\n[targets]\n{} = \"{bin_name}\"\n",
            host_target()
        ),
    )?;
    let mut commands = Vec::with_capacity(resource_files);
    for i in 0..resource_files {
        let dir = format!("commands/group{}", i % 4);
        fs::create_dir_all(root.join(&dir))?;
        let rel = format!("{dir}/cmd-{i:02}.md");
        let payload = format!("# Command {i}\n").repeat(200);
        fs::write(root.join(&rel), payload)?;
        commands.push(format!("\"{rel}\""));
    }
    fs::write(
        root.join("plugin.toml"),
        format!(
            "schema_version = 1\nid = \"bench-pkg\"\nversion = \"1.0.0\"\nname = \"Bench Plugin\"\nextension = \"native/extension.toml\"\n\n[resources]\ncommands = [{}]\n",
            commands.join(", ")
        ),
    )?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let fixture = tempfile::tempdir()?;
    create_plugin_fixture(fixture.path(), 38)?;

    measure("pkg_install_40_files", || {
        let store_dir = tempfile::tempdir()?;
        let store = PackageStore::new(store_dir.path());
        let record = store.install(InstallRequest::new(fixture.path(), PackageScope::Global))?;
        black_box(record.files.len());
        Ok(())
    })?;

    let verify_dir = tempfile::tempdir()?;
    let verify_store = PackageStore::new(verify_dir.path());
    drop(verify_store.install(InstallRequest::new(fixture.path(), PackageScope::Global))?);
    measure("pkg_verify_40_files", || {
        let report = verify_store.verify()?;
        black_box(report.packages);
        Ok(())
    })?;

    let mut bodies = Vec::with_capacity(1_000);
    for i in 0..1_000 {
        if i % 5 == 0 {
            let mut body = String::new();
            for step in 0..8 {
                body.push_str(&reasoning_line_markup(&format!("reasoning step {step} for item {i}")));
            }
            body.push('\n');
            body.push_str(&format!("Final visible answer for item {i}.\nSecond line of answer."));
            bodies.push(body);
        } else {
            bodies.push(format!(
                "Plain assistant response #{i} without reasoning markup.\n\
                 Includes several lines of markdown prose and code spans `foo_bar({i})`."
            ));
        }
    }
    measure("history_split_reasoning_1000x5", || {
        let mut hits = 0usize;
        for _ in 0..5 {
            for body in &bodies {
                if let Some((r, a)) = split_rendered_reasoning(body) {
                    hits = hits.wrapping_add(r.len() + a.len());
                }
            }
        }
        black_box(hits);
        Ok(())
    })?;

    Ok(())
}
