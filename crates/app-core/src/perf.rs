use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerformanceTier {
    Full,
    Reduced,
    Minimal,
}

impl PerformanceTier {
    pub fn label(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Reduced => "reduced",
            Self::Minimal => "minimal",
        }
    }

    pub fn badge(self) -> Option<&'static str> {
        match self {
            Self::Full => None,
            Self::Reduced => Some("perf:reduced"),
            Self::Minimal => Some("perf:minimal"),
        }
    }

    pub fn animations_enabled(self) -> bool {
        !matches!(self, Self::Minimal)
    }

    pub fn idle_animation_enabled(self) -> bool {
        matches!(self, Self::Full)
    }

    pub fn prompt_entry_animation_enabled(self) -> bool {
        !matches!(self, Self::Minimal)
    }
}

impl std::fmt::Display for PerformanceTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone)]
pub struct SystemProfile {
    pub load_avg_1m: Option<f64>,
    pub cpu_count: Option<usize>,
    pub available_memory_mb: Option<u64>,
    pub total_memory_mb: Option<u64>,
    pub is_ssh: bool,
    pub is_wsl: bool,
    pub terminal: String,
    pub tier: PerformanceTier,
    /// True when the host terminal is known to corrupt its GPU glyph atlas
    /// under heavy per-cell color/redraw churn (the macOS 26 "garbled glyphs"
    /// bug seen in the VS Code integrated terminal and Apple Terminal, where
    /// letters like n/m/r/w get re-rendered as stale boxes). When set we run a
    /// "glyph-safe" policy that suppresses decorative per-cell color animation
    /// and caps full-frame repaints to keep the atlas stable.
    pub fragile_glyph_cache: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyntheticSystemProfile {
    Native,
    Wsl,
    WslWindowsTerminal,
}

impl SyntheticSystemProfile {
    pub fn label(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Wsl => "wsl",
            Self::WslWindowsTerminal => "wsl-windows-terminal",
        }
    }
}

impl SystemProfile {
    pub fn load_ratio(&self) -> Option<f64> {
        match (self.load_avg_1m, self.cpu_count) {
            (Some(load), Some(cpus)) if cpus > 0 => Some(load / cpus as f64),
            _ => None,
        }
    }

    pub fn memory_pressure(&self) -> Option<f64> {
        match (self.available_memory_mb, self.total_memory_mb) {
            (Some(avail), Some(total)) if total > 0 => Some(1.0 - (avail as f64 / total as f64)),
            _ => None,
        }
    }

    pub fn is_windows_terminal(&self) -> bool {
        self.terminal == "windows-terminal"
    }

    pub fn is_windows_terminal_family(&self) -> bool {
        matches!(self.terminal.as_str(), "windows-terminal" | "cmd" | "conhost")
    }

    pub fn is_wsl_windows_terminal(&self) -> bool {
        self.is_wsl && self.is_windows_terminal()
    }
}

static PROFILE: OnceLock<SystemProfile> = OnceLock::new();

pub fn profile() -> &'static SystemProfile {
    PROFILE.get_or_init(detect)
}

/// Pin the process-global system profile to the synthetic Full-tier profile.
///
/// Test harnesses call this so rendered output (perf badge, animation policy,
/// idle status facts) does not depend on the host's load average or free
/// memory at the moment the test process first touched `profile()`. First
/// initialization wins: calling this after `profile()` has already run is a
/// no-op, and production code paths never call it.
pub fn pin_full_profile_for_tests() {
    let _ = PROFILE.set(synthetic_profile(SyntheticSystemProfile::Native));
}

pub fn synthetic_profile(kind: SyntheticSystemProfile) -> SystemProfile {
    match kind {
        SyntheticSystemProfile::Native => SystemProfile {
            load_avg_1m: Some(0.2),
            cpu_count: Some(8),
            available_memory_mb: Some(8192),
            total_memory_mb: Some(16384),
            is_ssh: false,
            is_wsl: false,
            terminal: "kitty".to_string(),
            tier: PerformanceTier::Full,
            fragile_glyph_cache: false,
        },
        SyntheticSystemProfile::Wsl => SystemProfile {
            load_avg_1m: Some(0.4),
            cpu_count: Some(8),
            available_memory_mb: Some(8192),
            total_memory_mb: Some(16384),
            is_ssh: false,
            is_wsl: true,
            terminal: "wezterm".to_string(),
            tier: compute_tier(Some(0.4), Some(8), Some(8192), Some(16384), false, true, "wezterm"),
            fragile_glyph_cache: false,
        },
        SyntheticSystemProfile::WslWindowsTerminal => SystemProfile {
            load_avg_1m: Some(0.4),
            cpu_count: Some(8),
            available_memory_mb: Some(8192),
            total_memory_mb: Some(16384),
            is_ssh: false,
            is_wsl: true,
            terminal: "windows-terminal".to_string(),
            tier: compute_tier(
                Some(0.4),
                Some(8),
                Some(8192),
                Some(16384),
                false,
                true,
                "windows-terminal",
            ),
            fragile_glyph_cache: false,
        },
    }
}

pub fn init_background() {
    std::thread::spawn(|| {
        let p = PROFILE.get_or_init(detect);
        crate::logging::info(&format!(
            "perf: tier={} terminal={} ssh={} wsl={} glyph_safe={} load={} cpus={} mem_avail={}MB mem_total={}MB",
            p.tier,
            p.terminal,
            p.is_ssh,
            p.is_wsl,
            p.fragile_glyph_cache,
            p.load_avg_1m.map(|v| format!("{:.1}", v)).unwrap_or_else(|| "?".into()),
            p.cpu_count.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            p.available_memory_mb
                .map(|v| v.to_string())
                .unwrap_or_else(|| "?".into()),
            p.total_memory_mb.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
        ));
    });
}

fn detect() -> SystemProfile {
    let is_ssh = std::env::var("SSH_CONNECTION").is_ok() || std::env::var("SSH_TTY").is_ok();
    let is_wsl = detect_wsl();
    let terminal = detect_terminal();
    let (load_avg_1m, cpu_count) = detect_load();
    let (available_memory_mb, total_memory_mb) = detect_memory();

    let auto_tier = compute_tier(
        load_avg_1m,
        cpu_count,
        available_memory_mb,
        total_memory_mb,
        is_ssh,
        is_wsl,
        &terminal,
    );

    // Highest priority: explicit env override (used by tests/CI to keep the
    // tier deterministic regardless of host load, and by users to force a
    // tier for one invocation without editing config).
    let env_tier =
        std::env::var("HORON_AGENT_PERF_TIER")
            .ok()
            .and_then(|raw| match raw.trim().to_ascii_lowercase().as_str() {
                "full" => Some(PerformanceTier::Full),
                "reduced" => Some(PerformanceTier::Reduced),
                "minimal" => Some(PerformanceTier::Minimal),
                _ => None,
            });

    let tier = env_tier.unwrap_or(auto_tier);

    SystemProfile {
        load_avg_1m,
        cpu_count,
        available_memory_mb,
        total_memory_mb,
        is_ssh,
        is_wsl,
        fragile_glyph_cache: detect_fragile_glyph_cache(&terminal),
        terminal,
        tier,
    }
}

/// Detect terminals whose GPU glyph atlas corrupts under heavy per-cell
/// color/redraw churn. On macOS 26 (Tahoe) the VS Code integrated terminal
/// (xterm.js) and Apple Terminal exhibit the "garbled glyphs" bug where a
/// fixed set of similar-shaped letters (n/m/r/w/...) get re-rendered as stale
/// cached boxes once the atlas overflows. Anthropic shipped the same class of
/// bug for Claude Code (anthropics/claude-code#60831, #61562) with the
/// `gpuAcceleration: off` workaround; we instead reduce the churn that
/// surfaces it. GPU-robust terminals (Ghostty, iTerm2, kitty, WezTerm,
/// Alacritty) are unaffected and excluded.
fn detect_fragile_glyph_cache(terminal: &str) -> bool {
    // Opt-out / opt-in override for users who want to force the behavior.
    if let Ok(raw) = std::env::var("HORON_AGENT_GLYPH_SAFE_MODE") {
        match raw.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => return true,
            "0" | "false" | "no" | "off" => return false,
            _ => {}
        }
    }

    // Only macOS surfaces this; other platforms render these terminals fine.
    if !cfg!(target_os = "macos") {
        return false;
    }

    matches!(terminal, "vscode" | "apple_terminal")
}

fn compute_tier(
    load_avg: Option<f64>,
    cpu_count: Option<usize>,
    avail_mb: Option<u64>,
    _total_mb: Option<u64>,
    is_ssh: bool,
    is_wsl: bool,
    terminal: &str,
) -> PerformanceTier {
    if is_ssh {
        return PerformanceTier::Minimal;
    }

    let mut score: i32 = 0;

    if let (Some(load), Some(cpus)) = (load_avg, cpu_count) {
        let ratio = load / cpus as f64;
        if ratio > 2.0 {
            score += 3;
        } else if ratio > 1.0 {
            score += 2;
        } else if ratio > 0.8 {
            score += 1;
        }
    }

    if let Some(avail) = avail_mb {
        if avail < 512 {
            score += 3;
        } else if avail < 1024 {
            score += 2;
        } else if avail < 2048 {
            score += 1;
        }
    }

    if is_wsl {
        score += 1;
    }

    match terminal {
        "windows-terminal" | "cmd" | "conhost" => score += 1,
        _ => {}
    }

    match score {
        0..=1 => PerformanceTier::Full,
        2..=3 => PerformanceTier::Reduced,
        _ => PerformanceTier::Minimal,
    }
}

fn detect_wsl() -> bool {
    if std::env::var("WSL_DISTRO_NAME").is_ok() || std::env::var("WSLENV").is_ok() {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(v) = std::fs::read_to_string("/proc/version") {
            let lower = v.to_ascii_lowercase();
            if lower.contains("microsoft") || lower.contains("wsl") {
                return true;
            }
        }
    }
    false
}

fn detect_terminal() -> String {
    if std::env::var("WT_SESSION").is_ok() {
        return "windows-terminal".to_string();
    }
    if std::env::var("WEZTERM_EXECUTABLE").is_ok() || std::env::var("WEZTERM_PANE").is_ok() {
        return "wezterm".to_string();
    }
    if std::env::var("KITTY_PID").is_ok() {
        return "kitty".to_string();
    }
    if std::env::var("GHOSTTY_RESOURCES_DIR").is_ok() {
        return "ghostty".to_string();
    }
    if std::env::var("ALACRITTY_WINDOW_ID").is_ok() {
        return "alacritty".to_string();
    }
    if let Ok(tp) = std::env::var("TERM_PROGRAM") {
        return tp.to_lowercase();
    }
    "unknown".to_string()
}

#[cfg(target_os = "linux")]
fn detect_load() -> (Option<f64>, Option<usize>) {
    let load = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|s| s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()));

    let cpus = std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .map(|s| s.matches("processor\t:").count())
        .filter(|&c| c > 0)
        .or_else(|| std::thread::available_parallelism().ok().map(|n| n.get()));

    (load, cpus)
}

#[cfg(target_os = "macos")]
fn detect_load() -> (Option<f64>, Option<usize>) {
    let load = {
        let mut loadavg: [libc::c_double; 3] = [0.0; 3];
        let n = unsafe { libc::getloadavg(loadavg.as_mut_ptr(), 1) };
        if n >= 1 { Some(loadavg[0]) } else { None }
    };
    let cpus = std::thread::available_parallelism().ok().map(|n| n.get());
    (load, cpus)
}

#[cfg(windows)]
fn detect_load() -> (Option<f64>, Option<usize>) {
    let cpus = std::thread::available_parallelism().ok().map(|n| n.get());
    (None, cpus)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn detect_load() -> (Option<f64>, Option<usize>) {
    let cpus = std::thread::available_parallelism().ok().map(|n| n.get());
    (None, cpus)
}

#[cfg(target_os = "linux")]
fn detect_memory() -> (Option<u64>, Option<u64>) {
    let contents = match std::fs::read_to_string("/proc/meminfo") {
        Ok(c) => c,
        Err(_) => return (None, None),
    };

    let mut total_kb: Option<u64> = None;
    let mut available_kb: Option<u64> = None;

    for line in contents.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = parse_meminfo_kb(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available_kb = parse_meminfo_kb(rest);
        }
        if total_kb.is_some() && available_kb.is_some() {
            break;
        }
    }

    (available_kb.map(|k| k / 1024), total_kb.map(|k| k / 1024))
}

#[cfg(target_os = "linux")]
fn parse_meminfo_kb(s: &str) -> Option<u64> {
    s.split_whitespace().next()?.parse().ok()
}

#[cfg(windows)]
fn detect_memory() -> (Option<u64>, Option<u64>) {
    use std::mem;

    #[repr(C)]
    struct MemoryStatusEx {
        dw_length: u32,
        dw_memory_load: u32,
        ull_total_phys: u64,
        ull_avail_phys: u64,
        ull_total_page_file: u64,
        ull_avail_page_file: u64,
        ull_total_virtual: u64,
        ull_avail_virtual: u64,
        ull_avail_extended_virtual: u64,
    }

    unsafe extern "system" {
        fn GlobalMemoryStatusEx(lpBuffer: *mut MemoryStatusEx) -> i32;
    }

    let mut status: MemoryStatusEx = unsafe { mem::zeroed() };
    status.dw_length = mem::size_of::<MemoryStatusEx>() as u32;

    let ret = unsafe { GlobalMemoryStatusEx(&mut status) };
    if ret != 0 {
        let total_mb = status.ull_total_phys / (1024 * 1024);
        let avail_mb = status.ull_avail_phys / (1024 * 1024);
        (Some(avail_mb), Some(total_mb))
    } else {
        (None, None)
    }
}

#[cfg(target_os = "macos")]
fn detect_memory() -> (Option<u64>, Option<u64>) {
    let total = {
        let mut size: u64 = 0;
        let mut len = std::mem::size_of::<u64>();
        let name = c"hw.memsize";
        let ret = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                &mut size as *mut u64 as *mut libc::c_void,
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if ret == 0 { Some(size / (1024 * 1024)) } else { None }
    };

    // macOS doesn't have a simple "available" metric like Linux's MemAvailable.
    // vm_stat gives pages free + inactive but parsing it adds complexity.
    // For tier detection, total memory is sufficient on macOS.
    (None, total)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn detect_memory() -> (Option<u64>, Option<u64>) {
    (None, None)
}

/// Serializes tests that reset or measure the process-wide perf counters.
///
/// `horon_agent_perf_counters` is one set of atomics for the whole test
/// binary, so a test that resets it and then asserts a delta is also measuring
/// every other test's traffic. Tests hold this around reset-and-measure;
/// production code only increments the counters, so it never takes the lock.
#[cfg(test)]
pub(crate) async fn lock_perf_counters() -> tokio::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::LazyLock<tokio::sync::Mutex<()>> = std::sync::LazyLock::new(|| tokio::sync::Mutex::new(()));
    LOCK.lock().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ssh_is_minimal() {
        let tier = compute_tier(Some(0.1), Some(8), Some(8000), Some(16000), true, false, "kitty");
        assert_eq!(tier, PerformanceTier::Minimal);
    }

    #[test]
    fn test_healthy_system_is_full() {
        let tier = compute_tier(Some(0.5), Some(8), Some(8000), Some(16000), false, false, "kitty");
        assert_eq!(tier, PerformanceTier::Full);
    }

    #[test]
    fn test_high_load_reduces() {
        let tier = compute_tier(Some(12.0), Some(4), Some(8000), Some(16000), false, false, "kitty");
        assert!(matches!(tier, PerformanceTier::Reduced | PerformanceTier::Minimal));
    }

    #[test]
    fn test_low_memory_reduces() {
        let tier = compute_tier(Some(0.5), Some(8), Some(400), Some(16000), false, false, "kitty");
        assert!(matches!(tier, PerformanceTier::Reduced | PerformanceTier::Minimal));
    }

    #[test]
    fn test_wsl_penalty() {
        let tier_no_wsl = compute_tier(Some(0.5), Some(4), Some(3000), Some(8000), false, false, "wezterm");
        let tier_wsl = compute_tier(Some(0.5), Some(4), Some(3000), Some(8000), false, true, "wezterm");
        assert!(tier_wsl as i32 >= tier_no_wsl as i32);
    }

    #[test]
    fn test_windows_terminal_penalty() {
        let tier_kitty = compute_tier(Some(0.7), Some(4), Some(2500), Some(8000), false, false, "kitty");
        let tier_wt = compute_tier(
            Some(0.7),
            Some(4),
            Some(2500),
            Some(8000),
            false,
            false,
            "windows-terminal",
        );
        assert!(tier_wt as i32 >= tier_kitty as i32);
    }

    #[test]
    fn test_profile_accessors() {
        let p = SystemProfile {
            load_avg_1m: Some(4.0),
            cpu_count: Some(8),
            available_memory_mb: Some(4000),
            total_memory_mb: Some(16000),
            is_ssh: false,
            is_wsl: false,
            terminal: "kitty".to_string(),
            tier: PerformanceTier::Full,
            fragile_glyph_cache: false,
        };
        assert!((p.load_ratio().unwrap() - 0.5).abs() < 0.01);
        assert!((p.memory_pressure().unwrap() - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_tier_display() {
        assert_eq!(PerformanceTier::Full.label(), "full");
        assert_eq!(PerformanceTier::Reduced.label(), "reduced");
        assert_eq!(PerformanceTier::Minimal.label(), "minimal");
    }

    #[test]
    fn test_badge() {
        assert!(PerformanceTier::Full.badge().is_none());
        assert!(PerformanceTier::Reduced.badge().is_some());
        assert!(PerformanceTier::Minimal.badge().is_some());
    }

    #[test]
    fn test_animation_gates() {
        assert!(PerformanceTier::Full.animations_enabled());
        assert!(PerformanceTier::Full.idle_animation_enabled());
        assert!(PerformanceTier::Full.prompt_entry_animation_enabled());

        assert!(PerformanceTier::Reduced.animations_enabled());
        assert!(!PerformanceTier::Reduced.idle_animation_enabled());
        assert!(PerformanceTier::Reduced.prompt_entry_animation_enabled());

        assert!(!PerformanceTier::Minimal.animations_enabled());
        assert!(!PerformanceTier::Minimal.idle_animation_enabled());
        assert!(!PerformanceTier::Minimal.prompt_entry_animation_enabled());
    }

    #[test]
    fn test_detect_runs() {
        let p = detect();
        assert!(!p.terminal.is_empty());
    }

    #[test]
    fn test_detect_fragile_glyph_cache_targets_macos_terminals() {
        // Env override must not leak between cases.
        let prev = std::env::var("HORON_AGENT_GLYPH_SAFE_MODE").ok();
        unsafe {
            std::env::remove_var("HORON_AGENT_GLYPH_SAFE_MODE");
        }
        assert!(detect_fragile_glyph_cache("vscode"));
        assert!(detect_fragile_glyph_cache("apple_terminal"));
        assert!(!detect_fragile_glyph_cache("ghostty"));
        assert!(!detect_fragile_glyph_cache("iterm.app"));
        assert!(!detect_fragile_glyph_cache("kitty"));
        if let Some(prev) = prev {
            unsafe {
                std::env::set_var("HORON_AGENT_GLYPH_SAFE_MODE", prev);
            }
        }
    }
}
