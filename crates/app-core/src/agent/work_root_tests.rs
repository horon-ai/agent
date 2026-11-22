use super::*;
use crate::auth::test_sandbox::AuthTestSandbox;
use crate::message::{Message, ToolDefinition};
use crate::provider::{EventStream, Provider};
use crate::tool::Registry;
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

#[derive(Clone)]
struct Silent;

#[async_trait]
impl Provider for Silent {
    fn name(&self) -> &str {
        "work-root-fixture"
    }

    fn fork(&self) -> Arc<dyn Provider> {
        Arc::new(self.clone())
    }

    async fn complete(
        &self,
        _: &[Message],
        _: &[ToolDefinition],
        _: &str,
        _: Option<&str>,
    ) -> anyhow::Result<EventStream> {
        anyhow::bail!("a working-root command never calls the provider")
    }
}

fn agent_in(root: &str) -> (AuthTestSandbox, Agent) {
    let sandbox = AuthTestSandbox::new().expect("isolated home");
    let agent = Agent::new_with_initial_working_dir(Arc::new(Silent), Registry::empty(), Some(root));
    (sandbox, agent)
}

fn dir_with_guidance(parent: &Path, name: &str, guidance: Option<&str>) -> String {
    let dir = parent.join(name);
    std::fs::create_dir_all(&dir).unwrap();
    if let Some(text) = guidance {
        std::fs::write(dir.join("AGENTS.md"), text).unwrap();
    }
    std::fs::canonicalize(&dir).unwrap().to_string_lossy().into_owned()
}

#[test]
fn a_moved_root_resumes_under_the_new_path_and_reads_its_guidance() {
    let temp = tempfile::tempdir().unwrap();
    let old = dir_with_guidance(temp.path(), "old", None);
    let new = dir_with_guidance(temp.path(), "new", Some("Use the new rules."));
    let (_sandbox, mut agent) = agent_in(&old);
    let stored_before = agent.messages().len();

    let (data, message) = agent.work_root_command(&["apply".into(), new.clone()]).expect("apply");

    assert_eq!(agent.working_dir(), Some(new.as_str()));
    assert_eq!(data["target"], new);
    assert_eq!(data["project_guidance_chars"], 18);
    assert!(message.contains("Stored messages are unchanged"));
    assert_eq!(agent.messages().len(), stored_before, "stored originals stay readable");
}

#[test]
fn a_preview_names_the_target_and_changes_nothing() {
    let temp = tempfile::tempdir().unwrap();
    let old = dir_with_guidance(temp.path(), "old", None);
    let new = dir_with_guidance(temp.path(), "new", None);
    let (_sandbox, mut agent) = agent_in(&old);

    let (data, message) = agent
        .work_root_command(&["preview".into(), new.clone()])
        .expect("preview");

    assert_eq!(agent.working_dir(), Some(old.as_str()));
    assert_eq!(data["applied"], false);
    assert_eq!(data["target"], new);
    assert!(data["project_guidance_chars"].is_null());
    assert!(message.contains("no project AGENTS.md"));
}

#[test]
fn an_invalid_or_unchanged_target_leaves_the_root_as_it_was() {
    let temp = tempfile::tempdir().unwrap();
    let old = dir_with_guidance(temp.path(), "old", None);
    let file = temp.path().join("file.txt");
    std::fs::write(&file, "x").unwrap();
    let (_sandbox, mut agent) = agent_in(&old);

    for bad in [
        temp.path().join("missing").to_string_lossy().into_owned(),
        file.to_string_lossy().into_owned(),
        String::new(),
        old.clone(),
    ] {
        assert!(
            agent.work_root_command(&["apply".into(), bad.clone()]).is_err(),
            "{bad:?}"
        );
        assert_eq!(agent.working_dir(), Some(old.as_str()), "{bad:?}");
    }
    assert!(agent.work_root_command(&["apply".into()]).is_err());
}
