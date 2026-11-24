//! Protocol 1.25 wire shape: `get_effective_settings` and its reply.
//!
//! Both are additions. The reply is an allow-list of rows with text values, so
//! the checks here are about what the wire carries: a session id, the row
//! fields, a source that travels as a stable spelling, and an `origin` that is
//! absent rather than empty when the shipped default supplied the value.

use crate::*;

fn row(key: &str, source: SettingSource, origin: Option<&str>) -> EffectiveSetting {
    EffectiveSetting {
        key: key.into(),
        label: "Stream idle timeout (seconds)".into(),
        value: "180".into(),
        source,
        origin: origin.map(str::to_string),
    }
}

#[test]
fn the_request_encodes_the_session_it_asks_about() {
    let request = ClientFrame::new(
        1,
        ApiRequest::GetEffectiveSettings {
            session_id: "s1".into(),
        },
    );

    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        r#"{"v":1,"id":1,"req":"get_effective_settings","session_id":"s1"}"#
    );
}

#[test]
fn the_reply_carries_rows_a_screen_can_print() {
    let reply = ServerFrame::reply(
        1,
        ApiEvent::EffectiveSettings {
            session_id: "s1".into(),
            settings: vec![row(
                "provider.stream_idle_timeout_secs",
                SettingSource::File,
                Some("/home/u/config.toml"),
            )],
        },
    );

    assert_eq!(
        serde_json::to_string(&reply).unwrap(),
        r#"{"v":1,"reply_to":1,"ev":"effective_settings","session_id":"s1","settings":[{"key":"provider.stream_idle_timeout_secs","label":"Stream idle timeout (seconds)","value":"180","source":"file","origin":"/home/u/config.toml"}]}"#
    );
    let decoded: ServerFrame = serde_json::from_str(&serde_json::to_string(&reply).unwrap()).unwrap();
    assert_eq!(decoded.event, reply.event, "the reply round-trips");
}

#[test]
fn a_default_value_carries_no_origin_rather_than_an_empty_one() {
    let reply = ServerFrame::reply(
        1,
        ApiEvent::EffectiveSettings {
            session_id: "s1".into(),
            settings: vec![row("provider.stream_idle_timeout_secs", SettingSource::Default, None)],
        },
    );

    let wire = serde_json::to_value(&reply).unwrap();
    assert_eq!(wire["settings"][0]["source"], "default");
    assert!(
        wire["settings"][0].get("origin").is_none(),
        "a default names no file, so the field is absent: {wire}"
    );
    let back: ServerFrame = serde_json::from_value(wire).unwrap();
    assert_eq!(back.event, reply.event);
}

#[test]
fn the_reply_needs_the_capability_and_an_older_peer_is_refused_by_name() {
    let request = ApiRequest::GetEffectiveSettings {
        session_id: "s1".into(),
    };
    assert_eq!(request.required_capability(), Some(CAPABILITY_EFFECTIVE_SETTINGS_V1));
    assert_eq!(
        request.missing_capability(&[]),
        Some(CAPABILITY_EFFECTIVE_SETTINGS_V1),
        "a peer that never negotiated the read is refused"
    );
    assert_eq!(
        request.missing_capability(&[CAPABILITY_EFFECTIVE_SETTINGS_V1.to_string()]),
        None
    );
}

#[test]
fn a_flag_supplied_value_travels_as_cli_with_the_flag_as_its_origin() {
    let reply = ServerFrame::reply(
        1,
        ApiEvent::EffectiveSettings {
            session_id: "s1".into(),
            settings: vec![row("tools.profile", SettingSource::Cli, Some("--tool-profile"))],
        },
    );

    let wire = serde_json::to_value(&reply).unwrap();
    assert_eq!(wire["settings"][0]["source"], "cli");
    assert_eq!(wire["settings"][0]["origin"], "--tool-profile");
    let back: ServerFrame = serde_json::from_value(wire).unwrap();
    assert_eq!(back.event, reply.event, "a flag source round-trips");
}
