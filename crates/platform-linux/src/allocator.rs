//! glibc allocator tuning for the long-running Linux server.
//!
//! The defaults glibc picks are wrong for a process that loads an embedding
//! model once and then holds it: freed large blocks stay in the arenas and the
//! brk heap keeps growing, so RSS ends up far above the live set. These knobs
//! trade a little throughput for a predictable resident size.

/// Apply the allocator tuning this build can use.
///
/// `use_system_allocator` is false when the caller linked a different global
/// allocator, where glibc's own tuning has nothing to act on. Only a glibc Linux
/// target has `mallopt`; every other target compiled from this crate has no
/// allocator knob to turn.
pub(crate) fn configure(use_system_allocator: bool) {
    tune_system_allocator(use_system_allocator);
}

/// Apply the tuning on the targets that have an allocator knob.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn tune_system_allocator(use_system_allocator: bool) {
    if use_system_allocator {
        tune_glibc();
    }
}

/// Apply the tuning on the targets that have none.
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn tune_system_allocator(_use_system_allocator: bool) {}

/// Set glibc's arena count and mmap threshold from the environment.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn tune_glibc() {
    use horon_agent_platform_api::log::{PlatformLogLevel as Level, log as report};

    // Safety: `mallopt` reads the scalar arguments and changes allocator
    // parameters; it dereferences no pointer.
    unsafe extern "C" {
        fn mallopt(param: i32, value: i32) -> i32;
    }

    const M_ARENA_MAX: i32 = -8;
    const M_MMAP_THRESHOLD: i32 = -3;

    // 17 threads do not need one arena per core.
    let arena_max = parse_alloc_tuning_env("HORON_AGENT_GLIBC_ARENA_MAX", 4);
    // An allocator that refuses the value keeps its default; report it rather
    // than failing startup over a tuning hint.
    if unsafe { mallopt(M_ARENA_MAX, arena_max) } == 0 {
        report(
            Level::Warn,
            "glibc refused M_ARENA_MAX; keeping the default arena count",
        );
    }

    // Pin the mmap threshold so large transient allocations (history JSON,
    // provider payloads) are served by mmap and returned to the OS immediately
    // on free, instead of landing in sbrk arenas where freed blocks below the
    // top chunk become permanent RSS retention.
    //
    // Tradeoff: setting M_MMAP_THRESHOLD via mallopt disables glibc's dynamic
    // threshold growth (normally the threshold rises toward 32 MiB as large
    // blocks are freed, keeping hot large buffers in the arena for cheap
    // reuse). Pinning trades some throughput on repeated large alloc/free
    // cycles (mmap/munmap syscalls and page faults each time) for a
    // predictable, immediate memory return. For a long-running interactive
    // agent, lower steady-state RSS wins.
    let mmap_threshold = parse_alloc_tuning_env("HORON_AGENT_GLIBC_MMAP_THRESHOLD", 256 * 1024);
    if unsafe { mallopt(M_MMAP_THRESHOLD, mmap_threshold) } == 0 {
        report(
            Level::Warn,
            "glibc refused M_MMAP_THRESHOLD; keeping the default mmap threshold",
        );
    }
}

/// Parse a positive `i32` allocator knob from an environment variable.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn parse_alloc_tuning_env(var: &str, default: i32) -> i32 {
    match std::env::var(var) {
        Ok(value) => parse_alloc_tuning(Some(&value), default),
        Err(_) => default,
    }
}

/// Pure parsing core of [`parse_alloc_tuning_env`], separated for unit tests.
///
/// Compiled for every test build so the parsing rules stay covered on the hosts
/// that do not tune a glibc allocator at all.
#[cfg(any(test, all(target_os = "linux", target_env = "gnu")))]
fn parse_alloc_tuning(value: Option<&str>, default: i32) -> i32 {
    let Some(text) = value else {
        return default;
    };
    match text.trim().parse::<i32>() {
        Ok(number) if number > 0 => number,
        _ => default,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_alloc_tuning;

    #[test]
    fn alloc_tuning_uses_default_when_unset() {
        assert_eq!(parse_alloc_tuning(None, 262_144), 262_144);
    }

    #[test]
    fn alloc_tuning_parses_positive_value_with_whitespace() {
        assert_eq!(parse_alloc_tuning(Some(" 131072 "), 262_144), 131_072);
    }

    #[test]
    fn alloc_tuning_rejects_garbage_zero_and_negative() {
        assert_eq!(parse_alloc_tuning(Some("not-a-number"), 4), 4);
        assert_eq!(parse_alloc_tuning(Some("0"), 4), 4);
        assert_eq!(parse_alloc_tuning(Some("-1"), 4), 4);
        // i32 overflow falls back to default rather than wrapping.
        assert_eq!(parse_alloc_tuning(Some("4294967296"), 4), 4);
    }
}
