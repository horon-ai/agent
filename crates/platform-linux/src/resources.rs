//! Host resource facts through the `/proc` and `/sys` interfaces.
//!
//! Linux answers every fact from a file, and each reader here is the one
//! `overnight.rs` used to carry: `/proc/meminfo` for memory and swap,
//! `/proc/loadavg` for load, `/sys/class/power_supply` for the battery and
//! `statvfs` for free space. The text parsing lives in
//! `horon_agent_platform_api::resources` so it can be tested on any host.

use std::ffi::CString;
use std::path::Path;

use horon_agent_platform_api::{PlatformResources, resources};

/// This host's resource facts.
///
/// `unavailable` stays empty, and a `None` means the read failed.
pub(crate) fn host_resources(disk_path: Option<&Path>) -> PlatformResources {
    let (memory_total_mb, memory_available_mb, swap_total_mb, swap_free_mb) = memory();
    let (battery_percent, battery_status) = battery();
    PlatformResources {
        memory_total_mb,
        memory_available_mb,
        swap_total_mb,
        swap_free_mb,
        load_one: load_one(),
        cpu_count: resources::cpu_count(),
        battery_percent,
        battery_status,
        disk_available_gb: disk_path.and_then(disk_available_gb),
        unavailable: Vec::new(),
    }
}

/// Memory and swap figures from `/proc/meminfo`.
fn memory() -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
    let Ok(contents) = std::fs::read_to_string("/proc/meminfo") else {
        return (None, None, None, None);
    };
    resources::parse_meminfo(&contents)
}

/// The one-minute load average from `/proc/loadavg`.
fn load_one() -> Option<f64> {
    let Ok(contents) = std::fs::read_to_string("/proc/loadavg") else {
        return None;
    };
    resources::parse_loadavg(&contents)
}

/// The first battery's charge and state from `/sys/class/power_supply`.
fn battery() -> (Option<u8>, Option<String>) {
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
        return (None, None);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_name().to_string_lossy().starts_with("BAT") {
            continue;
        }
        // A battery whose charge cannot be read is not one this can report; a
        // missing state file costs only the wording, not the percentage.
        let Ok(capacity) = std::fs::read_to_string(path.join("capacity")) else {
            continue;
        };
        // An unreadable state file costs the wording only: the charge still
        // stands, and the parser reads the empty string as "not reported".
        let status = std::fs::read_to_string(path.join("status"));
        let status = status.as_deref().unwrap_or("");
        return resources::parse_power_supply(&capacity, status);
    }
    (None, None)
}

/// Space available on the filesystem holding `path`, in gibibytes.
fn disk_available_gb(path: &Path) -> Option<f64> {
    use std::os::unix::ffi::OsStrExt;
    // A path holding an interior NUL cannot name a file, so `None` is right.
    let Ok(c_path) = CString::new(path.as_os_str().as_bytes()) else {
        return None;
    };
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `stat` is a zeroed `statvfs` and `c_path` is a valid C string.
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return None;
    }
    let bytes = stat.f_bavail as u64 * stat.f_frsize as u64;
    Some(resources::bytes_to_gib(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reports_the_same_facts_the_overnight_probe_did() {
        // The behaviour this preserves: `/proc` answers memory, load and CPU
        // count on every Linux host, and the values stay inside the ranges the
        // report can render.
        let facts = host_resources(Some(Path::new("/")));
        assert!(facts.unavailable.is_empty());
        let total = facts.memory_total_mb.expect("/proc/meminfo total");
        let available = facts.memory_available_mb.expect("/proc/meminfo available");
        assert!(total > 0);
        assert!(available <= total, "{available} MiB available of {total} MiB installed");
        assert!(facts.load_one.is_some_and(|load| load >= 0.0));
        assert!(facts.cpu_count.is_some_and(|cpus| cpus >= 1));
        assert!(facts.disk_available_gb.is_some_and(|gb| gb > 0.0));
    }

    #[test]
    fn swap_figures_are_internally_consistent() {
        let facts = host_resources(None);
        if let (Some(total), Some(free)) = (facts.swap_total_mb, facts.swap_free_mb) {
            assert!(free <= total, "{free} MiB free of {total} MiB swap");
        }
    }
}
