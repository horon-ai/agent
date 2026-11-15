use std::sync::Mutex;
use std::time::Instant;

static PROFILE: Mutex<Option<StartupProfile>> = Mutex::new(None);

/// A non-empty value makes the serve path write its startup stage table to
/// the log once the daemon socket is bound. Without it nothing is written.
pub const PROFILE_ENV: &str = "HORON_AGENT_STARTUP_PROFILE";

/// Config and provider setup are done (provider init reads config.toml and the
/// provider profile).
pub const CONFIG_LOADED: &str = "config_loaded";
/// The server value exists, with its persisted swarm state loaded.
pub const SERVER_NEW: &str = "server_new";
/// The main and debug sockets are bound.
pub const SOCKET_BOUND: &str = "socket_bound";
/// The stages the serve path marks, in order.
pub const SERVE_MARKS: [&str; 3] = [CONFIG_LOADED, SERVER_NEW, SOCKET_BOUND];

pub struct StartupProfile {
    start: Instant,
    marks: Vec<(String, Instant)>,
}

impl StartupProfile {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            start: now,
            marks: vec![("process_start".to_string(), now)],
        }
    }
}

pub fn init() {
    let mut guard = match PROFILE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    *guard = Some(StartupProfile::new());
}

pub fn mark(name: &str) {
    if let Ok(mut guard) = PROFILE.lock()
        && let Some(ref mut profile) = *guard
    {
        profile.marks.push((name.to_string(), Instant::now()));
    }
}

pub fn report() -> String {
    let guard = match PROFILE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let profile = match guard.as_ref() {
        Some(p) => p,
        None => return "No startup profile recorded".to_string(),
    };

    let total = profile
        .marks
        .last()
        .map(|(_, instant)| instant.duration_since(profile.start))
        .unwrap_or_default();
    let mut lines = vec![format!(
        "=== Startup Profile ({:.1}ms total) ===",
        total.as_secs_f64() * 1000.0
    )];

    for i in 1..profile.marks.len() {
        let delta = profile.marks[i].1.duration_since(profile.marks[i - 1].1);
        let from_start = profile.marks[i].1.duration_since(profile.start);
        let pct = if total.as_nanos() > 0 {
            (delta.as_nanos() as f64 / total.as_nanos() as f64) * 100.0
        } else {
            0.0
        };
        let bar = "█".repeat((pct / 2.0).ceil() as usize);
        lines.push(format!(
            "  {:>7.1}ms  {:>7.1}ms  {:>5.1}%  {:<30} {}",
            from_start.as_secs_f64() * 1000.0,
            delta.as_secs_f64() * 1000.0,
            pct,
            profile.marks[i].0,
            bar,
        ));
    }

    lines.join("\n")
}

fn enabled(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

fn report_lines(enabled: bool) -> Vec<String> {
    if !enabled {
        return Vec::new();
    }
    report().lines().map(str::to_owned).collect()
}

/// Write the startup stage table to the log when [`PROFILE_ENV`] is set.
pub fn report_to_log() {
    for line in report_lines(enabled(std::env::var_os(PROFILE_ENV).as_deref())) {
        crate::logging::info(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn startup_profile_is_silent_without_env() {
        init();
        for name in SERVE_MARKS {
            mark(name);
        }
        assert!(!enabled(None));
        assert!(!enabled(Some(OsStr::new(""))));
        assert!(enabled(Some(OsStr::new("1"))));
        assert!(report_lines(false).is_empty());

        let lines = report_lines(true);
        assert!(lines[0].starts_with("=== Startup Profile"), "{lines:?}");
        for name in SERVE_MARKS {
            assert!(
                lines.iter().any(|line| line.split_whitespace().nth(3) == Some(name)),
                "{name} missing from {lines:?}"
            );
        }
    }
}
