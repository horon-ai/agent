use super::*;

struct TestHome {
    _temp: tempfile::TempDir,
    project: std::path::PathBuf,
    prev_home: Option<std::ffi::OsString>,
}

impl TestHome {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("repo");
        std::fs::create_dir_all(&project).expect("project dir");
        let prev_home = std::env::var_os("HORON_HOME");
        crate::env::set_var("HORON_HOME", temp.path());
        Self {
            _temp: temp,
            project,
            prev_home,
        }
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        if let Some(prev_home) = self.prev_home.take() {
            crate::env::set_var("HORON_HOME", prev_home);
        } else {
            crate::env::remove_var("HORON_HOME");
        }
    }
}

#[tokio::test]
async fn initiative_tool_create_and_resume_round_trip() {
    let _guard = crate::storage::lock_test_env();
    let home = TestHome::new();
    let tool = InitiativeTool::new();
    let ctx = goal_ctx("ses_goal_tool", &home.project);

    let create = tool
        .execute(
            json!({
                "action": "create",
                "title": "Ship mobile MVP",
                "scope": "project",
                "next_steps": ["finish reconnect flow"]
            }),
            ctx.clone(),
        )
        .await
        .expect("create goal");
    assert!(create.output.contains("Created goal `ship-mobile-mvp`"));
    assert!(create.output.contains("Ship mobile MVP"));
    assert!(!create.output.contains("side panel"));

    let resume = tool
        .execute(json!({"action": "resume"}), ctx)
        .await
        .expect("resume goal");
    assert!(resume.output.contains("Resumed goal"));
    assert!(resume.output.contains("finish reconnect flow"));
}

#[tokio::test]
async fn initiative_tool_list_renders_goals_overview_as_tool_output() {
    let _guard = crate::storage::lock_test_env();
    let home = TestHome::new();
    crate::goal::create_goal(
        crate::goal::GoalCreateInput {
            title: "Ship mobile MVP".to_string(),
            scope: crate::goal::GoalScope::Project,
            ..crate::goal::GoalCreateInput::default()
        },
        Some(&home.project),
    )
    .expect("create goal");

    let tool = InitiativeTool::new();
    let ctx = goal_ctx("ses_goal_list", &home.project);
    // Older clients still send `display`; it must be accepted and change nothing.
    let list = tool
        .execute(json!({"action": "list", "display": "inline"}), ctx)
        .await
        .expect("list goals");

    assert!(list.output.contains("# Goals"));
    assert!(list.output.contains("Ship mobile MVP"));
    assert!(list.output.contains("`ship-mobile-mvp`"));
    assert_eq!(list.title.as_deref(), Some("1 goals"));
}

#[tokio::test]
async fn initiative_tool_update_renders_text_output() {
    let _guard = crate::storage::lock_test_env();
    let home = TestHome::new();
    let goal = crate::goal::create_goal(
        crate::goal::GoalCreateInput {
            title: "Ship mobile MVP".to_string(),
            scope: crate::goal::GoalScope::Project,
            next_steps: vec!["finish reconnect flow".to_string()],
            ..crate::goal::GoalCreateInput::default()
        },
        Some(&home.project),
    )
    .expect("create goal");

    let tool = InitiativeTool::new();
    let ctx = goal_ctx("ses_goal_update", &home.project);

    let updated = tool
        .execute(
            json!({
                "action": "update",
                "id": goal.id,
                "next_steps": ["ship reconnect flow"]
            }),
            ctx.clone(),
        )
        .await
        .expect("update goal");
    assert!(updated.output.contains("Updated goal `ship-mobile-mvp`"));

    let shown = tool
        .execute(json!({"action": "show", "id": goal.id}), ctx)
        .await
        .expect("show goal");
    assert!(shown.output.contains("# Goal: Ship mobile MVP"));
    assert!(shown.output.contains("ship reconnect flow"));
}

#[test]
fn test_initiative_schema_milestones_define_items() {
    let schema = InitiativeTool::new().parameters_schema();
    let milestone_items = &schema["properties"]["milestones"]["items"];

    assert_eq!(milestone_items["type"], "object");
    assert_eq!(milestone_items["additionalProperties"], json!(true));
    assert_eq!(milestone_items["properties"]["steps"]["type"], "array");
    assert_eq!(
        milestone_items["properties"]["steps"]["items"]["additionalProperties"],
        json!(true)
    );
}

#[test]
fn test_initiative_schema_omits_display_override() {
    let schema = InitiativeTool::new().parameters_schema();
    assert!(schema["properties"]["display"].is_null());
}

#[test]
fn test_initiative_schema_omits_public_enums_for_scope_and_status() {
    let schema = InitiativeTool::new().parameters_schema();
    assert!(schema["properties"]["scope"]["enum"].is_null());
    assert!(schema["properties"]["status"]["enum"].is_null());
}

fn goal_ctx(session_id: &str, project: &std::path::Path) -> ToolContext {
    ToolContext {
        session_id: session_id.to_string(),
        message_id: "msg1".to_string(),
        tool_call_id: "tool1".to_string(),
        working_dir: Some(project.to_path_buf()),
        stdin_request_tx: None,
        graceful_shutdown_signal: None,
        execution_mode: crate::tool::ToolExecutionMode::AgentTurn,
    }
}

fn open_todo(id: &str, content: &str) -> crate::todo::TodoItem {
    crate::todo::TodoItem {
        id: id.to_string(),
        content: content.to_string(),
        status: "pending".to_string(),
        priority: "medium".to_string(),
        ..Default::default()
    }
}

fn cancelled_todo(id: &str, content: &str) -> crate::todo::TodoItem {
    crate::todo::TodoItem {
        status: "cancelled".to_string(),
        ..open_todo(id, content)
    }
}

fn completed_todo(id: &str, content: &str) -> crate::todo::TodoItem {
    crate::todo::TodoItem {
        status: "completed".to_string(),
        ..open_todo(id, content)
    }
}

fn restore_home(prev_home: Option<std::ffi::OsString>) {
    if let Some(prev_home) = prev_home {
        crate::env::set_var("HORON_HOME", prev_home);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[tokio::test]
async fn goal_create_attaches_the_goal_to_the_session() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("repo");
    std::fs::create_dir_all(&project).expect("project dir");
    let prev_home = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", temp.path());

    let tool = InitiativeTool::new();
    let ctx = goal_ctx("ses_goal_attach", &project);
    let created = tool
        .execute(
            json!({"action": "create", "title": "Ship mobile MVP", "scope": "project"}),
            ctx,
        )
        .await
        .expect("create goal");
    assert!(created.output.contains("Created goal"));

    let attached = crate::goal::load_attached_goal("ses_goal_attach", Some(&project))
        .expect("load attached goal")
        .expect("create must attach the goal to the session");
    assert_eq!(attached.title, "Ship mobile MVP");
    assert_eq!(attached.scope, crate::goal::GoalScope::Project);

    restore_home(prev_home);
}

#[tokio::test]
async fn goal_update_to_completed_is_refused_while_todos_are_open() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("repo");
    std::fs::create_dir_all(&project).expect("project dir");
    let prev_home = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", temp.path());

    let goal = crate::goal::create_goal(
        crate::goal::GoalCreateInput {
            title: "Ship mobile MVP".to_string(),
            scope: crate::goal::GoalScope::Project,
            ..crate::goal::GoalCreateInput::default()
        },
        Some(&project),
    )
    .expect("create goal");

    crate::todo::save_todos(
        "ses_goal_gate",
        &[
            open_todo("t1", "finish reconnect flow"),
            open_todo("t2", "verify offline queue"),
        ],
    )
    .expect("save todos");

    let tool = InitiativeTool::new();
    let err = tool
        .execute(
            json!({"action": "update", "id": goal.id.clone(), "status": "completed"}),
            goal_ctx("ses_goal_gate", &project),
        )
        .await
        .expect_err("open todos must refuse completion");
    let message = err.to_string();
    println!("[qa] refusal text: {message}");
    assert!(message.contains("finish reconnect flow"), "{message}");
    assert!(message.contains("verify offline queue"), "{message}");
    assert!(message.contains("2 open todo tasks remain"), "{message}");

    // The refusal must not have moved the goal.
    let stored = crate::goal::load_goal(&goal.id, None, Some(&project))
        .expect("load goal")
        .expect("goal exists");
    assert_eq!(stored.status, crate::goal::GoalStatus::Active);

    restore_home(prev_home);
}

#[tokio::test]
async fn goal_update_to_completed_succeeds_without_open_todos() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("repo");
    std::fs::create_dir_all(&project).expect("project dir");
    let prev_home = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", temp.path());

    let goal = crate::goal::create_goal(
        crate::goal::GoalCreateInput {
            title: "Ship mobile MVP".to_string(),
            scope: crate::goal::GoalScope::Project,
            ..crate::goal::GoalCreateInput::default()
        },
        Some(&project),
    )
    .expect("create goal");

    crate::todo::save_todos(
        "ses_goal_done",
        &[
            completed_todo("t1", "finish reconnect flow"),
            completed_todo("t2", "verify offline queue"),
        ],
    )
    .expect("save todos");

    let tool = InitiativeTool::new();
    let updated = tool
        .execute(
            json!({"action": "update", "id": goal.id.clone(), "status": "completed"}),
            goal_ctx("ses_goal_done", &project),
        )
        .await
        .expect("completed todos allow completion");
    assert!(updated.output.contains("Updated goal"));

    let stored = crate::goal::load_goal(&goal.id, None, Some(&project))
        .expect("load goal")
        .expect("goal exists");
    assert_eq!(stored.status, crate::goal::GoalStatus::Completed);
    println!(
        "[qa] completion accepted after clearing todos: status={:?}",
        stored.status
    );

    restore_home(prev_home);
}

#[tokio::test]
async fn goal_completion_is_allowed_with_an_empty_todo_list() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("repo");
    std::fs::create_dir_all(&project).expect("project dir");
    let prev_home = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", temp.path());

    let goal = crate::goal::create_goal(
        crate::goal::GoalCreateInput {
            title: "Ship mobile MVP".to_string(),
            scope: crate::goal::GoalScope::Project,
            ..crate::goal::GoalCreateInput::default()
        },
        Some(&project),
    )
    .expect("create goal");

    let tool = InitiativeTool::new();
    tool.execute(
        json!({"action": "update", "id": goal.id.clone(), "status": "completed"}),
        goal_ctx("ses_goal_empty", &project),
    )
    .await
    .expect("an empty todo list allows completion");

    let stored = crate::goal::load_goal(&goal.id, None, Some(&project))
        .expect("load goal")
        .expect("goal exists");
    assert_eq!(stored.status, crate::goal::GoalStatus::Completed);

    restore_home(prev_home);
}

#[tokio::test]
async fn goal_completion_is_allowed_with_only_cancelled_todos() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("repo");
    std::fs::create_dir_all(&project).expect("project dir");
    let prev_home = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", temp.path());

    let goal = crate::goal::create_goal(
        crate::goal::GoalCreateInput {
            title: "Ship mobile MVP".to_string(),
            scope: crate::goal::GoalScope::Project,
            ..crate::goal::GoalCreateInput::default()
        },
        Some(&project),
    )
    .expect("create goal");

    crate::todo::save_todos("ses_goal_cancelled", &[cancelled_todo("t1", "drop the old auth path")])
        .expect("save todos");

    let tool = InitiativeTool::new();
    tool.execute(
        json!({"action": "update", "id": goal.id.clone(), "status": "completed"}),
        goal_ctx("ses_goal_cancelled", &project),
    )
    .await
    .expect("cancelled todos do not block completion");

    let stored = crate::goal::load_goal(&goal.id, None, Some(&project))
        .expect("load goal")
        .expect("goal exists");
    assert_eq!(stored.status, crate::goal::GoalStatus::Completed);

    restore_home(prev_home);
}
