use super::*;

#[test]
fn create_and_resume_goal_persists_project_goal() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("repo");
    std::fs::create_dir_all(&project).expect("project dir");
    let prev_home = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", temp.path());

    let goal = create_goal(
        GoalCreateInput {
            title: "Ship mobile MVP".to_string(),
            scope: GoalScope::Project,
            next_steps: vec!["finish reconnect flow".to_string()],
            progress_percent: Some(40),
            ..GoalCreateInput::default()
        },
        Some(&project),
    )
    .expect("create goal");
    assert_eq!(goal.id, "ship-mobile-mvp");

    let loaded = load_goal(&goal.id, Some(GoalScope::Project), Some(&project))
        .expect("load")
        .expect("goal exists");
    assert_eq!(loaded.title, "Ship mobile MVP");

    let manager = crate::memory::MemoryManager::new().with_project_dir(&project);
    let graph = manager.load_project_graph().expect("load graph");
    let goal_mem = graph
        .get_memory(&format!("goal:{}", goal.id))
        .expect("goal memory mirror");
    assert!(goal_mem.tags.iter().any(|tag| tag == "goal"));
    assert!(goal_mem.content.contains("Ship mobile MVP"));

    let session_id = "ses_goal_test";
    attach_goal_to_session(session_id, &goal, Some(&project)).expect("attach");
    let resumed = resume_goal(session_id, Some(&project))
        .expect("resume")
        .expect("goal resumed");
    assert_eq!(resumed.id, goal.id);

    if let Some(prev_home) = prev_home {
        crate::env::set_var("HORON_HOME", prev_home);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[test]
fn a_budget_limited_goal_is_not_resumed_automatically() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("repo");
    std::fs::create_dir_all(&project).expect("project dir");
    let prev_home = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", temp.path());

    let goal = create_goal(
        GoalCreateInput {
            title: "Spent out".to_string(),
            scope: GoalScope::Project,
            budget_tokens: Some(10),
            ..GoalCreateInput::default()
        },
        Some(&project),
    )
    .expect("create goal");
    attach_goal_to_session("ses_budget_limit", &goal, Some(&project)).expect("attach");

    update_goal(
        &goal.id,
        Some(GoalScope::Project),
        Some(&project),
        GoalUpdateInput {
            status: Some(GoalStatus::BudgetLimited),
            ..GoalUpdateInput::default()
        },
    )
    .expect("update")
    .expect("goal exists");

    assert!(
        resume_goal("ses_budget_limit", Some(&project))
            .expect("resume")
            .is_none(),
        "a goal that spent its budget must wait for an operator, not resume itself"
    );
    assert_eq!(
        GoalStatus::parse("budget_limited"),
        Some(GoalStatus::BudgetLimited),
        "the stored string must parse back"
    );
    assert_eq!(GoalStatus::BudgetLimited.as_str(), "budget_limited");

    let stored = load_goal(&goal.id, Some(GoalScope::Project), Some(&project))
        .expect("load")
        .expect("goal exists");
    assert_eq!(stored.budget_tokens, Some(10), "the ceiling survives the transition");

    if let Some(prev_home) = prev_home {
        crate::env::set_var("HORON_HOME", prev_home);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[test]
fn raising_the_budget_and_reactivating_resumes_a_spent_goal() {
    let _guard = crate::storage::lock_test_env();
    let temp = tempfile::tempdir().expect("tempdir");
    let project = temp.path().join("repo");
    std::fs::create_dir_all(&project).expect("project dir");
    let prev_home = std::env::var_os("HORON_HOME");
    crate::env::set_var("HORON_HOME", temp.path());

    let goal = create_goal(
        GoalCreateInput {
            title: "Spend more".to_string(),
            scope: GoalScope::Project,
            budget_tokens: Some(10),
            ..GoalCreateInput::default()
        },
        Some(&project),
    )
    .expect("create goal");
    attach_goal_to_session("ses_budget_raise", &goal, Some(&project)).expect("attach");
    update_goal(
        &goal.id,
        Some(GoalScope::Project),
        Some(&project),
        GoalUpdateInput {
            status: Some(GoalStatus::BudgetLimited),
            ..GoalUpdateInput::default()
        },
    )
    .expect("limit")
    .expect("goal exists");

    let raised = update_goal(
        &goal.id,
        Some(GoalScope::Project),
        Some(&project),
        GoalUpdateInput {
            status: Some(GoalStatus::Active),
            budget_tokens: Some(Some(20)),
            ..GoalUpdateInput::default()
        },
    )
    .expect("raise")
    .expect("goal exists");
    assert_eq!(raised.budget_tokens, Some(20));
    assert_eq!(raised.status, GoalStatus::Active);
    assert!(
        resume_goal("ses_budget_raise", Some(&project))
            .expect("resume")
            .is_some(),
        "an operator who raised the ceiling and reactivated the goal gets it back"
    );

    if let Some(prev_home) = prev_home {
        crate::env::set_var("HORON_HOME", prev_home);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}
