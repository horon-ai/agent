//! `horon-agent provider-doctor` command: a user-facing strict provider/model diagnostic.

use anyhow::Result;

pub async fn run_provider_doctor_command(
    provider: &str,
    _model: Option<&str>,
    _tier: &str,
    _emit_json: bool,
) -> Result<()> {
    anyhow::bail!("provider-doctor is not available in this build: `{provider}` has no provider-doctor driver")
}
