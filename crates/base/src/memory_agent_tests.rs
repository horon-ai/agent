use super::*;

#[test]
fn extraction_transcript_omits_internal_system_reminders() {
    let messages = vec![
        crate::message::Message::user("<system-reminder>\n# Session Context\nHardware: private\n</system-reminder>"),
        crate::message::Message::user("Remember that tests use a temporary database."),
        crate::message::Message::assistant_text("Understood."),
    ];
    let transcript = build_transcript_for_extraction(&messages);
    assert!(!transcript.contains("Hardware: private"));
    assert!(transcript.contains("tests use a temporary database"));
    assert!(transcript.contains("Understood"));
}

struct TestEnv(Vec<(&'static str, Option<std::ffi::OsString>)>);
impl TestEnv {
    fn set(values: &[(&'static str, &str)]) -> Self {
        let old = values
            .iter()
            .map(|(key, value)| {
                let old = std::env::var_os(key);
                crate::env::set_var(key, value);
                (*key, old)
            })
            .collect();
        Self(old)
    }
}
impl Drop for TestEnv {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..) {
            match value {
                Some(value) => crate::env::set_var(key, value),
                None => crate::env::remove_var(key),
            }
        }
    }
}

#[tokio::test]
async fn missing_jev_access_clears_pending_without_fallback() {
    struct RestoreHome(Option<std::ffi::OsString>);
    impl Drop for RestoreHome {
        fn drop(&mut self) {
            memory::clear_all_pending_memory();
            match self.0.take() {
                Some(home) => crate::env::set_var("HORON_HOME", home),
                None => crate::env::remove_var("HORON_HOME"),
            }
        }
    }

    let _lock = crate::storage::lock_test_env();
    let _env = TestEnv::set(&[("HORON_AGENT_MEMORY_JEV_PROVIDER", "disabled-for-test")]);
    let home = tempfile::tempdir().expect("isolated memory home");
    let _restore = RestoreHome(std::env::var_os("HORON_HOME"));
    crate::env::set_var("HORON_HOME", home.path());

    let sid = "jev-agent-no-fallback";
    let entry = memory::MemoryEntry::new(memory::MemoryCategory::Fact, "stale result");
    memory::MemoryManager::new()
        .remember_global(entry.clone())
        .expect("persist the queued memory");
    memory::set_pending_memory(
        sid,
        memory::format_relevant_prompt(std::slice::from_ref(&entry), 1).expect("canonical memory prompt"),
        1,
        vec![entry.id.clone()],
        None,
        None,
    );
    assert!(memory::has_pending_memory(sid));

    let (_, rx) = mpsc::channel(1);
    let mut agent = MemoryAgent::new(rx);
    agent
        .process_context(sid, &[crate::message::Message::user("current query")])
        .await
        .unwrap();
    assert!(!memory::has_pending_memory(sid));
}

#[test]
fn manager_without_working_dir_does_not_infer_process_project() {
    let manager = manager_for_working_dir(None);
    assert!(manager.load_project_graph().unwrap().memories.is_empty());
}
