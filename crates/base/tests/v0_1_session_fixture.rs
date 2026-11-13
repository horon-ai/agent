//! A session recorded by v0.1.x still loads.
//!
//! v0.1.x saved one snapshot per session at `sessions/<id>.json`, holding the
//! fields that release required (`id`, `parent_id`, `title`, `created_at`,
//! `updated_at`) plus the optional ones it emitted when they were set. The
//! record below is exactly that shape: every field added after v0.1.x is absent,
//! which is how a file written by that release looks.
//!
//! `plugin_id` is present on purpose. v0.1.x still wrote the native plugin
//! backend id, so a real v0.1.x session can carry `"plugin_id": "router"`; the
//! current reader must accept it and treat it as inert rather than refuse the
//! session. `cache_root_id`, `reasoning_effort_pinned` and the event-store
//! lineage do not appear because they did not exist yet.

use horon_agent_base::message::Role;
use horon_agent_base::session::Session;

const SESSION_ID: &str = "session_v0_1_fixture";

/// A throwaway home for one test.
///
/// The session store refuses a directory that is not private to its owner
/// (`crates/session-store/src/blobs.rs`, `ensure_dir`), and `std::fs::create_dir_all`
/// under a shared temp root leaves the default 0755 mode. The fixture therefore
/// creates the directory through the platform helper that sets the owner-only
/// mode the store expects.
fn temp_home() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "v0-1-session-fixture-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    horon_agent_platform::create_private_dir(&dir, true).unwrap();
    dir
}

fn v0_1_record(session_id: &str) -> String {
    format!(
        r#"{{
  "id": "{session_id}",
  "parent_id": null,
  "title": "Login request times out",
  "custom_title": "Timeout fix",
  "created_at": "2025-10-01T09:12:03.412Z",
  "updated_at": "2025-10-01T09:15:44.090Z",
  "messages": [
    {{
      "id": "msg-1",
      "role": "user",
      "content": [{{"type": "text", "text": "the login request times out after 30s"}}],
      "timestamp": "2025-10-01T09:12:03.412Z"
    }},
    {{
      "id": "msg-2",
      "role": "assistant",
      "content": [{{"type": "text", "text": "Raising the client timeout."}}],
      "timestamp": "2025-10-01T09:12:05.020Z"
    }}
  ],
  "provider_key": "openai",
  "plugin_id": "router",
  "model": "gpt-5.2-codex",
  "route_api_method": "openai-api",
  "working_dir": "/home/dev/app",
  "short_name": "oak",
  "status": "Closed",
  "last_pid": null,
  "last_active_at": "2025-10-01T09:15:43.000Z",
  "is_debug": false,
  "saved": true,
  "save_label": "timeout",
  "migration_epoch": 0
}}"#
    )
}

#[test]
fn session_written_by_v0_1_x_loads_with_its_transcript() {
    let home = temp_home();
    unsafe { std::env::set_var("HORON_HOME", &home) };

    let sessions = home.join("sessions");
    horon_agent_platform::create_private_dir(&sessions, true).unwrap();
    std::fs::write(sessions.join(format!("{SESSION_ID}.json")), v0_1_record(SESSION_ID)).unwrap();

    let loaded = Session::load(SESSION_ID).unwrap();

    assert_eq!(loaded.id, SESSION_ID);
    assert_eq!(loaded.title.as_deref(), Some("Login request times out"));
    assert_eq!(loaded.custom_title.as_deref(), Some("Timeout fix"));
    assert_eq!(loaded.model.as_deref(), Some("gpt-5.2-codex"));
    assert_eq!(loaded.route_api_method.as_deref(), Some("openai-api"));
    assert_eq!(loaded.short_name.as_deref(), Some("oak"));
    // The v0.1.x plugin backend id is carried through untouched and inert.
    assert_eq!(loaded.plugin_id.as_deref(), Some("router"));

    let messages = loaded.active_context();
    assert_eq!(messages.len(), 2, "v0.1.x transcript must load whole");
    assert_eq!(messages[0].id, "msg-1");
    assert_eq!(messages[0].role, Role::User);
    assert_eq!(messages[1].id, "msg-2");
    assert_eq!(messages[1].role, Role::Assistant);
    assert!(messages[1].content.iter().any(|block| matches!(
        block,
        horon_agent_base::message::ContentBlock::Text { text, .. } if text == "Raising the client timeout."
    )));

    let _ = std::fs::remove_dir_all(&home);
}
