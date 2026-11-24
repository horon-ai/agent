//! Micro-benchmarks for chunk-5 paths in `horon-agent-app-core` (`approval_grants`)
//! and `horon-agent-harness-api-server` (`credentials`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::error::Error;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Result<Self, Box<dyn Error>> {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let id = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("horon-opt5-core-{}-{id}", std::process::id()));
        if path.exists() {
            fs::remove_dir_all(&path)?;
        }
        fs::create_dir_all(&path)?;
        horon_agent_platform::fs::set_dir_permissions_owner_only(&path)?;
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        match fs::remove_dir_all(&self.path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                eprintln!("bench temp cleanup failed for {}: {err}", self.path.display());
            }
        }
    }
}

use horon_agent_app_core::approval_grants;
use horon_agent_app_core::env;
use horon_agent_harness_api_server::credentials::CredentialStore;
use horon_agent_time::{Duration as TimeDelta, Timestamp};
use serde_json::json;

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
    let min = times_us[0];
    let med = times_us[RUNS / 2];
    let max = times_us[RUNS - 1];
    println!(
        "BENCH {name}: runs={RUNS} min_us={min} med_us={med} max_us={max} allocs={last_allocs} alloc_bytes={last_bytes}"
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    // 1. Scoped approval grants listing on 40 stored grants.
    let home = TempDir::new()?;
    env::set_var("HORON_HOME", home.path());
    let now = Timestamp::now();
    let future = now + TimeDelta::hours(24);
    let grants: Vec<_> = (0..40)
        .map(|i| {
            let path = if i % 10 == 0 {
                format!("/work/repo/dir_{i}/file\u{001b}name.txt")
            } else {
                format!("/work/repo/dir_{i}/clean_file_name.txt")
            };
            json!({
                "workspace": "/work/repo",
                "path": path,
                "scope": if i % 2 == 0 { "path" } else { "folder" },
                "capability": if i % 3 == 0 { "append" } else { "write" },
                "granted_at": now,
                "expires_at": future,
            })
        })
        .collect();
    fs::write(
        home.path().join("approvals.json"),
        serde_json::to_vec(&json!({ "version": 1, "grants": grants }))?,
    )?;

    measure("approval_grants_list_40", || {
        let mut total = 0usize;
        for _ in 0..25 {
            total += approval_grants::list().len();
        }
        black_box(total);
        Ok(())
    })?;

    // 2. Credential authentication over 8 issued credentials.
    let cred_path = home.path().join("credentials.json");
    let store = CredentialStore::at(cred_path);
    let mut tokens = Vec::with_capacity(8);
    for i in 0..8 {
        tokens.push(store.issue(Some(&format!("client-{i}")))?.token);
    }
    measure("cred_authenticate_200", || {
        let mut authed = 0usize;
        for i in 0..200 {
            let tok = if i % 5 == 0 {
                "deadbeef"
            } else {
                tokens[i % tokens.len()].as_str()
            };
            if store.authenticate(tok)?.is_some() {
                authed += 1;
            }
        }
        black_box(authed);
        Ok(())
    })?;

    env::remove_var("HORON_HOME");
    Ok(())
}
