use super::hosted_tenant::enforce_startup;
use crate::hosting::{HostingInputs, HostingMode, HostingPaths, HostingSection, TENANT_ID_ENV};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn mode(path: &Path, bits: u32) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).expect("set mode");
}

struct Boundary {
    _root: tempfile::TempDir,
    home: PathBuf,
    runtime: PathBuf,
    operator: PathBuf,
}

impl Boundary {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("scratch root");
        let home = root.path().join("data/alice/home");
        let runtime = root.path().join("run/alice");
        let operator = root.path().join("operator/.horon");
        for dir in [&home, &runtime, &operator] {
            std::fs::create_dir_all(dir).expect("directory");
            mode(dir, 0o700);
        }
        Self {
            _root: root,
            home,
            runtime,
            operator,
        }
    }

    fn paths(&self) -> HostingPaths {
        HostingPaths {
            home: Some(self.home.clone()),
            runtime_dir: Some(self.runtime.clone()),
            operator_home: self.operator.clone(),
            gateway_enabled: false,
        }
    }
}

fn container_inputs(tenant: Option<&str>) -> HostingInputs {
    HostingInputs {
        mode: Some("per-user-container".to_string()),
        tenant_id: tenant.map(str::to_string),
        section: HostingSection::default(),
    }
}

#[test]
fn an_absent_mode_starts_without_hosting() {
    let policy = enforce_startup(
        &HostingInputs::default(),
        &HostingPaths {
            home: None,
            runtime_dir: None,
            operator_home: PathBuf::from("/nonexistent/operator/.horon"),
            gateway_enabled: true,
        },
    )
    .expect("a plain daemon starts");
    assert_eq!(policy.mode(), HostingMode::None);
}

#[test]
fn a_complete_container_boundary_starts() {
    let boundary = Boundary::new();
    let policy = enforce_startup(&container_inputs(Some("alice")), &boundary.paths()).expect("starts");
    assert!(policy.is_per_user_container());
    assert_eq!(policy.tenant_id().map(|tenant| tenant.as_str()), Some("alice"));
    assert!(policy.allows_shared_children());
}

#[test]
fn a_hosted_daemon_refuses_the_operator_home() {
    let boundary = Boundary::new();
    let mut paths = boundary.paths();
    paths.home = Some(boundary.operator.clone());
    let error = enforce_startup(&container_inputs(Some("alice")), &paths).expect_err("operator home");
    let message = error.to_string();
    assert!(message.contains("must not be the operator's own home"), "{message}");
}

#[test]
fn a_hosted_daemon_refuses_an_enabled_gateway() {
    let boundary = Boundary::new();
    let mut paths = boundary.paths();
    paths.gateway_enabled = true;
    let error = enforce_startup(&container_inputs(Some("alice")), &paths).expect_err("gateway");
    assert!(error.to_string().contains("TCP gateway must be disabled"), "{error}");
}

#[test]
fn a_hosted_daemon_requires_a_tenant_id() {
    let boundary = Boundary::new();
    let error = enforce_startup(&container_inputs(None), &boundary.paths()).expect_err("missing tenant");
    assert!(error.to_string().contains(TENANT_ID_ENV), "{error}");
}

#[test]
fn an_unknown_hosting_value_is_a_startup_error() {
    let boundary = Boundary::new();
    let inputs = HostingInputs {
        mode: Some("per_user_container".to_string()),
        tenant_id: Some("alice".to_string()),
        section: HostingSection::default(),
    };
    let error = enforce_startup(&inputs, &boundary.paths()).expect_err("unknown mode");
    assert!(
        error
            .to_string()
            .contains("allowed values: per-user-container, shared-worker"),
        "{error}"
    );
}

#[test]
fn a_shared_worker_starts_and_reports_no_shared_children() {
    let mut inputs = HostingInputs {
        mode: Some("shared-worker".to_string()),
        tenant_id: Some("alice".to_string()),
        section: HostingSection::default(),
    };
    let paths = HostingPaths {
        home: None,
        runtime_dir: None,
        operator_home: PathBuf::from("/home/operator/.horon"),
        gateway_enabled: true,
    };
    let policy = enforce_startup(&inputs, &paths).expect("a shared worker starts");
    assert!(policy.is_shared_worker());
    assert!(!policy.allows_shared_children());

    inputs.section.allow_shared_children = Some(true);
    let widened = enforce_startup(&inputs, &paths).expect("still starts");
    assert!(
        !widened.allows_shared_children(),
        "the config file must not widen a shared worker"
    );
}

#[test]
fn a_narrowed_container_refuses_shared_children() {
    let boundary = Boundary::new();
    let mut inputs = container_inputs(Some("alice"));
    inputs.section.allow_shared_children = Some(false);
    let policy = enforce_startup(&inputs, &boundary.paths()).expect("starts");
    assert!(!policy.allows_shared_children());
}
