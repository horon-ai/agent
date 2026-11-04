//! Global push-to-talk without HoronAgent Desktop (Linux).
//!
//! Holding the Copilot key while a HoronAgent CLI terminal is focused records the
//! microphone, transcribes through Nari, and sends the text to that session,
//! exactly like Desktop's CLI capture. Opt-in: needs `[dictation]
//! global_devices` and a Nari key.
//!
//! When HoronAgent Desktop is running it shares the same routing registry
//! (`$XDG_RUNTIME_DIR/horon-agent-global-voice-v1`), and the most recently focused
//! eligible registrant wins a press. The companion registers as eligible only
//! while a HoronAgent CLI window is focused, so Desktop keeps its own windows.
//!
//! Never records without a visible indicator: a desktop notification is shown
//! first, and recording is refused if it cannot be shown.

use horon_agent_voice::global::hotkey::{Edge, Listener};
use horon_agent_voice::{NariEvent, NariRecording, VoiceError};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const IDLE_POLL: Duration = Duration::from_millis(40);
const HELD_POLL: Duration = Duration::from_millis(15);
/// Compositor focus lookups are subprocesses, so refresh them less often.
const FOCUS_REFRESH: Duration = Duration::from_millis(400);
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

pub fn spawn() {
    let devices = horon_agent_base::config::config().dictation.global_devices.clone();
    if devices.is_empty() {
        return;
    }
    if std::env::var("HORON_AGENT_COMPANION_VOICE").as_deref() == Ok("0") {
        return;
    }
    let listener = match Listener::new(&devices) {
        Ok(listener) => listener,
        Err(error) => {
            horon_agent_base::logging::warn(&format!("companion voice unavailable: {error}"));
            eprintln!("horon-agent companion: global voice unavailable: {error}");
            return;
        }
    };
    eprintln!("horon-agent companion: global voice on {} device(s)", devices.len());
    if let Err(error) = std::thread::Builder::new()
        .name("horon-agent-companion-voice".into())
        .spawn(move || run(listener))
    {
        eprintln!("horon-agent companion: cannot start global voice worker: {error}");
    }
}

struct Capture {
    session: String,
    cancel: Arc<AtomicBool>,
    recording: NariRecording,
    indicator: Indicator,
}

fn run(mut listener: Listener) {
    let mut focused: Option<String> = None;
    let mut next_focus = Instant::now();
    let mut capture: Option<Capture> = None;
    loop {
        if capture.is_none() && Instant::now() >= next_focus {
            next_focus = Instant::now() + FOCUS_REFRESH;
            focused = match horon_agent_voice::dictation::focused_cli_session() {
                Ok(session) => session,
                Err(error) => {
                    horon_agent_base::logging::warn(&format!(
                        "companion voice: cannot inspect focused CLI session: {error:#}"
                    ));
                    None
                }
            };
            // Eligible and "focused" only while a HoronAgent CLI is in front, so a
            // focused Desktop window wins its own presses.
            listener.set_eligible(focused.is_some());
            listener.set_active(focused.is_some());
        }
        for edge in listener.poll() {
            match edge {
                Edge::Press | Edge::Tap => {
                    if capture.is_none()
                        && let Some(session) = focused.clone()
                    {
                        capture = start(session);
                    }
                }
                Edge::Release => {
                    if let Some(active) = capture.take() {
                        finish(active);
                    }
                }
                Edge::Cancel => {
                    if let Some(active) = capture.take() {
                        active.cancel.store(true, Ordering::SeqCst);
                        active.indicator.show("Voice cancelled");
                    }
                }
                // Shift+Copilot opens a Desktop voice window. Without
                // Desktop there is nothing to open.
                Edge::Spawn => {}
            }
        }
        std::thread::sleep(if capture.is_some() { HELD_POLL } else { IDLE_POLL });
    }
}

fn start(session: String) -> Option<Capture> {
    if !block_on(horon_agent_voice::global::session::permitted()) {
        eprintln!("horon-agent companion: voice denied by session check");
        return None;
    }
    let Some(key) = horon_agent_voice::nari_api_key() else {
        Indicator::new().show("Voice not configured (NARI_API_KEY)");
        return None;
    };
    // Prove a visible indicator exists before opening the microphone.
    let indicator = Indicator::new();
    if !indicator.show("Listening for HoronAgent…") {
        eprintln!("horon-agent companion: no notification daemon, not recording");
        return None;
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let recorder = horon_agent_base::config::config().dictation.recorder.clone();
    match NariRecording::start_auto(cancel.clone(), &key, &recorder) {
        Ok(recording) => Some(Capture {
            session,
            cancel,
            recording,
            indicator,
        }),
        Err(error) => {
            indicator.show(&failure_label(&error));
            None
        }
    }
}

/// Key-up: stop the microphone, wait for the final text, deliver it.
fn finish(capture: Capture) {
    capture.indicator.show("Transcribing…");
    capture.recording.stop();
    if let Err(error) = std::thread::Builder::new()
        .name("horon-agent-companion-voice-send".into())
        .spawn(move || {
            let result = wait_final(&capture.recording);
            let label = match result {
                Ok(text) if text.trim().is_empty() => "No speech detected".to_string(),
                Ok(text) => {
                    if !block_on(horon_agent_voice::global::session::permitted()) {
                        "Voice denied (session locked)".to_string()
                    } else {
                        match horon_agent_voice::dictation::send_transcript_blocking(
                            text.trim(),
                            horon_agent_base::protocol::TranscriptMode::Send,
                            Some(capture.session.clone()),
                            SEND_TIMEOUT,
                        ) {
                            Ok(()) => "Sent to HoronAgent".to_string(),
                            Err(error) => {
                                eprintln!("horon-agent companion: voice delivery failed: {error:#}");
                                "Couldn't reach HoronAgent".to_string()
                            }
                        }
                    }
                }
                Err(error) => failure_label(&error),
            };
            capture.indicator.show(&label);
        })
    {
        eprintln!("horon-agent companion: cannot start voice delivery worker: {error}");
    }
}

fn wait_final(recording: &NariRecording) -> Result<String, VoiceError> {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        while let Some(event) = recording.try_event() {
            if let NariEvent::Finished(result) = event {
                return result;
            }
        }
        if recording.is_finished() {
            while let Some(event) = recording.try_event() {
                if let NariEvent::Finished(result) = event {
                    return result;
                }
            }
            return Err(VoiceError::CaptureFailed);
        }
        if Instant::now() >= deadline {
            return Err(VoiceError::Timeout);
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

fn failure_label(error: &VoiceError) -> String {
    match error {
        VoiceError::NariNotConfigured => "Voice not configured".into(),
        VoiceError::Timeout => "Transcription timed out".into(),
        VoiceError::MicrophoneUnavailable => "No microphone recorder found".into(),
        other => format!("Voice failed: {other}"),
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    async_io::block_on(future)
}

/// One desktop notification, updated in place (`notify-send -p` / `-r`).
struct Indicator {
    id: std::sync::Mutex<Option<String>>,
}

impl Indicator {
    fn new() -> Self {
        Self {
            id: std::sync::Mutex::new(None),
        }
    }

    /// Show or replace the notification. Returns whether it was displayed.
    fn show(&self, text: &str) -> bool {
        let mut id = self.id.lock().unwrap_or_else(|p| p.into_inner());
        let mut cmd = Command::new("notify-send");
        cmd.args(["-a", "HoronAgent", "-p", "-u", "low", "HoronAgent voice", text]);
        if let Some(previous) = id.as_deref() {
            cmd.args(["-r", previous]);
        }
        let Ok(output) = cmd.stdin(Stdio::null()).stderr(Stdio::null()).output() else {
            return false;
        };
        if !output.status.success() {
            return false;
        }
        let printed = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !printed.is_empty() {
            *id = Some(printed);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_labels_are_short() {
        assert_eq!(failure_label(&VoiceError::NariNotConfigured), "Voice not configured");
        assert_eq!(failure_label(&VoiceError::Timeout), "Transcription timed out");
    }
}
