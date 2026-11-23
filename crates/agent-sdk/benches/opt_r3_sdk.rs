//! Micro-benchmarks for round-3 SDK and platform optimization paths in `horon-agent-sdk`.
//!
//! Exercises `HoronAgentClient::run_package_command` over a real socket pair and
//! `horon_agent_platform::process::package_child` under a release build with a
//! counting allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::error::Error;
use std::ffi::OsString;
use std::hint::black_box;
use std::io::{BufRead, BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use horon_agent_harness_api::{
    API_VERSION_MAJOR, ApiEvent, ApiRequest, CAPABILITY_PACKAGE_COMMANDS_V1, ClientFrame, PackageCommandKind,
    PackageCommandOutcome, RuntimeCommand, ServerFrame, read_frame, write_frame,
};
use horon_agent_platform::process::package_child::{PackageChild, PackageChildSpec, spawn};
use horon_agent_sdk::{ConnectOptions, ErrorKind, HoronAgentClient, Transport};

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

fn measure_with_setup<S, Init, Work>(name: &str, mut init: Init, mut work: Work) -> Result<(), Box<dyn Error>>
where
    Init: FnMut() -> Result<S, Box<dyn Error>>,
    Work: FnMut(&mut S) -> Result<(), Box<dyn Error>>,
{
    {
        let mut warmup_state = init()?;
        work(&mut warmup_state)?;
    }

    let mut times_us = Vec::with_capacity(RUNS);
    let mut last_allocs = 0u64;
    let mut last_bytes = 0u64;
    for _ in 0..RUNS {
        let mut state = init()?;
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
        COUNTING.with(|c| c.set(true));
        let start = Instant::now();
        let res = work(&mut state);
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

fn measure<Work>(name: &str, mut work: Work) -> Result<(), Box<dyn Error>>
where
    Work: FnMut() -> Result<(), Box<dyn Error>>,
{
    measure_with_setup(name, || Ok(()), |()| work())
}

struct PairTransport(UnixStream);

impl Transport for PairTransport {
    fn shutdown_handle(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        match self.0.try_clone() {
            Ok(cloned) => Some(Arc::new(move || {
                if cloned.shutdown(Shutdown::Both).is_err() {
                    // Best-effort wakeup during client teardown.
                }
            })),
            Err(_) => None,
        }
    }

    fn split(self: Box<Self>) -> horon_agent_sdk::Result<(Box<dyn BufRead + Send>, Box<dyn Write + Send>)> {
        let writer = self.0.try_clone().map_err(|error| {
            horon_agent_sdk::Error::new(ErrorKind::Transport, format!("clone benchmark socket pair: {error}"))
        })?;
        Ok((Box::new(BufReader::new(self.0)), Box::new(writer)))
    }
}

fn start_fake_package_harness() -> Result<(HoronAgentClient, std::thread::JoinHandle<()>), Box<dyn Error>> {
    let (ours, theirs) = UnixStream::pair()?;
    let reader_stream = theirs.try_clone()?;
    let handle = std::thread::spawn(move || {
        let mut reader = BufReader::new(reader_stream);
        let mut writer = theirs;
        let outcome = PackageCommandOutcome {
            name: "hello:greet".to_string(),
            package_id: "hello".to_string(),
            command: "greet".to_string(),
            kind: PackageCommandKind::Executable,
            text: None,
            output: Some("ok".to_string()),
            data: None,
        };
        let Ok(outcome_value) = serde_json::to_value(&outcome) else {
            return;
        };
        while let Ok(frame) = read_frame::<_, ClientFrame>(&mut reader) {
            let reply = match frame.request {
                ApiRequest::Hello { .. } => ServerFrame {
                    v: API_VERSION_MAJOR,
                    reply_to: Some(frame.id),
                    event: ApiEvent::HelloOk {
                        version: API_VERSION_MAJOR,
                        server: "bench-harness/1.0".to_string(),
                        capabilities: vec![
                            "runtime_commands".to_string(),
                            CAPABILITY_PACKAGE_COMMANDS_V1.to_string(),
                        ],
                    },
                },
                ApiRequest::RuntimeCommand {
                    session_id,
                    command: RuntimeCommand::PackageCommand,
                    ..
                } => ServerFrame {
                    v: API_VERSION_MAJOR,
                    reply_to: Some(frame.id),
                    event: ApiEvent::RuntimeCommandResult {
                        session_id,
                        command: RuntimeCommand::PackageCommand,
                        data: outcome_value.clone(),
                        message: String::new(),
                    },
                },
                _ => ServerFrame {
                    v: API_VERSION_MAJOR,
                    reply_to: Some(frame.id),
                    event: ApiEvent::Ok,
                },
            };
            if write_frame(&mut writer, &reply).is_err() {
                break;
            }
        }
    });

    let client = HoronAgentClient::connect_with(
        Box::new(PairTransport(ours)),
        ConnectOptions {
            request_timeout: Some(Duration::from_secs(5)),
            ensure_runtime: false,
            ..Default::default()
        },
    )?;
    Ok((client, handle))
}

fn spawn_term_ignoring_children(count: usize, root: &std::path::Path) -> Result<Vec<PackageChild>, Box<dyn Error>> {
    let sh_path = std::path::Path::new("/bin/sh");
    let args = [
        OsString::from("-c"),
        OsString::from("trap '' TERM; printf 'R\\n'; read _"),
    ];
    let spec = PackageChildSpec {
        program: sh_path,
        arguments: &args,
        root,
        id: "bench-pkg",
        kind: "extension",
        state_dir: root,
        grace: Duration::ZERO,
    };
    let mut children = Vec::with_capacity(count);
    for _ in 0..count {
        let mut child = spawn(&spec)?;
        let Some(stdout) = child.take_stdout() else {
            return Err("missing child stdout".into());
        };
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        if line != "R\n" {
            return Err(format!("unexpected child readiness line: {line:?}").into());
        }
        children.push(child);
    }
    Ok(children)
}

fn main() -> Result<(), Box<dyn Error>> {
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(60));
        eprintln!("opt_r3_sdk benchmark watchdog timeout (60s)");
        std::process::exit(1);
    });

    let (client, server_handle) = start_fake_package_harness()?;

    measure("sdk_run_pkg_cmd_empty_200", || {
        let mut total = 0usize;
        for _ in 0..200 {
            let outcome = client.run_package_command("s1", "hello", "greet", "")?;
            total = total.wrapping_add(outcome.name.len());
        }
        black_box(total);
        Ok(())
    })?;

    measure("sdk_run_pkg_cmd_short_200", || {
        let mut total = 0usize;
        for _ in 0..200 {
            let outcome =
                client.run_package_command("s1", "hello", "greet", "  --format json --limit 25 target one  ")?;
            total = total.wrapping_add(outcome.name.len());
        }
        black_box(total);
        Ok(())
    })?;

    let long_args = (0..250).map(|i| format!("arg_{i:03}")).collect::<Vec<_>>().join("   ");
    measure("sdk_run_pkg_cmd_long_40", || {
        let mut total = 0usize;
        for _ in 0..40 {
            let outcome = client.run_package_command("s1", "hello", "greet", &long_args)?;
            total = total.wrapping_add(outcome.name.len());
        }
        black_box(total);
        Ok(())
    })?;

    drop(client);
    if server_handle.join().is_err() {
        return Err("fake harness thread panicked".into());
    }

    let scratch = tempfile::tempdir()?;
    let sh_path = std::path::Path::new("/bin/sh");
    let true_args = [OsString::from("-c"), OsString::from("exit 0")];
    let spawn_spec = PackageChildSpec {
        program: sh_path,
        arguments: &true_args,
        root: scratch.path(),
        id: "bench-pkg",
        kind: "extension",
        state_dir: scratch.path(),
        grace: Duration::from_millis(50),
    };
    measure("pkg_child_spawn_wait_10", || {
        let mut code_sum = 0i32;
        for _ in 0..10 {
            let mut child = spawn(&spawn_spec)?;
            let status = child.wait()?;
            code_sum = code_sum.wrapping_add(status.code().unwrap_or(-1));
        }
        black_box(code_sum);
        Ok(())
    })?;

    measure_with_setup(
        "pkg_child_terminate_kill_10",
        || spawn_term_ignoring_children(10, scratch.path()),
        |children| {
            let mut count = 0usize;
            for child in children.iter_mut() {
                let status = child.terminate()?;
                black_box(status);
                count += 1;
            }
            black_box(count);
            Ok(())
        },
    )?;

    Ok(())
}
