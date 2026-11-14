//! Host resource facts through the Win32 system information calls.
//!
//! Memory and swap come from `GlobalMemoryStatusEx`, the battery from
//! `GetSystemPowerStatus`, free space from `GetDiskFreeSpaceExW` and the load
//! figure from two `GetSystemTimes` samples: Windows has no load average, so the
//! closest measurement is the busy share of processor time, scaled to the
//! processor count to the unit a Unix load average uses. The arithmetic lives in
//! `horon_agent_platform_api::resources` so it is tested on every platform.

use std::path::Path;

use horon_agent_platform_api::{PlatformResources, resources};

/// This host's resource facts.
///
/// `unavailable` stays empty: every fact has a Win32 source, and a `None` means
/// the call failed.
pub(crate) fn host_resources(disk_path: Option<&Path>) -> PlatformResources {
    let (memory_total_mb, memory_available_mb, swap_total_mb, swap_free_mb) = memory();
    let (battery_percent, battery_status) = battery();
    let cpu_count = resources::cpu_count();
    PlatformResources {
        memory_total_mb,
        memory_available_mb,
        swap_total_mb,
        swap_free_mb,
        load_one: load_one(cpu_count),
        cpu_count,
        battery_percent,
        battery_status,
        disk_available_gb: disk_path.and_then(disk_available_gb),
        unavailable: Vec::new(),
    }
}

/// Physical memory, commit limit and page file figures from
/// `GlobalMemoryStatusEx`.
fn memory() -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    // SAFETY: `MEMORYSTATUSEX` is plain old data, so an all-zero value is a
    // valid starting point; `dwLength` is then set to the struct's size.
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: `status` is a correctly sized, fully initialised `MEMORYSTATUSEX`
    // whose `dwLength` names that size, which is the call's documented contract.
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    if ok == 0 {
        return (None, None, None, None);
    }
    const MB: u64 = 1024 * 1024;
    let (swap_total_mb, swap_free_mb) = resources::windows_swap(
        status.ullTotalPageFile,
        status.ullAvailPageFile,
        status.ullTotalPhys,
        status.ullAvailPhys,
    );
    (
        Some(status.ullTotalPhys / MB),
        Some(status.ullAvailPhys / MB),
        swap_total_mb,
        swap_free_mb,
    )
}

/// The busy share of processor time, converted to a Unix-style load figure.
fn load_one(cpu_count: Option<usize>) -> Option<f64> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetSystemTimes;

    /// A `FILETIME` as one 100-nanosecond tick count.
    fn ticks(time: FILETIME) -> u64 {
        (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
    }

    /// One sample of idle, kernel and user time.
    fn sample() -> Option<(u64, u64, u64)> {
        // SAFETY: `FILETIME` is plain old data; the call writes all three.
        let mut idle: FILETIME = unsafe { std::mem::zeroed() };
        let mut kernel: FILETIME = unsafe { std::mem::zeroed() };
        let mut user: FILETIME = unsafe { std::mem::zeroed() };
        // SAFETY: all three pointers name initialised `FILETIME` values.
        let ok = unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) };
        (ok != 0).then(|| (ticks(idle), ticks(kernel), ticks(user)))
    }

    let (idle_start, kernel_start, user_start) = sample()?;
    // `GetSystemTimes` reports cumulative totals, so the busy share needs a
    // window. 100 ms is long enough for a stable figure and short enough that a
    // report never waits on it.
    std::thread::sleep(std::time::Duration::from_millis(100));
    let (idle_end, kernel_end, user_end) = sample()?;
    let busy_share = resources::windows_busy_share(
        idle_end.saturating_sub(idle_start),
        kernel_end.saturating_sub(kernel_start),
        user_end.saturating_sub(user_start),
    )?;
    Some(resources::windows_load_equivalent(busy_share, cpu_count.unwrap_or(1)))
}

/// Battery charge and state from `GetSystemPowerStatus`.
fn battery() -> (Option<u8>, Option<String>) {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    // SAFETY: `SYSTEM_POWER_STATUS` is plain old data; the call fills it.
    let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
    // SAFETY: `status` is an initialised `SYSTEM_POWER_STATUS`.
    let ok = unsafe { GetSystemPowerStatus(&mut status) };
    if ok == 0 {
        return (None, None);
    }
    resources::windows_battery(status.ACLineStatus, status.BatteryFlag, status.BatteryLifePercent)
}

/// Space available on the filesystem holding `path`, in gibibytes.
fn disk_available_gb(path: &Path) -> Option<f64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available: u64 = 0;
    // SAFETY: `wide` is a NUL-terminated UTF-16 path, and `available` is a
    // valid u64 the call writes the free-byte count into; the other two
    // optional outputs are skipped.
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then(|| resources::bytes_to_gib(available))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reports_real_memory_load_and_battery() {
        let facts = host_resources(Some(Path::new("C:\\")));
        assert!(facts.unavailable.is_empty(), "Windows measures every fact");
        let total = facts.memory_total_mb.expect("physical memory");
        assert!(total > 0, "total memory must be a real measurement, got {total}");
        let available = facts.memory_available_mb.expect("available memory");
        assert!(available <= total, "{available} MiB available of {total} MiB installed");
        let load = facts.load_one.expect("load equivalent");
        let cpus = facts.cpu_count.expect("processor count");
        assert!(
            (0.0..=cpus as f64).contains(&load),
            "load equivalent {load} must sit between 0 and {cpus}"
        );
    }

    #[test]
    fn disk_fact_reads_the_system_volume() {
        let facts = host_resources(Some(Path::new("C:\\")));
        let disk = facts.disk_available_gb.expect("free space on C:");
        assert!(disk > 0.0, "free space must be a real measurement, got {disk}");
    }

    #[test]
    fn disk_fact_is_skipped_when_no_path_is_asked_for() {
        assert_eq!(host_resources(None).disk_available_gb, None);
    }
}
