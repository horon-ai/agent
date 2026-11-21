//! What this machine can do for on-device speech, decided without starting
//! anything.
//!
//! Only files are looked at: a program in an absolute `PATH` entry, one
//! `ggml-*.bin` model under `$HORON_HOME/models`, and the player macOS keeps in
//! `/usr/bin`. A probe never runs a program, opens a device or downloads
//! anything, so the first `/voice` can say exactly what is missing without
//! touching the microphone.

use std::fs;
use std::path::{Path, PathBuf};

/// Least size a model file must have to be treated as a real model.
const MIN_MODEL_BYTES: u64 = 1024 * 1024;

/// How a program is told to do its work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Flavor {
    /// `ffmpeg` with the AVFoundation input.
    Ffmpeg,
    /// `arecord` with the ALSA input.
    Arecord,
    /// `whisper-cli` over one WAV file.
    Whisper,
    /// macOS `say`, or an espeak build, both reading a file.
    FileSpeaker,
}

/// One installed program and the argv shape it needs.
#[derive(Debug, Clone)]
pub(super) struct Tool {
    pub(super) path: PathBuf,
    pub(super) flavor: Flavor,
}

/// What this machine can do, decided without starting anything.
#[derive(Debug, Clone)]
pub(crate) struct Capability {
    pub(super) capture: Option<Tool>,
    pub(super) recognize: Option<Tool>,
    pub(super) model: Option<PathBuf>,
    pub(super) speak: Option<Tool>,
    unsupported: Option<String>,
}

impl Capability {
    /// The sentence that names every missing capture capability, or `None`
    /// when capture is ready.
    pub(crate) fn recognition_problem(&self) -> Option<String> {
        if let Some(reason) = &self.unsupported {
            return Some(reason.clone());
        }
        let mut missing: Vec<String> = Vec::new();
        if self.capture.is_none() {
            missing.push("no microphone recorder was found (install ffmpeg on macOS or arecord on Linux)".to_string());
        }
        if self.recognize.is_none() {
            missing.push("whisper-cli is not in PATH (install the whisper.cpp command-line tool)".to_string());
        }
        if self.model.is_none() {
            missing.push(format!(
                "no ggml model was found in {} (put a ggml-*.bin model from the whisper.cpp project there)",
                models_dir().display()
            ));
        }
        if missing.is_empty() {
            None
        } else {
            Some(format!(
                "local speech is not available: {}. Text input is unchanged.",
                missing.join("; ")
            ))
        }
    }

    /// The sentence that names a missing player, or `None` when playback is
    /// ready.
    pub(crate) fn playback_problem(&self) -> Option<String> {
        if let Some(reason) = &self.unsupported {
            return Some(reason.clone());
        }
        if self.speak.is_none() {
            return Some(
                "local playback is not available: no speech player was found (say on macOS, espeak-ng or espeak on Linux). Text input is unchanged."
                    .to_string(),
            );
        }
        None
    }

    /// One line for `/voice status`.
    pub(super) fn describe(&self) -> String {
        let recognition = match self.recognition_problem() {
            Some(problem) => problem,
            None => "recognition ready".to_string(),
        };
        let playback = match self.playback_problem() {
            Some(problem) => problem,
            None => "playback ready".to_string(),
        };
        format!("{recognition}; {playback}")
    }
}

/// Probe this machine without starting or opening anything.
///
/// Only files are looked at: an executable in one of `dirs`, one model from
/// `models`, and the one player macOS keeps outside PATH. The operating system
/// is the runtime name, so the client needs no per-platform compilation.
pub(crate) fn probe(os: &str, dirs: &[PathBuf], models: &[PathBuf]) -> Capability {
    let dirs = search_dirs(dirs);
    let dirs = dirs.as_slice();
    let model = models.iter().find(|path| usable_model(path.as_path())).cloned();
    let (capture, recognize, speak, unsupported) = match os {
        "macos" => (
            find(&["ffmpeg"], dirs, Flavor::Ffmpeg),
            find(&["whisper-cli"], dirs, Flavor::Whisper),
            find(&["say"], dirs, Flavor::FileSpeaker).or_else(|| absolute("/usr/bin/say", Flavor::FileSpeaker)),
            None,
        ),
        "linux" => (
            find(&["arecord"], dirs, Flavor::Arecord),
            find(&["whisper-cli"], dirs, Flavor::Whisper),
            find(&["espeak-ng", "espeak"], dirs, Flavor::FileSpeaker),
            None,
        ),
        other => (
            None,
            None,
            None,
            Some(format!(
                "local speech is not available on this platform ({other}); text input is unchanged"
            )),
        ),
    };
    Capability {
        capture,
        recognize,
        model,
        speak,
        unsupported,
    }
}

/// The entries that may be searched for a program.
///
/// A relative entry resolves against the client's working directory, so a
/// project could otherwise put its own `ffmpeg` in front of the user's tool.
/// An empty entry means the working directory for the same reason. Only
/// absolute entries are kept.
pub(crate) fn search_dirs(dirs: &[PathBuf]) -> Vec<PathBuf> {
    dirs.iter().filter(|dir| dir.is_absolute()).cloned().collect()
}

/// The first of `names` that exists in one of `dirs`, preferring an earlier
/// name anywhere over a later one.
fn find(names: &[&str], dirs: &[PathBuf], flavor: Flavor) -> Option<Tool> {
    for name in names {
        for dir in dirs {
            let path = dir.join(name);
            if path.is_file() {
                return Some(Tool { path, flavor });
            }
        }
    }
    None
}

/// A program at a fixed path, for the player macOS keeps in `/usr/bin`.
fn absolute(path: &str, flavor: Flavor) -> Option<Tool> {
    let path = PathBuf::from(path);
    if path.is_file() {
        Some(Tool { path, flavor })
    } else {
        None
    }
}

/// Whether `path` is a model file large enough to be one.
fn usable_model(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(metadata) => metadata.is_file() && metadata.len() >= MIN_MODEL_BYTES,
        Err(_) => false,
    }
}

/// Every usable `ggml-*.bin` file in `dir`, sorted for a stable choice.
pub(super) fn model_files(dir: &Path) -> Vec<PathBuf> {
    let mut models = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.starts_with("ggml-") || !name.ends_with(".bin") {
                continue;
            }
            if usable_model(&path) {
                models.push(path);
            }
        }
    }
    models.sort();
    models
}

/// The directory the model files are read from and the message names.
pub(super) fn models_dir() -> PathBuf {
    horon_agent_sdk::user_horon_agent_home().join("models")
}
