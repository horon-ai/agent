//! Tests for agent-definition discovery under `.horon/agents/`.

use super::{AgentDefinitionError, compose_initial_message, discover, resolve};

/// A sandboxed `HORON_HOME`, so a test never reads the real `~/.horon/agents`.
///
/// The guard is held for the whole test: `HORON_HOME` is process-wide state.
struct Sandbox {
    _dir: tempfile::TempDir,
    _guard: std::sync::MutexGuard<'static, ()>,
    previous: Option<std::ffi::OsString>,
}

impl Sandbox {
    fn new() -> Self {
        let guard = crate::storage::lock_test_env();
        let dir = tempfile::tempdir().expect("sandbox home");
        let previous = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", dir.path());
        Self {
            _dir: dir,
            _guard: guard,
            previous,
        }
    }

    /// `<home>/agents` — horon-agent's own definition directory.
    fn home_agents(&self) -> std::path::PathBuf {
        self._dir.path().join("agents")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(previous) => crate::env::set_var("HORON_HOME", previous),
            None => crate::env::remove_var("HORON_HOME"),
        }
    }
}

fn write_definition(directory: &std::path::Path, file: &str, body: &str) {
    std::fs::create_dir_all(directory).expect("create definition dir");
    std::fs::write(directory.join(file), body).expect("write definition");
}

fn scout_definition(directory: &std::path::Path) {
    write_definition(
        directory,
        "scout.md",
        "---\nname: scout\ndescription: Read-only investigator\ntools: [read, agentgrep, ls]\n---\n\nInvestigate and report.\n",
    );
}

#[test]
fn project_definition_shadows_the_home_definition() {
    let sandbox = Sandbox::new();
    let project = tempfile::tempdir().expect("project");
    scout_definition(&project.path().join(".horon/agents"));
    write_definition(
        &sandbox.home_agents(),
        "scout.md",
        "---\nname: scout\ndescription: Home scout\ntools: [bash]\n---\n\nHome body.\n",
    );

    let resolved = resolve("scout", Some(project.path())).expect("project definition wins");
    assert_eq!(resolved.description.as_deref(), Some("Read-only investigator"));
    assert_eq!(resolved.prompt, "Investigate and report.");
    assert_eq!(
        resolved.tools,
        Some(vec!["read".to_string(), "agentgrep".to_string(), "ls".to_string()])
    );
}

#[test]
fn home_definition_is_used_when_the_project_has_none() {
    let sandbox = Sandbox::new();
    let project = tempfile::tempdir().expect("project");
    write_definition(
        &sandbox.home_agents(),
        "reviewer.md",
        "---\nname: reviewer\ndescription: Reviews diffs\nmodel: gpt-5.5\neffort: high\n---\n\nReview.\n",
    );

    let resolved = resolve("reviewer", Some(project.path())).expect("home definition resolves");
    assert_eq!(resolved.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(resolved.effort.as_deref(), Some("high"));
    assert_eq!(resolved.tools, None);
}

#[test]
fn unknown_name_reports_the_known_agents() {
    let sandbox = Sandbox::new();
    let project = tempfile::tempdir().expect("project");
    scout_definition(&project.path().join(".horon/agents"));
    write_definition(
        &sandbox.home_agents(),
        "reviewer.md",
        "---\nname: reviewer\ndescription: Reviews diffs\n---\n\nReview.\n",
    );

    let error = resolve("missing", Some(project.path())).expect_err("unknown name must fail");
    let AgentDefinitionError::Unknown { name, known } = &error else {
        panic!("expected an unknown-name error");
    };
    assert_eq!(name, "missing");
    assert_eq!(known, &vec!["reviewer".to_string(), "scout".to_string()]);
    assert!(error.to_string().contains("reviewer, scout"), "got: {error}");
}

#[test]
fn unknown_name_with_no_definitions_says_where_to_put_one() {
    let _sandbox = Sandbox::new();
    let project = tempfile::tempdir().expect("project");

    let error = resolve("scout", Some(project.path())).expect_err("no definitions exist");
    let message = error.to_string();
    assert!(message.contains("no agent definitions exist"), "got: {message}");
    assert!(message.contains(".horon/agents/"), "got: {message}");
}

#[test]
fn a_definition_that_fails_to_parse_is_reported_for_its_own_name() {
    let _sandbox = Sandbox::new();
    let project = tempfile::tempdir().expect("project");
    write_definition(
        &project.path().join(".horon/agents"),
        "broken.md",
        "---\nname: broken\ndescription: never closed\n",
    );

    let error = resolve("broken", Some(project.path())).expect_err("unparsable definition fails");
    assert!(
        matches!(error, AgentDefinitionError::Unreadable { .. }),
        "expected an unreadable error, got: {error:?}"
    );
    assert!(discover(Some(project.path())).is_empty());
}

#[test]
fn discover_lists_names_in_order_and_skips_unparsable_files() {
    let _sandbox = Sandbox::new();
    let project = tempfile::tempdir().expect("project");
    let definitions = project.path().join(".horon/agents");
    scout_definition(&definitions);
    write_definition(
        &definitions,
        "reviewer.md",
        "---\nname: reviewer\ndescription: Reviews diffs\n---\n\nReview.\n",
    );
    write_definition(&definitions, "ignored.txt", "not markdown\n");

    let listed = discover(Some(project.path()));
    let names: Vec<&str> = listed.iter().map(|definition| definition.name.as_str()).collect();
    assert_eq!(names, vec!["reviewer", "scout"]);
}

#[test]
fn no_definition_directory_means_no_definitions() {
    let _sandbox = Sandbox::new();
    let project = tempfile::tempdir().expect("project");
    assert!(discover(Some(project.path())).is_empty());
}

#[test]
fn comma_delimited_tools_and_composed_messages() {
    let _sandbox = Sandbox::new();
    let project = tempfile::tempdir().expect("project");
    write_definition(
        &project.path().join(".horon/agents"),
        "comma.md",
        "---\nname: comma\ntools: read, ls\n---\n\nRole.\n",
    );

    let resolved = resolve("comma", Some(project.path())).expect("comma form parses");
    assert_eq!(resolved.tools, Some(vec!["read".to_string(), "ls".to_string()]));

    assert_eq!(
        compose_initial_message("Role.", Some("Task".to_string())).as_deref(),
        Some("Role.\n\n---\n\nTask")
    );
    assert_eq!(
        compose_initial_message("", Some("Task".to_string())).as_deref(),
        Some("Task")
    );
    assert_eq!(compose_initial_message("Role.", None).as_deref(), Some("Role."));
}
