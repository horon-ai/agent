//! The hardware lines of a session context: machine, CPU, GPU and memory.

use std::path::{Path, PathBuf};
use std::process::Command;

pub(super) fn hardware_context() -> Option<String> {
    // Hardware never changes for the life of the process, but this used to be
    // rebuilt for every session create/attach, forking `lspci` each time. On a
    // busy shared server that meant one subprocess per client connection.
    static HARDWARE_CONTEXT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    HARDWARE_CONTEXT.get_or_init(hardware_context_uncached).clone()
}

fn hardware_context_uncached() -> Option<String> {
    let mut lines = Vec::new();

    if let Some(machine) = machine_model() {
        lines.push(format!("  Machine: {}", machine));
    }
    if let Some(cpu) = cpu_model() {
        lines.push(format!("  CPU: {}", cpu));
    }
    if let Some(gpu) = gpu_summary() {
        lines.push(format!("  GPU: {}", gpu));
    }
    if let Some(memory) = memory_summary() {
        lines.push(format!("  Memory: {}", memory));
    }

    if lines.is_empty() {
        None
    } else {
        let mut out = vec!["Hardware:".to_string()];
        out.extend(lines);
        Some(out.join("\n"))
    }
}

fn read_trimmed_file(path: impl Into<PathBuf>) -> Option<String> {
    std::fs::read_to_string(path.into())
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn machine_model() -> Option<String> {
    let vendor = read_trimmed_file("/sys/devices/virtual/dmi/id/sys_vendor");
    let product = read_trimmed_file("/sys/devices/virtual/dmi/id/product_name");
    match (vendor, product) {
        (Some(vendor), Some(product)) if product.contains(&vendor) => Some(product),
        (Some(vendor), Some(product)) => Some(format!("{} {}", vendor, product)),
        (None, Some(product)) => Some(product),
        (Some(vendor), None) => Some(vendor),
        (None, None) => None,
    }
}

fn cpu_model() -> Option<String> {
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").ok()?;
    cpuinfo.lines().find_map(|line| {
        let (_, value) = line.split_once(':')?;
        if line.trim_start().starts_with("model name") {
            let value = value.trim();
            if value.is_empty() {
                None
            } else {
                Some(value.to_string())
            }
        } else {
            None
        }
    })
}

fn memory_summary() -> Option<String> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb = meminfo.lines().find_map(|line| {
        let rest = line.strip_prefix("MemTotal:")?.trim();
        rest.split_whitespace().next()?.parse::<u64>().ok()
    })?;
    let gib = kb as f64 / 1024.0 / 1024.0;
    Some(format!("{:.1} GiB", gib))
}

/// Where Linux lists PCI devices, one directory each with a `class` file.
const SYSFS_PCI_DEVICES: &str = "/sys/bus/pci/devices";

/// PCI class prefixes `lspci` names "VGA compatible controller" (0x0300),
/// "3D controller" (0x0302) and "Display controller" (0x0380): the three
/// kinds the GPU line reports.
const DISPLAY_CLASS_PREFIXES: [&str; 3] = ["0x0300", "0x0302", "0x0380"];

pub(super) fn gpu_summary() -> Option<String> {
    gpu_summary_under(Path::new(SYSFS_PCI_DEVICES), lspci_display_controllers)
}

/// Runs `probe` only when `pci_devices` lists a display controller. A machine
/// without one (a headless virtual machine, for example) and a system without
/// that directory (macOS, Windows) have no GPU line to report, so the `lspci`
/// process is not started for them.
pub(super) fn gpu_summary_under(pci_devices: &Path, probe: impl FnOnce() -> Option<String>) -> Option<String> {
    if lists_display_controller(pci_devices) {
        probe()
    } else {
        None
    }
}

pub(super) fn lists_display_controller(pci_devices: &Path) -> bool {
    let Ok(devices) = std::fs::read_dir(pci_devices) else {
        return false;
    };
    devices.flatten().any(|device| {
        read_trimmed_file(device.path().join("class"))
            .is_some_and(|class| DISPLAY_CLASS_PREFIXES.iter().any(|prefix| class.starts_with(prefix)))
    })
}

pub(super) fn lspci_display_controllers() -> Option<String> {
    horon_agent_perf_counters::record_session_context_gpu_probe_spawn();
    let output = Command::new("lspci").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut gpus: Vec<String> = text
        .lines()
        .filter(|line| {
            line.contains(" VGA compatible controller")
                || line.contains(" 3D controller")
                || line.contains(" Display controller")
        })
        .filter_map(|line| line.split_once(':').map(|(_, rest)| rest.trim().to_string()))
        .collect();
    gpus.dedup();
    if gpus.is_empty() { None } else { Some(gpus.join("; ")) }
}
