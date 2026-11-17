//! Frozen past records: the hashed time field and the event hash may not drift.
//!
//! `content_hash` covers the canonical timestamp text, so any change to how a
//! stored instant is parsed or spelled invalidates every recorded hash. These
//! samples are the exact bytes a session log on this machine already holds; the
//! hash in each line is what the pre-swap writer computed over them.

use horon_agent_session_store::*;
use horon_agent_time::{Precision, Timestamp};
use serde_json::json;

/// Verbatim lines from `crates/harness-api-server/tests/fixtures/history-page-log-500/events.jsonl`,
/// a 500-record log written by the chrono-based writer. The session root carries
/// nested `created_at`/`updated_at` metadata; the two message records carry a
/// nested message timestamp, a parent id and nanosecond event times.
const PAST_RECORDS: &[&str] = &[
    r#"{"content_hash":"ff75a31582fa013906241f32af75445b7679b14dd9098b5564ac0940bb796877","event_id":"d90fd7d1-1892-568b-89e4-12d2f3988a5e","kind":"session_created","origin_id":"4a50bc72-f869-56ca-8708-3451266e78d6","payload":{"branch_root":false,"metadata":{"created_at":"2026-01-01T00:00:00.000000000Z","migration_epoch":0,"model":"gpt-5.1-codex","provider_key":"openai","saved":true,"title":"perf fixture event log perf-events-500","updated_at":"2026-01-01T00:00:00.000000000Z","working_dir":"/tmp/horon-perf-fixture"}},"schema_version":1,"session_id":"perf-events-500","timestamp":"2026-01-01T00:00:00.000000000Z"}"#,
    r#"{"content_hash":"04626efeb743bc4b2f36b9d0e791bfdee6dbe57debfb106023cb2c120c6c1bcc","event_id":"3b287576-2bd7-5590-9abc-402833b91d15","kind":"message_appended","origin_id":"4a50bc72-f869-56ca-8708-3451266e78d6","parent_event_id":"d90fd7d1-1892-568b-89e4-12d2f3988a5e","payload":{"message":{"content":[{"text":"fixture message 1: yankee yankee sierra oscar juliet fixture bravo zulu alpha zulu quebec","type":"text"}],"id":"message_1767225600001_13666762375814655075","role":"user","timestamp":"2026-01-01T00:00:00.001000000Z"}},"schema_version":1,"session_id":"perf-events-500","timestamp":"2026-01-01T00:00:00.001000000Z"}"#,
    r#"{"content_hash":"39b4d9307f91c364de2860a065f8c6826b13fa4f4ca6ab2bc5e39f95b2ebf187","event_id":"afb68e29-2f7f-5f57-9977-f07f207531ad","kind":"message_appended","origin_id":"4a50bc72-f869-56ca-8708-3451266e78d6","parent_event_id":"f431414f-d210-574b-ac22-6ca33b669f8e","payload":{"message":{"content":[{"text":"fixture message 499: bravo mike yankee store log tango delta uniform victor papa golf november","type":"text"}],"id":"message_1767225600499_6549051912535767460","role":"user","timestamp":"2026-01-01T00:00:00.499000000Z"}},"schema_version":1,"session_id":"perf-events-500","timestamp":"2026-01-01T00:00:00.499000000Z"}"#,
];

#[test]
fn past_records_keep_their_hashed_bytes_and_recorded_hash() {
    for line in PAST_RECORDS {
        let envelope: EventEnvelope = serde_json::from_str(line).expect("past record parses");
        // The recorded hash is the whole check: it was computed over the old
        // writer's canonical timestamp text.
        envelope.verify_hash().expect("recorded content_hash still matches");
        // Byte identity covers what the hash cannot: field spelling and order.
        assert_eq!(envelope.canonical_json().expect("canonical json"), *line);
    }
}

#[test]
fn equal_instants_hash_the_same_whatever_fraction_was_stored() {
    // The same second, written at four precisions. Every one must canonicalize
    // to the fixed nanosecond form before hashing.
    let spellings = [
        "2026-01-01T00:00:00Z",
        "2026-01-01T00:00:00.000Z",
        "2026-01-01T00:00:00.000000Z",
        "2026-01-01T00:00:00.000000000Z",
    ];
    let canonical = "2026-01-01T00:00:00.000000000Z";
    for spelling in spellings {
        let parsed: Timestamp = serde_json::from_value(json!(spelling)).expect("timestamp parses");
        assert_eq!(canonical_timestamp(&parsed), canonical, "for {spelling}");
    }
}

#[test]
fn fraction_precision_survives_parsing_to_the_nanosecond() {
    for (input, canonical) in [
        ("2024-05-06T07:08:09Z", "2024-05-06T07:08:09.000000000Z"),
        ("2024-05-06T07:08:09.123Z", "2024-05-06T07:08:09.123000000Z"),
        ("2024-05-06T07:08:09.123456Z", "2024-05-06T07:08:09.123456000Z"),
        ("2024-05-06T07:08:09.123456789Z", "2024-05-06T07:08:09.123456789Z"),
    ] {
        let parsed: Timestamp = input.parse().expect("timestamp parses");
        assert_eq!(canonical_timestamp(&parsed), canonical, "hashed form of {input}");
        // The stored (unhashed) form keeps the automatic precision, so a record
        // read and written again is byte-identical to what was stored.
        assert_eq!(
            parsed.to_rfc3339_opts(Precision::Auto, true),
            input,
            "stored form of {input}"
        );
    }
}

#[test]
fn offset_inputs_normalize_to_utc_before_hashing() {
    // An offset spelling is the same instant as its UTC form, so it must hash to
    // the UTC text rather than keep a local wall clock.
    let parsed: Timestamp = serde_json::from_value(json!("2019-03-01T12:00:00+09:00")).expect("offset parses");
    assert_eq!(canonical_timestamp(&parsed), "2019-03-01T03:00:00.000000000Z");
    assert_eq!(
        serde_json::to_string(&parsed).expect("serde"),
        "\"2019-03-01T03:00:00Z\""
    );
    assert_eq!(parsed, Timestamp::from_timestamp(1_551_409_200, 0).expect("instant"));
}

#[test]
fn leap_second_input_keeps_the_sixtieth_second() {
    // chrono printed `:60` and kept the leaping second; the stored nanosecond
    // field carries it (`1_000_000_000`), so a record read and rewritten keeps
    // the same text and the same hash.
    let parsed: Timestamp = "2016-12-31T23:59:60Z".parse().expect("leap second parses");
    assert_eq!(parsed.timestamp_subsec_nanos(), 1_000_000_000);
    assert_eq!(canonical_timestamp(&parsed), "2016-12-31T23:59:60.000000000Z");
    assert_eq!(parsed.to_rfc3339_opts(Precision::Auto, true), "2016-12-31T23:59:60Z");
    // The same text with a fraction is a leaping second too.
    let fractional: Timestamp = "2016-12-31T23:59:60.500Z"
        .parse()
        .expect("fractional leap second parses");
    assert_eq!(canonical_timestamp(&fractional), "2016-12-31T23:59:60.500000000Z");
}

#[test]
fn extended_years_written_by_the_formatter_parse_back() {
    // The stored range reaches past year 9999, where RFC 3339 requires a signed
    // year. Whatever the writer emits has to be readable again, or a record it
    // hashed could never be verified.
    let far = Timestamp::from_timestamp(320_000_000_000, 0).expect("a year past 9999 is representable");
    let text = far.to_rfc3339_opts(Precision::Nanos, true);
    assert!(text.starts_with('+'), "signed year form, got {text}");
    assert_eq!(text.parse::<Timestamp>().expect("the writer's own text parses"), far);
}

#[test]
fn millisecond_boundary_matches_the_message_header_form() {
    // ACP and message headers use millisecond precision with `Z`.
    let parsed = Timestamp::from_timestamp(1_714_979_289, 123_000_000).expect("instant");
    assert_eq!(
        parsed.to_rfc3339_opts(Precision::Millis, true),
        "2024-05-06T07:08:09.123Z"
    );
    assert_eq!(parsed.timestamp(), 1_714_979_289);
    assert_eq!(parsed.timestamp_millis(), 1_714_979_289_123);
    assert_eq!(parsed.timestamp_subsec_nanos(), 123_000_000);
    // The default display form is the stored one: automatic precision, `+00:00`.
    assert_eq!(parsed.to_rfc3339(), "2024-05-06T07:08:09.123+00:00");
}
