use super::lazy_activation::{RecordingTool, context, registry_with_inactive_tool};
use super::*;
use std::sync::atomic::Ordering;

#[tokio::test]
async fn clone_keeps_activator() {
    let (registry, calls, runs) = registry_with_inactive_tool("lazy_clone").await;
    let clone = registry.clone();

    let output = clone
        .execute("lazy_clone", serde_json::json!({}), context())
        .await
        .expect("a clone must keep the activator of the registry it came from");

    assert_eq!(output.output, "lazy_clone ran");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    assert!(
        registry.tool_names().await.iter().any(|name| name == "lazy_clone"),
        "activation through a clone is visible to the registry it came from"
    );
}

#[tokio::test]
async fn clone_sharing_compaction_keeps_activator() {
    let (registry, calls, runs) = registry_with_inactive_tool("lazy_shared").await;
    let clone = registry.clone_sharing_compaction();

    let output = clone
        .execute("lazy_shared", serde_json::json!({}), context())
        .await
        .expect("the per-call clone must keep the activator");

    assert_eq!(output.output, "lazy_shared ran");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn weak_registry_upgrade_keeps_activator() {
    let (registry, calls, runs) = registry_with_inactive_tool("lazy_upgraded").await;
    let weak = registry.downgrade();
    let upgraded = weak.upgrade().expect("the registry is still alive");

    let output = upgraded
        .execute("lazy_upgraded", serde_json::json!({}), context())
        .await
        .expect("an upgraded registry must keep the activator");

    assert_eq!(output.output, "lazy_upgraded ran");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn empty_registry_has_no_activator() {
    let registry = Registry::empty();
    let (tool, runs) = RecordingTool::new("lazy_absent");
    registry
        .register_inactive("lazy_absent".to_string(), Arc::new(tool))
        .await;

    let error = registry
        .execute("lazy_absent", serde_json::json!({}), context())
        .await
        .expect_err("without an activator an inactive tool stays uncallable");

    assert_eq!(runs.load(Ordering::SeqCst), 0);
    assert!(
        error.to_string().starts_with("Unknown tool: lazy_absent."),
        "got: {error}"
    );
}
