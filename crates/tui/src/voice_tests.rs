//! Local voice: consent, capability probes, draft insertion and cancellation.
//!
//! Every test either reads files or drives the app state; no test opens a
//! microphone, starts a recognizer, speaks, or reaches the network.

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::{Action, App};
use crate::sdk_bridge::AttachedSession;
use crate::transcript::Item;
use crate::voice::{self, Capability, Event, Outcome, Phase, Voice};

fn app() -> App {
    let (_sender, receiver) = mpsc::channel();
    App::new("test".to_string(), receiver)
}

/// A capability with nothing installed, decided from empty directories.
fn missing(os: &str) -> Capability {
    voice::probe(os, &[], &[])
}

fn last_notice(app: &App) -> String {
    app.transcript
        .items()
        .iter()
        .rev()
        .find_map(|item| match item {
            Item::Notice(text) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn voice_command_is_registered() {
    let mut app = app();
    for ch in "/voice".chars() {
        assert_eq!(app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)), None);
    }
    assert_eq!(app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), None);
    assert_eq!(
        last_notice(&app),
        "voice: local speech uses this device's microphone and programs installed on this computer. Audio and recognized text stay on this device: nothing is sent to the harness, the model or the network, and the system may ask for microphone access. Denying that leaves text input unchanged. Type /voice consent to allow capture for this session."
    );
}

#[test]
fn consent_is_required_before_capture() {
    let mut app = app();
    app.voice = Voice::with_capability(missing("macos"));
    app.input.set_text("keep this");
    app.run_voice("");
    assert_eq!(app.voice.phase(), Phase::Idle);
    assert_eq!(app.input.text(), "keep this");
    assert!(last_notice(&app).contains("consent"));
}

#[test]
fn consent_is_session_state_and_forget_takes_it_back() {
    let mut app = app();
    app.voice = Voice::with_capability(missing("macos"));
    app.run_voice("consent");
    assert!(app.voice.consent_given());
    app.run_voice("forget");
    assert!(!app.voice.consent_given());
    assert_eq!(app.voice.phase(), Phase::Idle);
}

#[test]
fn missing_capability_is_named_and_text_still_sends() {
    let mut app = app();
    app.voice = Voice::with_capability(missing("linux"));
    app.run_voice("consent");
    app.run_voice("");
    assert_eq!(app.voice.phase(), Phase::Idle);
    let notice = last_notice(&app);
    assert!(notice.contains("local speech is not available"), "{notice}");
    assert!(notice.contains("Text input is unchanged"), "{notice}");
    app.input.set_text("plain text still works");
    let action = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(action, Some(Action::Send(text)) if text == "plain text still works"));
}

#[test]
fn playback_names_a_missing_player() {
    let mut app = app();
    app.voice = Voice::with_capability(missing("linux"));
    app.run_voice("say hello");
    assert_eq!(app.voice.phase(), Phase::Idle);
    let notice = last_notice(&app);
    assert!(notice.contains("local playback is not available"), "{notice}");
    assert!(notice.contains("Text input is unchanged"), "{notice}");
}

#[test]
fn capability_probe_reads_files_without_running_them() {
    let bins = tempfile::tempdir().expect("temporary bin directory");
    let models = tempfile::tempdir().expect("temporary model directory");
    let marker = bins.path().join("ran");
    let script = format!("#!/bin/sh\ntouch '{}'\n", marker.display());
    for name in ["ffmpeg", "whisper-cli", "say"] {
        fs::write(bins.path().join(name), &script).expect("fixture program");
    }
    let model = models.path().join("ggml-tiny.en.bin");
    fs::write(&model, vec![0u8; 1024 * 1024 + 1]).expect("fixture model");
    let capability = voice::probe("macos", &[PathBuf::from(bins.path())], &[model]);
    assert!(capability.recognition_problem().is_none());
    assert!(capability.playback_problem().is_none());
    // The probe looked at the files only: if it had run any of them, the
    // marker the scripts share would exist now.
    assert!(!marker.exists(), "the capability probe started a program");
}

#[test]
fn recognized_text_is_inserted_into_the_draft_and_not_sent() {
    let mut app = app();
    app.input.set_text("keep this");
    app.voice.phase = Phase::Transcribing;
    app.voice.current = 1;
    let outcome = app.voice.accept(Event::Transcribed {
        generation: 1,
        text: "hello from voice".to_string(),
    });
    assert_eq!(outcome, Some(Outcome::Draft("hello from voice".to_string())));
    if let Some(outcome) = outcome {
        app.apply_voice(outcome);
    }
    assert_eq!(app.input.text(), "keep this hello from voice");
    assert!(app.in_flight.is_none());
    assert!(app.queue.is_empty());
    let notice = last_notice(&app);
    assert!(notice.contains("inserted into the draft (not sent)"), "{notice}");
}

#[test]
fn a_late_result_does_not_change_a_newer_draft() {
    let mut app = app();
    app.voice.phase = Phase::Transcribing;
    app.voice.current = 7;
    app.cancel_voice();
    assert_eq!(app.voice.phase(), Phase::Idle);
    app.input.set_text("kept");
    let outcome = app.voice.accept(Event::Transcribed {
        generation: 7,
        text: "stale words".to_string(),
    });
    assert_eq!(outcome, None);
    assert!(!app.tick_voice());
    assert_eq!(app.input.text(), "kept");
}

#[test]
fn escape_cancels_a_capture_without_touching_the_draft() {
    let mut app = app();
    app.voice.phase = Phase::Recording;
    app.voice.current = 3;
    app.input.set_text("keep this");
    let action = app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(action.is_none());
    assert_eq!(app.voice.phase(), Phase::Idle);
    assert_eq!(app.input.text(), "keep this");
    assert!(last_notice(&app).contains("cancelled"));
}

#[test]
fn wav_header_declares_the_captured_length() {
    let header = voice::wav_header(3200);
    assert_eq!(&header[0..4], b"RIFF");
    assert_eq!(&header[8..16], b"WAVEfmt ");
    assert_eq!(&header[36..40], b"data");
    assert_eq!(
        u32::from_le_bytes([header[4], header[5], header[6], header[7]]),
        36 + 3200
    );
    assert_eq!(
        u32::from_le_bytes([header[40], header[41], header[42], header[43]]),
        3200
    );
    assert_eq!(
        u32::from_le_bytes([header[24], header[25], header[26], header[27]]),
        16_000
    );
}

#[test]
fn silence_annotations_do_not_reach_the_draft() {
    assert_eq!(
        voice::clean_recognition("[BLANK_AUDIO]\n\nhello there\n(MUSIC)\n"),
        "hello there"
    );
}

#[test]
fn relative_and_empty_path_entries_are_not_searched() {
    let first = std::env::temp_dir();
    let second = first.join("voice-search-test");
    let entries = vec![
        PathBuf::new(),
        PathBuf::from("."),
        PathBuf::from("bin"),
        first.clone(),
        second.clone(),
    ];
    assert_eq!(crate::voice::capability::search_dirs(&entries), vec![first, second]);
}

#[test]
fn switching_sessions_forgets_consent() {
    let mut app = app();
    app.voice = Voice::with_capability(missing("macos"));
    app.run_voice("consent");
    assert!(app.voice.consent_given());
    app.attach_session(
        AttachedSession {
            description: "socket · session_b".to_string(),
            session_id: "session_b".to_string(),
            status: "idle".to_string(),
            model: None,
            effort: None,
            history: Vec::new(),
            title: None,
        },
        false,
    );
    assert!(!app.voice.consent_given());
    assert_eq!(app.voice.phase(), Phase::Idle);
}

#[test]
fn losing_the_connection_forgets_consent() {
    let mut app = app();
    app.voice = Voice::with_capability(missing("macos"));
    app.run_voice("consent");
    assert!(app.voice.consent_given());
    app.disconnected = true;
    app.tick_voice();
    assert!(!app.voice.consent_given());
}
