#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// Tune jemalloc for a long-running server with bursty allocations (e.g. loading
// and unloading an ~87 MB ONNX embedding model). The defaults (muzzy_decay_ms:0,
// retain:true, narenas:8*ncpu) caused 1.4 GB RSS in previous testing.
//
// dirty_decay_ms:1000  — return dirty pages to OS after 1 s idle
// muzzy_decay_ms:1000  — release muzzy pages after 1 s
// narenas:4            — limit arena count (17 threads don't need 64 arenas)
// prof:true            — enable profiling support in jemalloc-prof builds
// prof_active:false    — keep sampling disabled until explicitly enabled at runtime
#[cfg(all(feature = "jemalloc", not(feature = "jemalloc-prof")))]
// jemalloc reads this exact exported symbol name at startup.
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static malloc_conf: Option<&'static [u8; 50]> = Some(b"dirty_decay_ms:1000,muzzy_decay_ms:1000,narenas:4\0");

#[cfg(feature = "jemalloc-prof")]
// jemalloc reads this exact exported symbol name at startup.
#[allow(non_upper_case_globals)]
#[unsafe(no_mangle)]
pub static malloc_conf: Option<&'static [u8; 78]> =
    Some(b"dirty_decay_ms:1000,muzzy_decay_ms:1000,narenas:4,prof:true,prof_active:false\0");

use anyhow::Result;

fn main() -> Result<()> {
    // Windows starts the main thread with a much smaller stack than the Unix
    // environments this project is developed on, and the CLI's provider and
    // config work can overflow it before Tokio takes over. Every other platform
    // reports no requirement and runs on the main thread directly, which keeps
    // the linker defaults of the auxiliary binaries unchanged.
    let Some(stack_size) = horon_agent_platform::process::main_thread_stack_size() else {
        return run_main();
    };
    const MAIN_THREAD_NAME: &str = "horon-agent-main";
    match std::thread::Builder::new()
        .name(MAIN_THREAD_NAME.to_string())
        .stack_size(stack_size)
        .spawn(run_main)?
        .join()
    {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

fn run_main() -> Result<()> {
    // glibc keeps freed large blocks in its arenas unless it is told otherwise;
    // the platform tunes that where the switch exists and does nothing when a
    // different global allocator is linked.
    horon_agent_platform::process::configure_system_allocator(!cfg!(feature = "jemalloc"));
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;

    runtime.block_on(async { horon_agent::run().await })
}
