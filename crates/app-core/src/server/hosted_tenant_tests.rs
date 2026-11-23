use super::hosted_tenant::enforce_policy_with_paths;
use crate::hosting::{self, HostingInputs, HostingMode, HostingPaths, HostingPolicy, HostingSection};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn enforce_startup(inputs: &HostingInputs, paths: &HostingPaths) -> anyhow::Result<HostingPolicy> {
    let policy = hosting::resolve(inputs)?;
    enforce_policy_with_paths(&policy, || paths.clone())?;
    Ok(policy)
}

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
