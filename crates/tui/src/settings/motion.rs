//! The motion preset and the frame cadence it allows.
//!
//! A turn in flight is the only thing on screen that changes without an event:
//! the spinner beside `running` and the rows of text still arriving. `motion`
//! decides whether that animation runs at all, `motion.spinner_interval` how
//! long one frame stays, and `motion.max_fps` the most frames a second a stream
//! may draw. Keystrokes never wait for a frame: the event loop's poll timeout is
//! what a key waits for, so a slow cadence lowers the redraw rate without making
//! the keyboard late.

use std::time::Duration;

use super::layers::{SettingValue, Settings};
use crate::theme::Symbols;

/// The presets the `motion` setting accepts, in the order it lists them.
pub const MODES: &[&str] = &["full", "reduced", "static"];

/// Milliseconds one spinner frame stays when no layer sets the interval.
///
/// It is the cadence the client drew a running turn at before the setting
/// existed, so the default is not a change in how often a stream redraws; the
/// frame limit only tightens from here.
pub const DEFAULT_SPINNER_INTERVAL: i64 = 50;
/// The most frames a second a stream draws when no layer sets the limit.
pub const DEFAULT_MAX_FPS: i64 = 30;
/// The shortest interval a frame may stay: below it the spinner is a blur and
/// the client redraws faster than a terminal can present.
pub const MIN_SPINNER_INTERVAL: i64 = 40;
/// The longest interval a frame may stay: beyond it a turn looks stopped.
pub const MAX_SPINNER_INTERVAL: i64 = 2_000;
/// The slowest stream the limit may ask for.
pub const MIN_MAX_FPS: i64 = 1;
/// The fastest stream the limit may ask for.
pub const MAX_MAX_FPS: i64 = 240;

/// A named amount of animation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MotionMode {
    /// The spinner turns and a quiet stream redraws on its interval.
    #[default]
    Full,
    /// The spinner keeps its first frame; nothing moves on its own.
    Reduced,
    /// No spinner at all, and no frame without an event.
    Static,
}

impl MotionMode {
    /// The preset a settings or flag value names.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "full" => Some(Self::Full),
            "reduced" => Some(Self::Reduced),
            "static" => Some(Self::Static),
            _ => None,
        }
    }

    /// The name the settings registry and the flag use.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Reduced => "reduced",
            Self::Static => "static",
        }
    }
}

/// The resolved motion: the preset, and the bounded timing it implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Motion {
    /// The preset in force.
    pub mode: MotionMode,
    /// How long one spinner frame stays.
    spinner_interval: Duration,
    /// The fastest stream the frame limit allows.
    fastest_frame: Duration,
}

impl Default for Motion {
    fn default() -> Self {
        Self::from_values(MotionMode::default(), DEFAULT_SPINNER_INTERVAL, DEFAULT_MAX_FPS)
    }
}

impl Motion {
    /// The motion a resolved settings snapshot asks for, and what was rejected.
    ///
    /// A number outside its range is refused rather than clamped: a value the
    /// user did not mean applies nothing, and the warning names the default that
    /// took its place.
    pub fn from_settings(settings: &Settings) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        // The enum declaration refuses a value outside the list before it gets
        // here, so an unreadable preset means the key was never set.
        let mode = match settings.text("motion.preset").and_then(MotionMode::parse) {
            Some(mode) => mode,
            None => MotionMode::default(),
        };
        let spinner_interval = bounded(
            settings.number("motion.spinner_interval"),
            "motion.spinner_interval",
            "milliseconds",
            MIN_SPINNER_INTERVAL..=MAX_SPINNER_INTERVAL,
            DEFAULT_SPINNER_INTERVAL,
            &mut warnings,
        );
        let max_fps = bounded(
            settings.number("motion.max_fps"),
            "motion.max_fps",
            "frames a second",
            MIN_MAX_FPS..=MAX_MAX_FPS,
            DEFAULT_MAX_FPS,
            &mut warnings,
        );
        (Self::from_values(mode, spinner_interval, max_fps), warnings)
    }

    fn from_values(mode: MotionMode, spinner_interval: i64, max_fps: i64) -> Self {
        // The limit is a floor on the interval, so a slow spinner interval and a
        // high limit cannot both be honoured: the frame the setting asks for
        // stays, and the limit only adds frames below it.
        let fastest_frame = Duration::from_micros(1_000_000 / u64::try_from(max_fps.max(1)).unwrap_or(1));
        Self {
            mode,
            spinner_interval: Duration::from_millis(u64::try_from(spinner_interval.max(1)).unwrap_or(1)),
            fastest_frame: fastest_frame.max(Duration::from_millis(1)),
        }
    }

    /// How long a frame of a turn in flight may stay, or `None` when the preset
    /// animates nothing and only an event draws.
    pub fn frame_interval(self) -> Option<Duration> {
        match self.mode {
            MotionMode::Full => Some(self.spinner_interval.max(self.fastest_frame)),
            MotionMode::Reduced | MotionMode::Static => None,
        }
    }

    /// The glyph a running turn draws at `frame`, or `None` for a preset that
    /// draws no spinner.
    ///
    /// The frames come from the glyph preset, so a terminal set to ASCII spins
    /// with ASCII marks instead of announcing Unicode it cannot draw.
    pub fn glyph(self, symbols: Symbols, frame: usize) -> Option<&'static str> {
        let frames = symbols.spinner;
        match self.mode {
            MotionMode::Full => frames.get(frame % frames.len().max(1)).copied(),
            MotionMode::Reduced => frames.first().copied(),
            MotionMode::Static => None,
        }
    }
}

/// One bounded number from the settings, or the default with a warning.
fn bounded(
    value: Option<i64>,
    id: &str,
    unit: &str,
    range: std::ops::RangeInclusive<i64>,
    default: i64,
    warnings: &mut Vec<String>,
) -> i64 {
    let Some(value) = value else {
        return default;
    };
    if range.contains(&value) {
        return value;
    }
    let (low, high) = (range.start(), range.end());
    warnings.push(format!(
        "{id} must be {low} to {high} {unit}; {value} is ignored and {default} applies"
    ));
    default
}

/// A command-line value the motion flags refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BadArgument(String);

impl std::fmt::Display for BadArgument {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<BadArgument> for String {
    fn from(error: BadArgument) -> Self {
        error.0
    }
}

/// A `--motion` value, or why it cannot be used.
pub fn parse_mode(value: &str) -> Result<MotionMode, BadArgument> {
    MotionMode::parse(value)
        .ok_or_else(|| BadArgument(format!("expected one of {}, found `{value}`", MODES.join(", "))))
}

/// A `--spinner-interval` value, or why it cannot be used.
pub fn parse_spinner_interval(raw: &str) -> Result<i64, BadArgument> {
    parse_bounded(raw, "milliseconds", MIN_SPINNER_INTERVAL, MAX_SPINNER_INTERVAL)
}

/// A `--max-fps` value, or why it cannot be used.
pub fn parse_max_fps(raw: &str) -> Result<i64, BadArgument> {
    parse_bounded(raw, "frames a second", MIN_MAX_FPS, MAX_MAX_FPS)
}

fn parse_bounded(raw: &str, unit: &str, min: i64, max: i64) -> Result<i64, BadArgument> {
    let Ok(value) = raw.trim().parse::<i64>() else {
        return Err(BadArgument(format!("expected a whole number of {unit}, found `{raw}`")));
    };
    if (min..=max).contains(&value) {
        return Ok(value);
    }
    Err(BadArgument(format!("expected {min} to {max} {unit}, found `{raw}`")))
}

/// The settings the command line overrode, as registry ids and values.
///
/// A launch flag outranks every file for the life of the process, so it is
/// carried by the reloader and reapplied after each resolve instead of being
/// written anywhere.
pub fn launch_overrides(
    mode: Option<MotionMode>,
    spinner_interval: Option<i64>,
    max_fps: Option<i64>,
) -> Vec<(&'static str, SettingValue)> {
    let mut overrides = Vec::new();
    if let Some(mode) = mode {
        overrides.push(("motion.preset", SettingValue::Str(mode.as_str().to_string())));
    }
    if let Some(value) = spinner_interval {
        overrides.push(("motion.spinner_interval", SettingValue::Number(value)));
    }
    if let Some(value) = max_fps {
        overrides.push(("motion.max_fps", SettingValue::Number(value)));
    }
    overrides
}

#[cfg(test)]
#[path = "motion_tests.rs"]
mod tests;
