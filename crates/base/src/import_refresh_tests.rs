use super::refresh::PreparedTakeover;
use super::tests::EnvVarGuard;
use super::*;
use serde_json::{Value, json};
use std::io::Write;

const SOURCE: &str = "claude-live";
const TARGET: &str = "session_takeover_refresh";

fn row(role: &str, uuid: Option<&str>, parent: Option<&str>, text: &str) -> Value {
    json!({"type":role, "sessionId":SOURCE, "uuid":uuid, "parentUuid":parent,
           "message":{"role":role, "content":text}})
}

fn original() -> [Value; 2] {
    [
        row("user", Some("u1"), None, "question"),
        row("assistant", Some("a1"), Some("u1"), "answer"),
    ]
}

fn write_rows(path: &Path, rows: &[Value]) -> Result<()> {
    let mut file = File::create(path)?;
    for entry in rows {
        serde_json::to_writer(&mut file, entry)?;
        writeln!(file)?;
    }
    Ok(())
}

fn with_staging(rows: &[Value], check: impl FnOnce(&Path, PreparedTakeover) -> Result<()>) -> Result<()> {
    let _guard = crate::storage::lock_test_env();
    let home = tempfile::tempdir()?;
    let _home = EnvVarGuard::set_path("HORON_HOME", home.path());
    let path = home.path().join("claude.jsonl");
    write_rows(&path, rows)?;
    check(&path, PreparedTakeover::prepare(&path, SOURCE, TARGET.to_owned())?)
}

fn log_bytes(staged: &PreparedTakeover) -> Result<Vec<u8>> {
    Ok(std::fs::read(staged.session.event_log_path()?)?)
}

#[test]
fn shutdown_flush_preserves_the_event_chain_and_anonymous_message_ids() -> Result<()> {
    let mut rows = original().to_vec();
    rows.push(row("assistant", None, Some("a1"), "same text"));
    with_staging(&rows, |path, mut staged| {
        let before = log_bytes(&staged)?;
        let saved_id = staged.session.active_context()[2].id.clone();
        staged.refresh(path, SOURCE)?;
        assert_eq!(log_bytes(&staged)?, before);

        let mut flushed = rows.clone();
        flushed.push(row("assistant", None, Some("a1"), "same text"));
        write_rows(path, &flushed)?;
        staged.refresh(path, SOURCE)?;
        let after = log_bytes(&staged)?;
        assert!(after.starts_with(&before));
        let history = Session::load(TARGET)?.history_messages()?;
        assert_eq!(
            history.iter().map(StoredMessage::content_preview).collect::<Vec<_>>(),
            ["question", "answer", "same text", "same text"]
        );
        assert_eq!(history[2].id, saved_id);
        assert_ne!(history[3].id, saved_id);
        Ok(())
    })
}

#[test]
fn changes_to_the_staged_conversation_preserve_its_original_log() -> Result<()> {
    let [user, assistant] = original();
    with_staging(&[user.clone(), assistant.clone()], |path, mut staged| {
        let before = log_bytes(&staged)?;
        let extra = row("user", Some("u2"), None, "inserted");
        let mut unrelated = extra.clone();
        unrelated["sessionId"] = json!("another-session");
        for changed in [
            vec![user.clone()],
            vec![assistant.clone(), user.clone()],
            vec![user.clone(), extra, assistant.clone()],
            vec![user.clone(), row("assistant", Some("a1"), Some("u1"), "edited")],
            vec![user.clone(), row("assistant", Some("a2"), Some("u1"), "answer")],
            vec![user.clone(), assistant.clone(), unrelated],
            vec![user.clone(), assistant.clone(), assistant.clone()],
        ] {
            write_rows(path, &changed)?;
            assert!(
                staged.refresh(path, SOURCE).is_err(),
                "accepted changed rows {changed:?}"
            );
            assert_eq!(log_bytes(&staged)?, before);
        }
        // A timestamped root sorts before the two staged messages.
        let mut earlier = row("user", Some("early"), None, "chronologically first");
        earlier["timestamp"] = json!("2026-01-01T00:00:00Z");
        write_rows(path, &[user.clone(), assistant.clone(), earlier])?;
        assert!(staged.refresh(path, SOURCE).is_err());
        assert_eq!(log_bytes(&staged)?, before);
        Ok(())
    })
}

#[test]
fn final_refresh_retries_the_reserved_tail_after_a_sync_failure() -> Result<()> {
    let original = original();
    with_staging(&original, |path, mut staged| {
        let mut rows = original.to_vec();
        rows.push(row("assistant", None, Some("a1"), "shutdown reply"));
        write_rows(path, &rows)?;
        // A hook is installed before opening a writer. Reopen at the exact
        // staged head; both save attempts below use that one facade.
        staged.session = Session::load(TARGET)?;
        let mut fail = true;
        staged.session.set_event_io_hook(Box::new(move |point| {
            if point == horon_agent_session_store::IoPoint::LogDataSync && std::mem::take(&mut fail) {
                return Err(std::io::Error::other("injected shutdown sync failure"));
            }
            Ok(())
        }))?;
        assert!(staged.refresh(path, SOURCE).is_err());
        let reserved = staged.session.pending_event_ids();
        assert!(!reserved.is_empty());
        staged.refresh(path, SOURCE)?;
        let mut events = Vec::new();
        for line in BufReader::new(File::open(staged.session.event_log_path()?)?).lines() {
            events.push(serde_json::from_str::<Value>(&line?)?);
        }
        for id in reserved {
            assert_eq!(events.iter().filter(|event| event["event_id"] == id).count(), 1);
        }
        let history = Session::load(TARGET)?.history_messages()?;
        assert_eq!(history.len(), 3);
        assert_eq!(history[2].content_preview(), "shutdown reply");
        Ok(())
    })
}

#[test]
fn a_concurrent_horon_reply_remains_durable_on_every_refresh_attempt() -> Result<()> {
    let original = original();
    with_staging(&original, |path, mut staged| {
        let mut writer = Session::load(TARGET)?;
        append_text_message(&mut writer, Role::Assistant, "Horon reply".into(), None);
        writer.save()?;
        let before = log_bytes(&staged)?;
        let mut rows = original.to_vec();
        rows.push(row("assistant", Some("a2"), Some("a1"), "Claude reply"));
        write_rows(path, &rows)?;
        for _ in 0..2 {
            let error = staged.refresh(path, SOURCE).expect_err("stale writer");
            assert!(error.to_string().contains("SourceConflict"), "{error:#}");
        }
        assert_eq!(log_bytes(&staged)?, before);
        assert_eq!(
            Session::load(TARGET)?.history_messages()?[2].content_preview(),
            "Horon reply"
        );
        Ok(())
    })
}

#[test]
fn a_bounded_initial_import_appends_only_the_normalized_shutdown_tail() -> Result<()> {
    let rows = (0..IMPORT_HISTORY_MAX_MESSAGES + 3)
        .map(|index| row("user", None, None, &index.to_string()))
        .collect::<Vec<_>>();
    with_staging(&rows, |path, mut staged| {
        let saved = staged.session.active_context();
        assert!(saved[0].content_preview().contains("3 older messages were omitted"));
        let before = log_bytes(&staged)?;
        let mut flushed = rows.clone();
        let mut tool = row("assistant", Some("tool"), None, "");
        tool["message"]["content"] = json!([{"type":"tool_use","id":"call","name":"read","input":{"path":"a"}}]);
        flushed.push(tool);
        write_rows(path, &flushed)?;
        staged.refresh(path, SOURCE)?;
        let history = Session::load(TARGET)?.history_messages()?;
        assert_eq!(history.len(), IMPORT_HISTORY_MAX_MESSAGES + 2);
        assert!(
            history
                .last()
                .unwrap()
                .content_preview()
                .contains("[Imported tool call: read]")
        );
        assert!(log_bytes(&staged)?.starts_with(&before));
        Ok(())
    })
}
