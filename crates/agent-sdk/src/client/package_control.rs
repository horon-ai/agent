use horon_agent_harness_api::{
    ApiEvent, ApiRequest, CAPABILITY_PACKAGE_CONTROL_V1, PackageControlReceipt, PackageControlRequest,
};

use super::{HoronAgentClient, Result, unexpected};

impl HoronAgentClient {
    /// Apply an operation and return the daemon's published catalog generation.
    pub fn package_control(&self, request: PackageControlRequest) -> Result<PackageControlReceipt> {
        self.require_capability(CAPABILITY_PACKAGE_CONTROL_V1, "package_control")?;
        let PackageControlRequest {
            operation_id,
            operation,
        } = request;
        let frame = self.request_ok(ApiRequest::PackageControl {
            operation_id,
            operation,
        })?;
        match frame.event {
            ApiEvent::PackageControl { receipt } => Ok(receipt),
            other => Err(unexpected("package_control", &other)),
        }
    }
}
