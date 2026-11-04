//! Print the stage counters beside the process facts they are read with.
//!
//! The daemon exposes the same values through `horon-agent debug 'server:memory'`
//! (`process.stage_counters`, `process.fd_count`, `process.thread_count`). This
//! example exists so the surface can be read without a running daemon - from a
//! CI job, a soak harness, or an A/B workload.
//!
//! ```text
//! cargo run -p horon-agent-base --example perf_counters_surface
//! ```
//!
//! Exit status is 0 when counters are enabled and 3 when
//! `HORON_AGENT_PERF_COUNTERS` turned them off, so a caller that needs the
//! numbers can tell "disabled" from "measured zero".
//!
//! `perf_counters_surface version --json` prints the build metadata instead, so
//! this binary can serve as an A/B workload: the runner reads each side's
//! profile and feature set before it times anything.

fn version_payload() -> serde_json::Value {
    let flags = [
        ("embeddings", cfg!(feature = "embeddings")),
        ("jemalloc", cfg!(feature = "jemalloc")),
        ("jemalloc-prof", cfg!(feature = "jemalloc-prof")),
        ("test-support", cfg!(feature = "test-support")),
    ];
    let build_features: Vec<&str> = flags
        .into_iter()
        .filter_map(|(name, enabled)| enabled.then_some(name))
        .collect();
    serde_json::json!({
        "version": horon_agent_build_meta::version(),
        "semver": horon_agent_build_meta::semver(),
        "git_hash": horon_agent_build_meta::git_hash(),
        "git_tag": horon_agent_build_meta::git_tag(),
        "build_profile": horon_agent_build_meta::build_profile(),
        "build_features": build_features,
        "release_build": horon_agent_build_meta::is_release_build(),
    })
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("version") {
        match serde_json::to_string_pretty(&version_payload()) {
            Ok(text) => println!("{text}"),
            Err(error) => {
                eprintln!("failed to serialize build metadata: {error}");
                std::process::exit(1);
            }
        }
        return;
    }

    let counters = horon_agent_perf_counters::snapshot();
    let process = horon_agent_base::process_memory::snapshot_with_source("perf_counters_surface");

    let payload = serde_json::json!({
        "counters": counters,
        "process": {
            "rss_bytes": process.rss_bytes,
            "peak_rss_bytes": process.peak_rss_bytes,
            "thread_count": process.thread_count,
            "fd_count": process.fd_count,
            "allocator": process.allocator.name,
            "allocator_live_bytes": process.allocator.stats.as_ref().and_then(|stats| stats.allocated_bytes),
            "allocator_retained_bytes": process.allocator.stats.as_ref().and_then(|stats| stats.retained_bytes),
        },
        "resource_high_water": counters.resource_high_water(),
    });

    match serde_json::to_string_pretty(&payload) {
        Ok(text) => println!("{text}"),
        Err(error) => {
            eprintln!("failed to serialize the counter surface: {error}");
            std::process::exit(1);
        }
    }

    if !counters.enabled {
        eprintln!(
            "counters are disabled by {}; the zeros above are not measurements",
            horon_agent_perf_counters::ENV_ENABLE
        );
        std::process::exit(3);
    }
}
