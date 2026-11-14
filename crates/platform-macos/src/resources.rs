//! Host resource facts through the macOS system interfaces.
//!
//! Every call here is the platform's own measurement: `sysctlbyname` and
//! `host_statistics64` for memory, the `vm.swapusage` counter for swap,
//! `getloadavg` for load, `pmset -g batt` for the battery, and `statvfs` for
//! free space. `libc` already exposes the syscalls, so no dependency is added.
//!
//! macOS has no single "available memory" figure. The one used here matches
//! what the system itself treats as reclaimable for a new allocation: free,
//! inactive and speculative pages, in `resources::macos_memory`.

use std::ffi::CString;
use std::path::Path;
use std::process::Command;

use horon_agent_platform_api::{PlatformResources, resources};

/// This host's resource facts.
///
/// `unavailable` stays empty: every fact the report asks for has a macOS source,
/// and a `None` here means the read itself failed.
pub(crate) fn host_resources(disk_path: Option<&Path>) -> PlatformResources {
    let (memory_total_mb, memory_available_mb) = memory();
    let (swap_total_mb, swap_free_mb) = swap();
    let (battery_percent, battery_status) = battery();
    PlatformResources {
        memory_total_mb,
        memory_available_mb,
        swap_total_mb,
        swap_free_mb,
        load_one: load_one(),
        cpu_count: cpu_count(),
        battery_percent,
        battery_status,
        disk_available_gb: disk_path.and_then(disk_available_gb),
        unavailable: Vec::new(),
    }
}

// `mach_host_self` is a stable libSystem entry point that `libc` labels with a
// removal notice pointing at the `mach2` crate. Declaring the symbol here
// avoids adding a dependency for one call and keeps the build warning-free.
unsafe extern "C" {
    fn mach_host_self() -> libc::mach_port_t;
}

/// One `sysctl` value read as a byte string.
fn sysctl_bytes(name: &std::ffi::CStr) -> Option<Vec<u8>> {
    let mut size: libc::size_t = 0;
    // SAFETY: a null buffer with a zero size asks the kernel for the required
    // size, which is the documented way to size the second call.
    let rc = unsafe { libc::sysctlbyname(name.as_ptr(), std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) };
    if rc != 0 || size == 0 {
        return None;
    }
    let mut buffer = vec![0u8; size];
    // SAFETY: `buffer` holds exactly `size` bytes, which is the length the
    // kernel reported, and the pointer is valid for that length.
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            buffer.as_mut_ptr().cast::<libc::c_void>(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buffer.truncate(size);
    Some(buffer)
}

/// Installed memory and memory available for a new allocation.
fn memory() -> (Option<u64>, Option<u64>) {
    let total_bytes = sysctl_bytes(c"hw.memsize").and_then(|bytes| {
        let eight: [u8; 8] = match bytes.get(..8)?.try_into() {
            Ok(eight) => eight,
            Err(_) => return None,
        };
        Some(u64::from_ne_bytes(eight))
    });
    let Some(total_bytes) = total_bytes else {
        return (None, None);
    };
    let page_size = page_size().unwrap_or(0);
    let Some((free_pages, inactive_pages, speculative_pages)) = vm_page_counts() else {
        return (Some(total_bytes / (1024 * 1024)), None);
    };
    resources::macos_memory(total_bytes, page_size, free_pages, inactive_pages, speculative_pages)
}

/// The system page size, which `vm_statistics64` counts in.
fn page_size() -> Option<u64> {
    // SAFETY: `_SC_PAGESIZE` takes no pointer and returns the size or -1.
    let value = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    (value > 0).then_some(value as u64)
}

/// Free, inactive and speculative page counts from `HOST_VM_INFO64`.
fn vm_page_counts() -> Option<(u64, u64, u64)> {
    let mut stats = std::mem::MaybeUninit::<libc::vm_statistics64>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: `stats` is a correctly sized, properly aligned, zeroed
    // `vm_statistics64`; `count` is the record count the flavour requires, which
    // the kernel updates to what it wrote.
    let rc = unsafe {
        libc::host_statistics64(
            mach_host_self(),
            libc::HOST_VM_INFO64,
            stats.as_mut_ptr().cast::<libc::integer_t>(),
            &mut count,
        )
    };
    if rc != libc::KERN_SUCCESS {
        return None;
    }
    // SAFETY: a successful call filled the struct.
    let stats = unsafe { stats.assume_init() };
    Some((
        u64::from(stats.free_count),
        u64::from(stats.inactive_count),
        u64::from(stats.speculative_count),
    ))
}

/// Swap size and free swap from the `vm.swapusage` counter.
fn swap() -> (Option<u64>, Option<u64>) {
    let Some(bytes) = sysctl_bytes(c"vm.swapusage") else {
        return (None, None);
    };
    let size = std::mem::size_of::<libc::xsw_usage>();
    if bytes.len() < size {
        return (None, None);
    }
    // SAFETY: the kernel returned at least a whole `xsw_usage`, and the type is
    // plain old data with no padding requirements beyond alignment 8.
    let usage = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<libc::xsw_usage>()) };
    resources::macos_swap(usage.xsu_total, usage.xsu_avail)
}

/// The one-minute load average.
fn load_one() -> Option<f64> {
    let mut loadavg = [0.0_f64; 3];
    // SAFETY: `getloadavg` writes at most `nelem` doubles into the array.
    let written = unsafe { libc::getloadavg(loadavg.as_mut_ptr(), 1) };
    (written >= 1).then_some(loadavg[0])
}

/// Logical processors the scheduler runs on.
fn cpu_count() -> Option<usize> {
    resources::cpu_count()
}

/// Battery charge and state from `pmset -g batt`.
///
/// A desktop Mac prints only `Now drawing from 'AC Power'`, which the parser
/// reads as an absent battery rather than a failure.
fn battery() -> (Option<u8>, Option<String>) {
    let Ok(output) = Command::new("/usr/bin/pmset").arg("-g").arg("batt").output() else {
        return (None, None);
    };
    if !output.status.success() {
        return (None, None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    match stdout.lines().find(|line| line.contains("InternalBattery")) {
        Some(line) => resources::parse_pmset_battery(line),
        None => (None, None),
    }
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
    fn snapshot_reports_real_memory_and_load() {
        let facts = host_resources(Some(Path::new("/")));
        assert!(facts.unavailable.is_empty(), "macOS measures every fact");
        let total = facts.memory_total_mb.expect("total memory on macOS");
        assert!(total > 0, "total memory must be a real measurement, got {total}");
        let available = facts.memory_available_mb.expect("available memory on macOS");
        assert!(
            available < total,
            "{available} MiB available cannot exceed {total} MiB installed"
        );
        assert!(facts.load_one.is_some_and(|load| load >= 0.0));
        assert!(facts.cpu_count.is_some_and(|cpus| cpus >= 1));
    }

    #[test]
    fn snapshot_reports_battery_and_disk_on_this_machine() {
        let facts = host_resources(Some(Path::new("/")));
        // The owner's machines are laptops; a desktop Mac reports no battery at
        // all, which is absence rather than a failed read, so only the charge
        // range is asserted when one is present.
        if let Some(percent) = facts.battery_percent {
            assert!(percent <= 100, "battery percent out of range: {percent}");
            assert!(facts.battery_status.is_some(), "a charge must come with a state");
        }
        let disk = facts.disk_available_gb.expect("free space on /");
        assert!(disk > 0.0, "free space must be a real measurement, got {disk}");
    }

    #[test]
    fn disk_fact_is_skipped_when_no_path_is_asked_for() {
        let facts = host_resources(None);
        assert_eq!(facts.disk_available_gb, None);
    }

    #[test]
    fn swap_figures_are_internally_consistent() {
        let facts = host_resources(None);
        if let (Some(total), Some(free)) = (facts.swap_total_mb, facts.swap_free_mb) {
            assert!(free <= total, "{free} MiB free of {total} MiB swap");
        }
    }
}
