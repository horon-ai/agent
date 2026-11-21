//! On-device speech input and playback for the composer.
//!
//! The client never records, recognizes or speaks by itself: it starts programs
//! the user already installed on this computer. On macOS those are `ffmpeg` for
//! the microphone, `whisper-cli` for recognition and `say` for playback; on
//! Linux they are `arecord`, `whisper-cli` and `espeak-ng` (or `espeak`). The
//! model is a `ggml-*.bin` file under `$HORON_HOME/models`, which the user puts
//! there: this module never downloads a program, a model or a voice.
//!
//! Nothing here uses the network or the harness. Captured audio, the temporary
//! WAV and the recognized text live under `$HORON_HOME/voice` and are removed
//! when the worker is done. What comes back is inserted into the draft and
//! nothing else is done with it: the user's own enter is what sends it.
//!
//! Capture needs the user's consent for the session, given only by
//! `/voice consent`. Consent is held in memory, so leaving the client, moving to
//! another session or losing the connection forgets it; a settings file, a
//! project file or the environment cannot give it, and `/voice forget` takes it
//! back. Until consent is given no microphone is opened and no program is
//! started.
//!
//! A program runs in its own process group and only one voice activity runs at
//! a time. `/voice stop` finishes a capture (its audio is still recognized),
//! `/voice cancel`, `esc`, a prompt arriving, a session switch and leaving the
//! client stop the group and discard the result. A recorder is capped at
//! [`MAX_SECONDS`], a recognition at [`MAX_RECOGNIZE_SECONDS`], a playback at
//! [`MAX_PLAY_SECONDS`], and an old result never reaches a newer draft: every
//! start gets a generation number and a result from an older one is dropped.
//!
//! The entry point is `/voice` (and the `app.voice` key, `ctrl+space` by
//! default). When a program or the model is missing, the client says exactly
//! what is missing and leaves text input untouched; it never starts an
//! install or a download on its own.

use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use horon_agent_sdk::process;

/// Seconds one capture may run before the recorder is stopped.
///
/// The recorder is given the same limit, so it normally ends on its own; the
/// worker's deadline is only the backstop for a program that ignores its own
/// option.
pub(crate) const MAX_SECONDS: u64 = 120;

/// The whisper program waits at most this long for the audio it was given.
const MAX_RECOGNIZE_SECONDS: u64 = 180;

/// The player waits at most this long before it is stopped.
const MAX_PLAY_SECONDS: u64 = 300;

/// How often the worker looks at the running program while it waits.
const POLL: Duration = Duration::from_millis(25);

/// The sample rate every recorder is asked for and the WAV header declares.
const SAMPLE_RATE: u32 = 16_000;

/// Most bytes one capture may produce: 16-bit mono at [`SAMPLE_RATE`].
const MAX_RAW_BYTES: u64 = SAMPLE_RATE as u64 * 2 * MAX_SECONDS;

/// Least audio worth recognizing; anything shorter is a denied microphone or a
/// recorder that never started.
const MIN_AUDIO_BYTES: u64 = SAMPLE_RATE as u64 / 20 * 2;

/// Most bytes read back from the recognizer's text file.
const MAX_TEXT_BYTES: u64 = 64 * 1024;

/// Most characters one recognized draft may add.
const MAX_DRAFT_CHARS: usize = 4000;

/// Most characters one playback may speak.
const MAX_SPEECH_CHARS: usize = 8000;

/// Scratch files older than this are left from a killed process and removed
/// before the next capture.
const STALE_SCRATCH: Duration = Duration::from_secs(60 * 60);

/// What `/voice consent` shows before the first capture.
pub(crate) const CONSENT: &str = "voice: local speech uses this device's microphone and programs installed on this computer. Audio and recognized text stay on this device: nothing is sent to the harness, the model or the network, and the system may ask for microphone access. Denying that leaves text input unchanged. Type /voice consent to allow capture for this session.";

/// What an unknown `/voice` argument shows.
pub(crate) const USAGE: &str =
    "voice: /voice starts or finishes dictation; also consent, forget, stop, cancel, say <text>, read, status";

/// The process group of the program in flight, for the exit path.
///
/// Zero means nothing is running. The terminal's restore path stops this group
/// so a client that leaves does not leave a recorder behind.
static ACTIVE_PID: AtomicU32 = AtomicU32::new(0);

/// The number in the next scratch file's name.
static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

/// Where the voice state is while it is doing something.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// Nothing is captured, recognized or spoken.
    Idle,
    /// The microphone is open and the recorder is running.
    Recording,
    /// The capture ended and the local recognizer is running.
    Transcribing,
    /// A local player is speaking.
    Speaking,
}

impl Phase {
    /// The word `/voice status` and a notice use for this phase.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Recording => "recording",
            Self::Transcribing => "recognizing",
            Self::Speaking => "speaking",
        }
    }
}

/// One worker result on its way back to the app.
#[derive(Debug)]
pub(crate) enum Event {
    /// The recorder finished and the recognizer is about to run.
    Captured { generation: u64 },
    /// The recognizer returned this text.
    Transcribed { generation: u64, text: String },
    /// The activity could not finish.
    Failed { generation: u64, message: String },
    /// The player finished, or failed and names why.
    Spoken { generation: u64, error: Option<String> },
}

/// Why a start request did not begin.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Start {
    /// The activity started.
    Started,
    /// Capture needs `/voice consent` first.
    NeedsConsent,
    /// Something else is already running.
    Busy(Phase),
    /// The capability is missing, with the sentence that names what is missing.
    Unavailable(String),
}

/// What a finished activity became.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Recognized text to insert into the draft, never to send.
    Draft(String),
    /// A line for the transcript, already prefixed by the caller.
    Notice(String),
}

pub(crate) mod capability;

pub(crate) use capability::{Capability, probe};
use capability::{Flavor, Tool, model_files, models_dir};

/// The directory captures and speech files are written to.
fn scratch_dir() -> PathBuf {
    horon_agent_sdk::user_horon_agent_home().join("voice")
}

/// One job the worker thread runs: capture, recognize or speak.
struct Job {
    capture: Option<Tool>,
    recognize: Option<Tool>,
    model: Option<PathBuf>,
    speak: Option<Tool>,
    base: PathBuf,
    generation: u64,
    /// The recorder's pid while it runs, so a stop reaches the recorder and not
    /// the recognizer that follows it.
    recorder: Arc<AtomicU32>,
    /// Set when a stop is asked for: the capture ends and is still recognized.
    finish: Arc<AtomicBool>,
}

/// The session's voice state: consent, the probe, and the one activity.
pub(crate) struct Voice {
    consent: bool,
    capability: Capability,
    /// What is running, for the hint and the keys.
    pub(crate) phase: Phase,
    /// The generation of the current activity; a result from another is stale.
    pub(crate) current: u64,
    cancel: Option<Arc<AtomicBool>>,
    /// The recorder's pid while a capture runs; `/voice stop` reaches this and
    /// nothing else.
    recorder: Option<Arc<AtomicU32>>,
    /// Set by `/voice stop`, so a recorder that has not spawned yet also stops.
    finish: Option<Arc<AtomicBool>>,
    events: Receiver<Event>,
    sender: Sender<Event>,
}

impl Default for Voice {
    fn default() -> Self {
        Self::new()
    }
}

impl Voice {
    /// Probe this machine for the programs and model.
    pub(crate) fn new() -> Self {
        let dirs = match std::env::var_os("PATH") {
            Some(value) => std::env::split_paths(&value).collect::<Vec<PathBuf>>(),
            None => Vec::new(),
        };
        let models = model_files(&models_dir());
        Self::with_capability(probe(std::env::consts::OS, &dirs, &models))
    }

    /// A voice state over a capability already decided.
    pub(crate) fn with_capability(capability: Capability) -> Self {
        let (sender, events) = mpsc::channel();
        Self {
            consent: false,
            capability,
            phase: Phase::Idle,
            current: 0,
            cancel: None,
            recorder: None,
            finish: None,
            events,
            sender,
        }
    }

    /// Whether consent was given for this session.
    pub(crate) fn consent_given(&self) -> bool {
        self.consent
    }

    /// Record consent for the rest of this session.
    pub(crate) fn give_consent(&mut self) {
        self.consent = true;
    }

    /// Take consent back and stop whatever was running.
    pub(crate) fn forget(&mut self) {
        self.consent = false;
        self.abandon();
    }

    /// The phase the hint and the keys read.
    pub(crate) fn phase(&self) -> Phase {
        self.phase
    }

    /// Open the microphone and start the recorder, if everything is ready.
    pub(crate) fn start_recording(&mut self) -> Start {
        if !self.consent {
            return Start::NeedsConsent;
        }
        if self.phase != Phase::Idle {
            return Start::Busy(self.phase);
        }
        if let Some(problem) = self.capability.recognition_problem() {
            return Start::Unavailable(problem);
        }
        let dir = scratch_dir();
        if let Err(error) = fs::create_dir_all(&dir) {
            return Start::Unavailable(format!("cannot create {}: {error}", dir.display()));
        }
        sweep(&dir);
        self.current = self.current.wrapping_add(1);
        let generation = self.current;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        let recorder = Arc::new(AtomicU32::new(0));
        self.recorder = Some(Arc::clone(&recorder));
        let finish = Arc::new(AtomicBool::new(false));
        self.finish = Some(Arc::clone(&finish));
        let job = Job {
            capture: self.capability.capture.clone(),
            recognize: self.capability.recognize.clone(),
            model: self.capability.model.clone(),
            speak: None,
            base: scratch_path(&dir),
            generation,
            recorder,
            finish,
        };
        let sender = self.sender.clone();
        let spawned = std::thread::Builder::new()
            .name("horon-tui-voice".to_string())
            .spawn(move || {
                let attempt = capture_and_recognize(&job, &cancel, &sender);
                match attempt {
                    Attempt::Done(text) => {
                        report_to(&sender, Event::Transcribed { generation, text });
                    }
                    Attempt::Failed(message) => {
                        report_to(&sender, Event::Failed { generation, message });
                    }
                    Attempt::Cancelled => {}
                }
            });
        match spawned {
            Ok(_handle) => {
                self.phase = Phase::Recording;
                Start::Started
            }
            Err(error) => {
                self.cancel = None;
                self.recorder = None;
                self.finish = None;
                Start::Unavailable(format!("cannot start the voice worker: {error}"))
            }
        }
    }

    /// Finish the capture: the recorder is stopped and what it wrote is still
    /// recognized.
    pub(crate) fn stop_recording(&mut self) {
        if self.phase != Phase::Recording {
            return;
        }
        self.phase = Phase::Transcribing;
        // Only the recorder is stopped here. The recognizer may already be the
        // running program, and its result is what this stop is asking for. The
        // flag also catches a recorder that has not published its pid yet.
        if let Some(finish) = &self.finish {
            finish.store(true, Ordering::SeqCst);
        }
        if let Some(recorder) = &self.recorder {
            let pid = recorder.swap(0, Ordering::SeqCst);
            if pid != 0 {
                stop_group(pid);
            }
        }
    }

    /// Speak `text` with the local player, if one is installed.
    pub(crate) fn start_playback(&mut self, text: &str) -> Start {
        if self.phase != Phase::Idle {
            return Start::Busy(self.phase);
        }
        if let Some(problem) = self.capability.playback_problem() {
            return Start::Unavailable(problem);
        }
        let spoken = speech_text(text);
        if spoken.is_empty() {
            return Start::Unavailable("there is no text to speak".to_string());
        }
        let dir = scratch_dir();
        if let Err(error) = fs::create_dir_all(&dir) {
            return Start::Unavailable(format!("cannot create {}: {error}", dir.display()));
        }
        self.current = self.current.wrapping_add(1);
        let generation = self.current;
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(Arc::clone(&cancel));
        let job = Job {
            capture: None,
            recognize: None,
            model: None,
            speak: self.capability.speak.clone(),
            base: scratch_path(&dir),
            generation,
            recorder: Arc::new(AtomicU32::new(0)),
            finish: Arc::new(AtomicBool::new(false)),
        };
        let sender = self.sender.clone();
        let spawned = std::thread::Builder::new()
            .name("horon-tui-voice-play".to_string())
            .spawn(move || match play_text(&job, &spoken, &cancel) {
                None => {}
                Some(Ok(())) => {
                    report_to(
                        &sender,
                        Event::Spoken {
                            generation,
                            error: None,
                        },
                    );
                }
                Some(Err(error)) => {
                    report_to(
                        &sender,
                        Event::Spoken {
                            generation,
                            error: Some(error.into_message()),
                        },
                    );
                }
            });
        match spawned {
            Ok(_handle) => {
                self.phase = Phase::Speaking;
                Start::Started
            }
            Err(error) => {
                self.cancel = None;
                Start::Unavailable(format!("cannot start the voice worker: {error}"))
            }
        }
    }

    /// Stop whatever is running and make its late result irrelevant.
    pub(crate) fn abandon(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.store(true, Ordering::SeqCst);
        }
        self.recorder = None;
        self.finish = None;
        kill_active_process();
        if self.phase != Phase::Idle {
            // A result already on its way carries the old generation and is
            // dropped by `accept`.
            self.current = self.current.wrapping_add(1);
            self.phase = Phase::Idle;
        }
    }

    /// Collect finished activities for the app.
    pub(crate) fn poll(&mut self) -> Vec<Outcome> {
        let mut outcomes = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            if let Some(outcome) = self.accept(event) {
                outcomes.push(outcome);
            }
        }
        outcomes
    }

    /// Apply one result, dropping a stale generation or a phase it cannot
    /// belong to.
    pub(crate) fn accept(&mut self, event: Event) -> Option<Outcome> {
        match event {
            Event::Captured { generation } if generation == self.current && self.phase == Phase::Recording => {
                self.phase = Phase::Transcribing;
                None
            }
            Event::Transcribed { generation, text }
                if generation == self.current && self.phase == Phase::Transcribing =>
            {
                self.phase = Phase::Idle;
                self.cancel = None;
                self.recorder = None;
                self.finish = None;
                Some(Outcome::Draft(text))
            }
            Event::Failed { generation, message }
                if generation == self.current && matches!(self.phase, Phase::Recording | Phase::Transcribing) =>
            {
                self.phase = Phase::Idle;
                self.cancel = None;
                self.recorder = None;
                self.finish = None;
                Some(Outcome::Notice(message))
            }
            Event::Spoken { generation, error } if generation == self.current && self.phase == Phase::Speaking => {
                self.phase = Phase::Idle;
                self.cancel = None;
                self.recorder = None;
                self.finish = None;
                error.map(Outcome::Notice)
            }
            _ => None,
        }
    }

    /// One line for `/voice status`.
    pub(crate) fn status(&self) -> String {
        let consent = if self.consent_given() {
            "consent recorded for this session"
        } else {
            "no consent yet; /voice consent allows capture"
        };
        format!(
            "voice: {}; {consent}; {}",
            self.phase.label(),
            self.capability.describe()
        )
    }
}

impl Drop for Voice {
    fn drop(&mut self) {
        self.abandon();
    }
}

/// Stop the program in flight, for the terminal's exit and signal path.
pub(crate) fn kill_active_process() {
    let pid = ACTIVE_PID.swap(0, Ordering::SeqCst);
    if pid != 0 {
        // The program may already have ended; the client is stopping.
        stop_group(pid);
    }
}

/// Stop a process group, treating a program that already ended as done.
fn stop_group(pid: u32) {
    drop(process::force_stop_process_group(pid));
}

/// Send a result to the app, or drop it when the client is gone.
fn report_to(sender: &Sender<Event>, event: Event) {
    // The app is gone when this fails; the worker has nobody left to report to.
    drop(sender.send(event));
}

/// What one worker attempt produced.
enum Attempt {
    Done(String),
    Failed(String),
    Cancelled,
}

/// Why a local program could not do its work.
///
/// The sentence is already user-facing: the worker passes it to the transcript
/// unchanged, so no caller branches on a cause and nothing is parsed back out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VoiceError(String);

impl VoiceError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// The sentence the transcript shows.
    fn into_message(self) -> String {
        self.0
    }
}

impl std::fmt::Display for VoiceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// The command group of one program while it runs.
///
/// Rust does not stop a child when its handle is dropped, so a panic or an
/// early return between the spawn and the normal stop would leave the recorder
/// holding the microphone.
struct Active<'a> {
    pid: u32,
    armed: bool,
    /// The recorder's slot, set only for the recorder.
    recorder: Option<&'a AtomicU32>,
}

impl<'a> Active<'a> {
    fn new(pid: u32, recorder: Option<&'a AtomicU32>) -> Self {
        ACTIVE_PID.store(pid, Ordering::SeqCst);
        if let Some(slot) = recorder {
            slot.store(pid, Ordering::SeqCst);
        }
        Self {
            pid,
            armed: true,
            recorder,
        }
    }

    /// Clear the slot this guard published, if it is still this program.
    fn clear_recorder(&mut self) {
        if let Some(slot) = self.recorder
            && slot.load(Ordering::SeqCst) == self.pid
        {
            slot.store(0, Ordering::SeqCst);
        }
    }

    /// Clear the record after the program and its children are gone.
    fn disarm(&mut self) {
        self.armed = false;
        self.clear_recorder();
        if ACTIVE_PID.load(Ordering::SeqCst) == self.pid {
            ACTIVE_PID.store(0, Ordering::SeqCst);
        }
    }
}

impl Drop for Active<'_> {
    fn drop(&mut self) {
        if self.armed {
            // The program may have ended already; this guard is unwinding.
            stop_group(self.pid);
        }
        self.clear_recorder();
        if ACTIVE_PID.load(Ordering::SeqCst) == self.pid {
            ACTIVE_PID.store(0, Ordering::SeqCst);
        }
    }
}

/// How a program ended.
enum ChildEnd {
    /// It exited on its own.
    Exited(ExitStatus),
    /// It ran past the deadline and was stopped.
    Stopped,
    /// A cancel was requested and the group was stopped.
    Cancelled,
}

/// Run one program in its own group, waiting at most until `deadline`.
fn run_child(
    tool: &Tool,
    args: &[OsString],
    deadline: Instant,
    cancel: &AtomicBool,
    finish: Option<&AtomicBool>,
    recorder: Option<&AtomicU32>,
) -> Result<ChildEnd, VoiceError> {
    let mut command = Command::new(&tool.path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Its own group, so a stop and the client's exit reach children too.
    process::isolate_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| VoiceError::new(format!("cannot start {}: {error}", tool.path.display())))?;
    let pid = child.id();
    let mut active = Active::new(pid, recorder);
    loop {
        if cancel.load(Ordering::SeqCst) {
            stop_group(pid);
            reap(&mut child);
            active.disarm();
            return Ok(ChildEnd::Cancelled);
        }
        // A stop asked for while the capture runs ends it as if its own limit
        // had been reached: the audio recorded so far is still recognized.
        if finish.is_some_and(|finish| finish.load(Ordering::SeqCst)) {
            stop_group(pid);
            reap(&mut child);
            active.disarm();
            return Ok(ChildEnd::Stopped);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                // A helper the program left behind still shares its group.
                process::kill_process_group(pid);
                active.disarm();
                return Ok(ChildEnd::Exited(status));
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(None) => {
                stop_group(pid);
                reap(&mut child);
                active.disarm();
                return Ok(ChildEnd::Stopped);
            }
            Err(error) => {
                stop_group(pid);
                reap(&mut child);
                active.disarm();
                return Err(VoiceError::new(format!(
                    "cannot wait for {}: {error}",
                    tool.path.display()
                )));
            }
        }
    }
}

/// Reap a program that was stopped, so it does not stay a zombie.
fn reap(child: &mut std::process::Child) {
    // The status is already known not to matter on this path.
    drop(child.wait());
}

/// Capture, then recognize, removing the files it wrote on the way out.
fn capture_and_recognize(job: &Job, cancel: &AtomicBool, sender: &Sender<Event>) -> Attempt {
    let raw = job.base.with_extension("raw");
    let wav = job.base.with_extension("wav");
    let attempt = record_then_recognize(job, &raw, &wav, cancel, sender);
    discard(&raw);
    discard(&wav);
    discard(&job.base.with_extension("txt"));
    attempt
}

/// Capture, then recognize, removing nothing: the caller owns cleanup.
fn record_then_recognize(job: &Job, raw: &Path, wav: &Path, cancel: &AtomicBool, sender: &Sender<Event>) -> Attempt {
    let (Some(capture), Some(recognize), Some(model)) = (&job.capture, &job.recognize, &job.model) else {
        return Attempt::Failed("local speech is not available".to_string());
    };
    let deadline = Instant::now() + Duration::from_secs(MAX_SECONDS + 15);
    let args = recorder_args(capture, raw);
    match run_child(
        capture,
        &args,
        deadline,
        cancel,
        Some(job.finish.as_ref()),
        Some(job.recorder.as_ref()),
    ) {
        // A recorder that was stopped or failed still wrote what it could; the
        // length check below decides whether that is worth recognizing.
        Ok(ChildEnd::Exited(_)) | Ok(ChildEnd::Stopped) => {}
        Ok(ChildEnd::Cancelled) => return Attempt::Cancelled,
        Err(error) => return Attempt::Failed(error.into_message()),
    }
    report_to(
        sender,
        Event::Captured {
            generation: job.generation,
        },
    );
    if cancel.load(Ordering::SeqCst) {
        return Attempt::Cancelled;
    }
    if let Err(error) = write_wav(raw, wav) {
        return Attempt::Failed(error.into_message());
    }
    let recognize_deadline = Instant::now() + Duration::from_secs(MAX_RECOGNIZE_SECONDS);
    match run_child(
        recognize,
        &transcriber_args(model, wav, &job.base),
        recognize_deadline,
        cancel,
        None,
        None,
    ) {
        Ok(ChildEnd::Cancelled) => Attempt::Cancelled,
        Ok(ChildEnd::Stopped) => Attempt::Failed(format!(
            "recognition ran longer than {MAX_RECOGNIZE_SECONDS} s and was stopped"
        )),
        Ok(ChildEnd::Exited(status)) if !status.success() => {
            Attempt::Failed(format!("whisper-cli stopped with {status}"))
        }
        Ok(ChildEnd::Exited(_status)) => match read_recognized(&job.base.with_extension("txt")) {
            Ok(recognized) if recognized.is_empty() => Attempt::Failed("no speech was recognized".to_string()),
            Ok(recognized) => Attempt::Done(recognized),
            Err(error) => Attempt::Failed(error.into_message()),
        },
        Err(error) => Attempt::Failed(error.into_message()),
    }
}

/// The argv for the recorder, in the shape its program expects.
fn recorder_args(tool: &Tool, raw: &Path) -> Vec<OsString> {
    let limit = OsString::from(MAX_SECONDS.to_string());
    let mut args: Vec<OsString> = match tool.flavor {
        Flavor::Ffmpeg => [
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-f",
            "avfoundation",
            "-i",
            ":0",
            "-ar",
            "16000",
            "-ac",
            "1",
            "-t",
        ]
        .iter()
        .copied()
        .map(OsString::from)
        .collect(),
        Flavor::Arecord => ["-q", "-f", "S16_LE", "-r", "16000", "-c", "1", "-t", "raw", "-d"]
            .iter()
            .copied()
            .map(OsString::from)
            .collect(),
        Flavor::Whisper | Flavor::FileSpeaker => Vec::new(),
    };
    args.push(limit);
    if tool.flavor == Flavor::Ffmpeg {
        args.extend(["-f", "s16le", "-y"].iter().copied().map(OsString::from));
    }
    args.push(raw.as_os_str().to_os_string());
    args
}

/// The argv for the recognizer: one model, one WAV, and a text file beside it.
fn transcriber_args(model: &Path, wav: &Path, prefix: &Path) -> Vec<OsString> {
    vec![
        OsString::from("-m"),
        model.as_os_str().to_os_string(),
        OsString::from("-f"),
        wav.as_os_str().to_os_string(),
        OsString::from("-otxt"),
        OsString::from("-nt"),
        OsString::from("-of"),
        prefix.as_os_str().to_os_string(),
    ]
}

/// Put a RIFF header in front of the captured raw PCM.
///
/// The recorder writes raw samples so that stopping it cannot leave a header
/// whose length is wrong; the length is known here, after the process ended.
pub(crate) fn write_wav(raw: &Path, wav: &Path) -> Result<(), VoiceError> {
    let length = match fs::metadata(raw) {
        Ok(metadata) => metadata.len(),
        Err(error) => return Err(VoiceError::new(format!("cannot read the captured audio: {error}"))),
    };
    if length < MIN_AUDIO_BYTES {
        return Err(VoiceError::new(
            "no audio was captured; check the microphone permission for this terminal",
        ));
    }
    let data_len = length.min(MAX_RAW_BYTES);
    let mut input = match fs::File::open(raw) {
        Ok(file) => file,
        Err(error) => return Err(VoiceError::new(format!("cannot read the captured audio: {error}"))),
    };
    let mut output = match fs::File::create(wav) {
        Ok(file) => file,
        Err(error) => return Err(VoiceError::new(format!("cannot create the WAV file: {error}"))),
    };
    if let Err(error) = output.write_all(&wav_header(data_len)) {
        return Err(VoiceError::new(format!("cannot write the WAV header: {error}")));
    }
    let mut limited = Read::by_ref(&mut input).take(data_len);
    if let Err(error) = std::io::copy(&mut limited, &mut output) {
        return Err(VoiceError::new(format!("cannot copy the captured audio: {error}")));
    }
    match output.flush() {
        Ok(()) => Ok(()),
        Err(error) => Err(VoiceError::new(format!("cannot write the WAV file: {error}"))),
    }
}

/// The 44-byte WAV header for `data_len` bytes of 16-bit mono audio.
pub(crate) fn wav_header(data_len: u64) -> [u8; 44] {
    let data = data_len.min(u32::MAX as u64) as u32;
    let byte_rate = SAMPLE_RATE * 2;
    let mut header = [0u8; 44];
    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    header[8..16].copy_from_slice(b"WAVEfmt ");
    header[16..20].copy_from_slice(&16u32.to_le_bytes());
    header[20..22].copy_from_slice(&1u16.to_le_bytes());
    header[22..24].copy_from_slice(&1u16.to_le_bytes());
    header[24..28].copy_from_slice(&SAMPLE_RATE.to_le_bytes());
    header[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    header[32..34].copy_from_slice(&2u16.to_le_bytes());
    header[34..36].copy_from_slice(&16u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&data.to_le_bytes());
    header
}

/// Read the recognizer's text file, bounded, with silence markers dropped.
fn read_recognized(path: &Path) -> Result<String, VoiceError> {
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) => return Err(VoiceError::new(format!("whisper-cli wrote no text: {error}"))),
    };
    let mut bytes = Vec::new();
    let mut limited = Read::by_ref(&mut file).take(MAX_TEXT_BYTES);
    if let Err(error) = limited.read_to_end(&mut bytes) {
        return Err(VoiceError::new(format!("cannot read the recognized text: {error}")));
    }
    Ok(clean_recognition(&String::from_utf8_lossy(&bytes)))
}

/// Join the recognized lines, dropping the annotations whisper writes when it
/// found no speech, and cap the draft one capture may add.
pub(crate) fn clean_recognition(raw: &str) -> String {
    let mut kept = String::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || is_annotation(trimmed) {
            continue;
        }
        let clean: String = trimmed
            .chars()
            .filter(|character| *character == '\t' || !character.is_control())
            .collect();
        let clean = clean.trim();
        if clean.is_empty() {
            continue;
        }
        if !kept.is_empty() {
            kept.push(' ');
        }
        kept.push_str(clean);
    }
    kept.chars().take(MAX_DRAFT_CHARS).collect()
}

/// Whether a whole line is a whisper annotation rather than speech.
fn is_annotation(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "[blank_audio]" | "[music]" | "(music)" | "[silence]" | "(silence)" | "[no speech]" | "[inaudible]"
    )
}

/// The text to speak, with control characters removed and the cap applied.
fn speech_text(raw: &str) -> String {
    let mut clean = String::new();
    for word in raw.split_whitespace() {
        let word: String = word.chars().filter(|character| !character.is_control()).collect();
        if word.is_empty() {
            continue;
        }
        if !clean.is_empty() {
            clean.push(' ');
        }
        clean.push_str(&word);
    }
    clean.chars().take(MAX_SPEECH_CHARS).collect()
}

/// Speak the job's text, returning `None` when it was cancelled.
fn play_text(job: &Job, spoken: &str, cancel: &AtomicBool) -> Option<Result<(), VoiceError>> {
    let Some(speak) = &job.speak else {
        return Some(Err(VoiceError::new("local playback is not available")));
    };
    let speech_file = job.base.with_extension("txt");
    let args = match speak.flavor {
        Flavor::FileSpeaker => {
            if let Err(error) = fs::write(&speech_file, spoken) {
                return Some(Err(VoiceError::new(format!("cannot write the speech text: {error}"))));
            }
            vec![OsString::from("-f"), speech_file.as_os_str().to_os_string()]
        }
        Flavor::Ffmpeg | Flavor::Arecord | Flavor::Whisper => {
            return Some(Err(VoiceError::new("local playback is not available")));
        }
    };
    let deadline = Instant::now() + Duration::from_secs(MAX_PLAY_SECONDS);
    let result = run_child(speak, &args, deadline, cancel, None, None);
    discard(&speech_file);
    match result {
        Ok(ChildEnd::Exited(status)) if status.success() => Some(Ok(())),
        Ok(ChildEnd::Exited(status)) => Some(Err(VoiceError::new(format!("the speech player stopped with {status}")))),
        Ok(ChildEnd::Stopped) => Some(Err(VoiceError::new(format!(
            "the speech player ran longer than {MAX_PLAY_SECONDS} s and was stopped"
        )))),
        Ok(ChildEnd::Cancelled) => None,
        Err(error) => Some(Err(error)),
    }
}

/// The base name for one activity's files, unique within the process.
fn scratch_path(dir: &Path) -> PathBuf {
    let number = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
    let millis = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_millis(),
        Err(_) => 0,
    };
    dir.join(format!("voice-{}-{millis}-{number}", std::process::id()))
}

/// Remove files a killed run left behind, keeping the sweep to this client's
/// own names.
fn sweep(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with("voice-") {
            continue;
        }
        let stale = match entry.metadata().and_then(|metadata| metadata.modified()) {
            Ok(modified) => match SystemTime::now().duration_since(modified) {
                Ok(age) => age > STALE_SCRATCH,
                Err(_) => false,
            },
            Err(_) => false,
        };
        if stale {
            discard(&entry.path());
        }
    }
}

/// Remove one scratch file; a file already gone needs no report.
fn discard(path: &Path) {
    // Nothing is left to tell: the worker has finished with the file.
    drop(fs::remove_file(path));
}
