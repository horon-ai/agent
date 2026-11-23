use std::io::{self, Write};

use anyhow::Result;
use serde::Serialize;

use crate::config::NamedProviderConfig;
use crate::provider::external::profile_credential_status;

#[derive(Serialize)]
struct ProfileStatus<'a> {
    profile: &'a str,
    credential_source: &'static str,
    status: &'static str,
}

pub(super) fn run(json: bool) -> Result<()> {
    report(&crate::config::config().providers, json, io::stdout().lock())
}

fn report<'a>(
    profiles: impl IntoIterator<Item = (&'a String, &'a NamedProviderConfig)>,
    json: bool,
    mut out: impl Write,
) -> Result<()> {
    let rows: Vec<_> = profiles
        .into_iter()
        .map(|(name, profile)| {
            let (credential_source, status) = profile_credential_status(profile);
            ProfileStatus {
                profile: name,
                credential_source,
                status,
            }
        })
        .collect();
    if json {
        serde_json::to_writer_pretty(&mut out, &serde_json::json!({ "profiles": rows }))?;
        writeln!(out)?;
    } else {
        for row in rows {
            writeln!(out, "{}\t{}\t{}", row.profile, row.credential_source, row.status)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn report_lists_status_without_credential_bytes() -> Result<()> {
        let mut profiles = BTreeMap::new();
        profiles.insert(
            "inline".to_owned(),
            NamedProviderConfig {
                base_url: "https://llm.example.com/v1".to_owned(),
                api_key: Some("FIXTURE-SECRET".to_owned()),
                ..Default::default()
            },
        );
        profiles.insert(
            "missing".to_owned(),
            NamedProviderConfig {
                base_url: "https://llm.example.com/v1".to_owned(),
                api_key_env: Some("HORON_FIXTURE_UNSET_KEY_FOR_STATUS".to_owned()),
                ..Default::default()
            },
        );
        let mut output = Vec::new();
        report(&profiles, true, &mut output)?;
        let text = String::from_utf8(output)?;
        assert!(!text.contains("FIXTURE-SECRET"));
        let value: serde_json::Value = serde_json::from_str(&text)?;
        assert_eq!(value["profiles"][0]["status"], "authenticated");
        assert_eq!(value["profiles"][0]["credential_source"], "inline");
        assert_eq!(value["profiles"][1]["status"], "unauthenticated");
        assert_eq!(value["profiles"][1]["credential_source"], "environment");
        Ok(())
    }
}
