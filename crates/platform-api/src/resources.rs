//! Host resource facts and the parsers that turn raw operating-system output
//! into them.
//!
//! The readers differ per platform (a `/proc` file, a `sysctl` key, a Win32
//! call), but the shapes they produce are one type and every text format is
//! parsed here, where it can be tested with a recorded sample on any operating
//! system. A fact an operating system cannot measure at all is named in
//! [`PlatformResources::unavailable`], so a report can say "unavailable on this
//! platform" instead of leaving a null that reads like a broken probe.

/// Host resource facts as one operating system can report them.
///
/// Every field is `None` when the value is absent or could not be read. The
/// difference between "this platform cannot measure it" and "the read failed"
/// is carried by [`unavailable`](Self::unavailable): a fact named there is not
/// measurable here, and a fact absent from it but `None` is a failed read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlatformResources {
    /// Physical memory installed, in mebibytes.
    pub memory_total_mb: Option<u64>,
    /// Memory the operating system could hand to a new allocation, in
    /// mebibytes. Each platform's own notion of "available", not free memory.
    pub memory_available_mb: Option<u64>,
    /// Swap or page file size, in mebibytes.
    pub swap_total_mb: Option<u64>,
    /// Unused swap or page file, in mebibytes.
    pub swap_free_mb: Option<u64>,
    /// One-minute load average.
    pub load_one: Option<f64>,
    /// Logical processors the operating system schedules on.
    pub cpu_count: Option<usize>,
    /// Battery charge, as the percentage the platform reports.
    pub battery_percent: Option<u8>,
    /// Battery state in the platform's own wording (`Charging`, `Discharging`,
    /// `AC attached`, and so on).
    pub battery_status: Option<String>,
    /// Space available on the filesystem holding a path, in gibibytes.
    pub disk_available_gb: Option<f64>,
    /// Facts this operating system cannot measure at all, by field name.
    pub unavailable: Vec<&'static str>,
}

/// A number in the platform's own text, or `None` when the text is not one.
///
/// Every reader here reports an unreadable value as absent instead of
/// panicking: a resource probe that aborts is worse than one that says less.
fn parse_number<T: std::str::FromStr>(text: &str) -> Option<T> {
    let Ok(value) = text.trim().parse::<T>() else {
        return None;
    };
    Some(value)
}

/// Logical processors the operating system schedules on.
///
/// `available_parallelism` reports the CPU quota a container is limited to
/// where the platform enforces one, which is the count work can actually use.
pub fn cpu_count() -> Option<usize> {
    match std::thread::available_parallelism() {
        Ok(value) => Some(value.get()),
        Err(_) => None,
    }
}

/// A `/proc/meminfo` figure: the value is in kibibytes.
fn meminfo_kb(rest: &str) -> Option<u64> {
    rest.split_whitespace().next().and_then(parse_number)
}

/// Memory and swap figures from a `/proc/meminfo` body, in mebibytes.
///
/// Returns `(total, available, swap_total, swap_free)`. Lines the platform does
/// not carry stay `None`.
pub fn parse_meminfo(contents: &str) -> (Option<u64>, Option<u64>, Option<u64>, Option<u64>) {
    let mut total_kb = None;
    let mut available_kb = None;
    let mut swap_total_kb = None;
    let mut swap_free_kb = None;
    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = meminfo_kb(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available_kb = meminfo_kb(rest);
        } else if let Some(rest) = line.strip_prefix("SwapTotal:") {
            swap_total_kb = meminfo_kb(rest);
        } else if let Some(rest) = line.strip_prefix("SwapFree:") {
            swap_free_kb = meminfo_kb(rest);
        }
    }
    (
        total_kb.map(|kb| kb / 1024),
        available_kb.map(|kb| kb / 1024),
        swap_total_kb.map(|kb| kb / 1024),
        swap_free_kb.map(|kb| kb / 1024),
    )
}

/// One-minute load average from a `/proc/loadavg` body.
pub fn parse_loadavg(contents: &str) -> Option<f64> {
    contents.split_whitespace().next().and_then(parse_number)
}

/// Charge and state from a `/sys/class/power_supply/BAT*/` pair.
///
/// `capacity` is the percentage file's contents and `status` the state file's,
/// both trimmed by the caller. A charge outside 0–100 is rejected rather than
/// clamped, because a bogus figure is worse than a missing one.
pub fn parse_power_supply(capacity: &str, status: &str) -> (Option<u8>, Option<String>) {
    let percent = parse_number::<u8>(capacity).filter(|percent| *percent <= 100);
    let status = status.trim();
    let status = if status.is_empty() {
        None
    } else {
        Some(status.to_string())
    };
    (percent, status)
}

/// macOS memory totals from `hw.memsize` and the `vm_statistics64` page counts.
///
/// Returns `(total_mb, available_mb)`. `available` follows the macOS
/// definition of memory the system can hand out without swapping: free,
/// inactive and speculative pages, which is what `vm_stat` labels as reclaimable
/// once the file cache behind the inactive pages is dropped.
pub fn macos_memory(
    total_bytes: u64,
    page_size: u64,
    free_pages: u64,
    inactive_pages: u64,
    speculative_pages: u64,
) -> (Option<u64>, Option<u64>) {
    const MB: u64 = 1024 * 1024;
    if total_bytes == 0 || page_size == 0 {
        return (None, None);
    }
    let total_mb = total_bytes / MB;
    let available_pages = free_pages
        .saturating_add(inactive_pages)
        .saturating_add(speculative_pages);
    let available_mb = available_pages.saturating_mul(page_size) / MB;
    // A reading larger than the installed memory is a torn read across two
    // syscalls, not a measurement, so it is dropped rather than reported.
    if available_mb > total_mb {
        (Some(total_mb), None)
    } else {
        (Some(total_mb), Some(available_mb))
    }
}

/// Swap totals from the macOS `vm.swapusage` counter, in mebibytes.
pub fn macos_swap(total_bytes: u64, avail_bytes: u64) -> (Option<u64>, Option<u64>) {
    const MB: u64 = 1024 * 1024;
    let total = total_bytes / MB;
    let free = avail_bytes / MB;
    if total == 0 {
        (None, None)
    } else {
        (Some(total), Some(free.min(total)))
    }
}

/// Charge and state from one `pmset -g batt` line.
///
/// The line reads like ` -InternalBattery-0 (id=25231459)\t80%; AC attached;
/// not charging present: true`. A machine with no battery prints
/// `Now drawing from 'AC Power'` and no battery line at all, which yields
/// `(None, None)`: an absent battery, not a failed read.
pub fn parse_pmset_battery(line: &str) -> (Option<u8>, Option<String>) {
    let Some((before, after)) = line.split_once('%') else {
        return (None, None);
    };
    // The digits sit immediately before the sign.
    let digits: String = before
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let percent = parse_number::<u8>(&digits).filter(|percent| *percent <= 100);
    // `80%; AC attached; not charging present: true` -> `AC attached; not charging`.
    let status = after
        .split_once(';')
        .map(|(_, rest)| rest)
        .unwrap_or(after)
        .split("present:")
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches(';')
        .trim();
    let status = if status.is_empty() {
        None
    } else {
        Some(status.to_string())
    };
    (percent, status)
}

/// Swap figures from `GlobalMemoryStatusEx`, in mebibytes.
///
/// Windows reports a commit limit that includes physical memory, so the page
/// file's own size is the part of the page-file total beyond RAM; the same
/// subtraction gives its free space. A machine with no page file yields
/// `(None, None)`.
pub fn windows_swap(
    total_page_file: u64,
    avail_page_file: u64,
    total_phys: u64,
    avail_phys: u64,
) -> (Option<u64>, Option<u64>) {
    const MB: u64 = 1024 * 1024;
    let total = total_page_file.saturating_sub(total_phys);
    if total == 0 {
        return (None, None);
    }
    let free = avail_page_file.saturating_sub(avail_phys).min(total);
    (Some(total / MB), Some(free / MB))
}

/// Charge and state from `SYSTEM_POWER_STATUS`.
///
/// `battery_life_percent` is 255 when the battery state is unknown and
/// `battery_flag` carries `1` for charging and `8` for no battery installed;
/// both come straight from the API's documented values.
pub fn windows_battery(ac_line_status: u8, battery_flag: u8, battery_life_percent: u8) -> (Option<u8>, Option<String>) {
    const BATTERY_FLAG_CHARGING: u8 = 1;
    const BATTERY_FLAG_NO_BATTERY: u8 = 8;
    const AC_LINE_ONLINE: u8 = 1;
    if battery_flag & BATTERY_FLAG_NO_BATTERY != 0 || battery_life_percent == 255 {
        return (None, None);
    }
    let percent = (battery_life_percent <= 100).then_some(battery_life_percent);
    let status = if battery_flag & BATTERY_FLAG_CHARGING != 0 {
        "Charging"
    } else if ac_line_status == AC_LINE_ONLINE {
        "AC attached"
    } else {
        "Discharging"
    };
    (percent, Some(status.to_string()))
}

/// The share of processor time that was not idle, from two `GetSystemTimes`
/// samples.
///
/// Windows has no load average, so the equivalent this platform can measure is
/// the busy share of processor time over a sampling window. `kernel` includes
/// idle time, which is why idle is subtracted from it rather than added.
pub fn windows_busy_share(idle: u64, kernel: u64, user: u64) -> Option<f64> {
    let total = kernel.saturating_add(user);
    if total == 0 {
        return None;
    }
    let idle = idle.min(total);
    Some((total - idle) as f64 / total as f64)
}

/// The load figure Windows reports: the busy share scaled to the processor
/// count, which is the same unit as a Unix load average (runnable work).
pub fn windows_load_equivalent(busy_share: f64, cpu_count: usize) -> f64 {
    busy_share.clamp(0.0, 1.0) * cpu_count as f64
}

/// Bytes to gibibytes for the disk figure.
pub fn bytes_to_gib(bytes: u64) -> f64 {
    bytes as f64 / 1024.0 / 1024.0 / 1024.0
}

/// Whether a name is one of the facts [`PlatformResources`] reports.
pub fn fact_is_reported(name: &str) -> bool {
    const FACTS: [&str; 9] = [
        "memory_total_mb",
        "memory_available_mb",
        "swap_total_mb",
        "swap_free_mb",
        "load_one",
        "battery_percent",
        "battery_status",
        "disk_available_gb",
        "cpu_count",
    ];
    FACTS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Recorded from a Linux host: 64 GiB of RAM, 2 GiB of swap.
    const MEMINFO: &str = "MemTotal:       65863004 kB\nMemFree:         1234567 kB\nMemAvailable:   41234567 kB\nBuffers:          123456 kB\nCached:         20000000 kB\nSwapTotal:       2097152 kB\nSwapFree:        1048576 kB\n";

    #[test]
    fn meminfo_parses_total_available_and_swap() {
        assert_eq!(
            parse_meminfo(MEMINFO),
            (Some(64319), Some(40268), Some(2048), Some(1024))
        );
    }

    #[test]
    fn meminfo_without_the_optional_lines_stays_partial() {
        let contents = "MemTotal:        1024000 kB\nMemFree:          512000 kB\n";
        assert_eq!(parse_meminfo(contents), (Some(1000), None, None, None));
    }

    #[test]
    fn meminfo_garbage_is_not_a_measurement() {
        assert_eq!(
            parse_meminfo("MemTotal: nonsense kB\nMemAvailable:  kB\n"),
            (None, None, None, None)
        );
    }

    #[test]
    fn loadavg_takes_the_one_minute_figure() {
        assert_eq!(parse_loadavg("0.52 0.61 0.70 2/1234 56789\n"), Some(0.52));
        assert_eq!(parse_loadavg("4.00 3.10 2.50"), Some(4.0));
        assert_eq!(parse_loadavg(""), None);
        assert_eq!(parse_loadavg("not-a-number"), None);
    }

    #[test]
    fn power_supply_reads_charge_and_state() {
        assert_eq!(
            parse_power_supply("80\n", "Discharging\n"),
            (Some(80), Some("Discharging".to_string()))
        );
    }

    #[test]
    fn power_supply_rejects_an_out_of_range_charge() {
        // A 255 here is the kernel's "unknown" sentinel, not a charge.
        assert_eq!(
            parse_power_supply("255", "Unknown"),
            (None, Some("Unknown".to_string()))
        );
        assert_eq!(parse_power_supply("", ""), (None, None));
    }

    #[test]
    fn macos_memory_counts_free_inactive_and_speculative_pages() {
        // 64 GiB installed, 16 KiB pages: 604,833 free + 1,221,554 inactive +
        // 125,866 speculative pages came from `vm_stat` on the same host.
        let (total, available) = macos_memory(68_719_476_736, 16_384, 604_833, 1_221_554, 125_866);
        assert_eq!(total, Some(65_536));
        let available = available.expect("available memory");
        // 1,952,253 pages * 16 KiB = 29.8 GiB = 30,504 MiB.
        assert!((30_500..30_510).contains(&available), "got {available} MiB");
        assert!(available < total.unwrap());
    }

    #[test]
    fn macos_memory_drops_an_available_reading_above_the_total() {
        // A torn read across two syscalls must not publish "more free than
        // installed".
        assert_eq!(macos_memory(1_073_741_824, 16_384, 100_000, 0, 0), (Some(1024), None));
    }

    #[test]
    fn macos_memory_without_a_page_size_is_unmeasured() {
        assert_eq!(macos_memory(68_719_476_736, 0, 1, 1, 1), (None, None));
    }

    #[test]
    fn macos_swap_is_skipped_when_no_swap_is_configured() {
        assert_eq!(macos_swap(0, 0), (None, None));
        assert_eq!(macos_swap(2_147_483_648, 1_073_741_824), (Some(2048), Some(1024)));
    }

    #[test]
    fn pmset_battery_reads_charge_and_state() {
        let line = " -InternalBattery-0 (id=25231459)\t80%; AC attached; not charging present: true";
        assert_eq!(
            parse_pmset_battery(line),
            (Some(80), Some("AC attached; not charging".to_string()))
        );
        let discharging = " -InternalBattery-0 (id=1)\t97%; discharging; 4:12 remaining present: true";
        assert_eq!(
            parse_pmset_battery(discharging),
            (Some(97), Some("discharging; 4:12 remaining".to_string()))
        );
    }

    #[test]
    fn pmset_without_a_battery_line_is_absent_not_failed() {
        assert_eq!(parse_pmset_battery("Now drawing from 'AC Power'"), (None, None));
    }

    #[test]
    fn windows_battery_maps_the_documented_flags() {
        // On AC, charging, 80%.
        assert_eq!(windows_battery(1, 1, 80), (Some(80), Some("Charging".to_string())));
        // On AC, not charging, 100%.
        assert_eq!(windows_battery(1, 0, 100), (Some(100), Some("AC attached".to_string())));
        // Off AC, discharging.
        assert_eq!(windows_battery(0, 0, 42), (Some(42), Some("Discharging".to_string())));
    }

    #[test]
    fn windows_battery_reports_no_battery_at_all() {
        assert_eq!(windows_battery(1, 8, 255), (None, None));
        // The unknown sentinel alone is enough.
        assert_eq!(windows_battery(0, 0, 255), (None, None));
    }

    #[test]
    fn windows_swap_subtracts_physical_memory_from_the_commit_limit() {
        const GIB: u64 = 1024 * 1024 * 1024;
        // 32 GiB RAM, 40 GiB commit limit, 8 GiB of page file of which half is
        // free beyond physical memory.
        let (total, free) = windows_swap(40 * GIB, 20 * GIB, 32 * GIB, 12 * GIB);
        assert_eq!(total, Some(8192));
        assert_eq!(free, Some(8192));
    }

    #[test]
    fn windows_swap_without_a_page_file_is_absent() {
        const GIB: u64 = 1024 * 1024 * 1024;
        assert_eq!(windows_swap(16 * GIB, 8 * GIB, 16 * GIB, 8 * GIB), (None, None));
    }

    #[test]
    fn windows_busy_share_excludes_idle_time_from_the_kernel_total() {
        // Windows counts idle time inside the kernel figure: 140 kernel ticks
        // of which 100 were idle, plus 30 user ticks. Busy = 170 - 100 = 70.
        let share = windows_busy_share(100, 140, 30).expect("busy share");
        assert!((share - 70.0 / 170.0).abs() < 1e-12, "got {share}");
    }

    #[test]
    fn windows_busy_share_clamps_a_torn_idle_reading() {
        // Idle can never exceed the totals it is part of; a torn read is
        // clamped to "fully idle" instead of going negative.
        let share = windows_busy_share(500, 40, 30).expect("busy share");
        assert!((share - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn windows_busy_share_without_elapsed_time_is_unmeasured() {
        assert_eq!(windows_busy_share(0, 0, 0), None);
    }

    #[test]
    fn windows_load_equivalent_scales_the_busy_share_to_processors() {
        assert!((windows_load_equivalent(0.5, 8) - 4.0).abs() < f64::EPSILON);
        // A nonsensical share is clamped instead of producing a negative load.
        assert!((windows_load_equivalent(1.5, 4) - 4.0).abs() < f64::EPSILON);
        assert!((windows_load_equivalent(-1.0, 4) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn bytes_to_gib_converts_binary_gigabytes() {
        assert!((bytes_to_gib(1024 * 1024 * 1024) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn reported_fact_names_cover_exactly_the_struct_fields() {
        // Every name an adapter may put in `unavailable` must be a field the
        // report can render, so a typo cannot hide a fact from the reader.
        for name in [
            "memory_total_mb",
            "memory_available_mb",
            "swap_total_mb",
            "swap_free_mb",
            "load_one",
            "battery_percent",
            "battery_status",
            "disk_available_gb",
            "cpu_count",
        ] {
            assert!(fact_is_reported(name), "{name} must be reported");
        }
        assert!(!fact_is_reported("battery"));
        assert!(!fact_is_reported(""));
    }
}
