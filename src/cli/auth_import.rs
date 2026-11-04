use super::provider_init::ProviderChoice;
use anyhow::Result;

pub(crate) fn run(choice: &ProviderChoice, json: bool) -> Result<()> {
    let _ = (choice, json);
    anyhow::bail!(
        "Local credential import was removed with the vendor providers. Use HoronRouter to attach vendor accounts."
    )
}
