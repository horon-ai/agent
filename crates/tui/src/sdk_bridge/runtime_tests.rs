//! The bridge's runtime report: what it carries, what a blank effort reads as,
//! and which answers are dropped instead of drawn.
//!
//! Nothing here talks to a daemon: each part is one SDK call in the caller, and
//! what those calls feed into — the fields the status line reads and the level
//! a `/effort` confirms — is what these tests pin.

use horon_agent_sdk::RuntimeInfo;

use super::Incoming;
use super::runtime::{effort, reported};

/// A runtime report of the session named, with nothing else known.
fn info(session_id: &str, effort: Option<&str>) -> RuntimeInfo {
    RuntimeInfo {
        server: "horon-agent".to_string(),
        protocol_version: 1,
        capabilities: Vec::new(),
        healthy: true,
        session_id: session_id.to_string(),
        provider: Some("mock".to_string()),
        model: Some("mock-model".to_string()),
        reasoning_effort: effort.map(str::to_string),
        auth_method: None,
        fallback_models: Vec::new(),
        serving_model: None,
        providers: vec!["mock".to_string()],
        routes: Vec::new(),
    }
}

#[test]
fn a_padded_effort_is_trimmed_and_a_blank_one_is_unknown() {
    assert_eq!(effort(Some("  high ".to_string())).as_deref(), Some("high"));
    assert_eq!(effort(Some("   ".to_string())), None);
    assert_eq!(effort(Some(String::new())), None);
    assert_eq!(effort(None), None);
}

#[test]
fn a_report_of_the_session_the_worker_follows_names_what_the_status_line_shows() {
    let answer = reported(info("s1", Some("high")), "s1", None);
    match answer {
        Some(Incoming::RuntimeInfo {
            provider,
            model,
            effort,
            level,
        }) => {
            assert_eq!(provider.as_deref(), Some("mock"));
            assert_eq!(model.as_deref(), Some("mock-model"));
            assert_eq!(effort.as_deref(), Some("high"));
            assert_eq!(level, None, "a plain report confirmed a level nobody asked for");
        }
        other => panic!("expected a runtime report, got {other:?}"),
    }
}

#[test]
fn a_report_for_another_session_is_dropped() {
    // The daemon rewrites a typed prefix to the full id: an answer that
    // describes a session the worker has left must never put its model beside
    // the conversation on screen.
    assert_eq!(reported(info("session_other", Some("high")), "s1", None), None);
}

#[test]
fn a_confirmed_level_travels_with_the_answer() {
    let answer = reported(info("s1", None), "s1", Some("xhigh"));
    match answer {
        Some(Incoming::RuntimeInfo { effort, level, .. }) => {
            assert_eq!(level.as_deref(), Some("xhigh"));
            assert_eq!(
                effort.as_deref(),
                Some("xhigh"),
                "a harness that reports no effort lost the level it just stored"
            );
        }
        other => panic!("expected a runtime report, got {other:?}"),
    }
}

#[test]
fn a_reported_effort_wins_over_the_level_that_asked_for_it() {
    // A provider may hold a different name for the level it was given; the
    // status line shows what the harness says it holds, not what was typed.
    let answer = reported(info("s1", Some("medium")), "s1", Some("high"));
    match answer {
        Some(Incoming::RuntimeInfo { effort, .. }) => assert_eq!(effort.as_deref(), Some("medium")),
        other => panic!("expected a runtime report, got {other:?}"),
    }
}

#[test]
fn an_empty_effort_from_the_harness_is_not_shown_as_a_level() {
    let answer = reported(info("s1", Some("")), "s1", None);
    match answer {
        Some(Incoming::RuntimeInfo { effort, .. }) => assert_eq!(effort, None),
        other => panic!("expected a runtime report, got {other:?}"),
    }
}
