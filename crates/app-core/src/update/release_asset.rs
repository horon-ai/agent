//! Verify and stage a release's agent binary before touching an installed version or channel.
use anyhow::{Context, Result, bail};
use std::io::{Cursor, Write};
use std::path::{Component, Path, PathBuf};

fn asset<'a>(release: &'a super::GitHubRelease, name: &str) -> Result<&'a super::GitHubAsset> {
    let mut matches = release.assets.iter().filter(|asset| asset.name == name);
    let found = matches
        .next()
        .with_context(|| format!("release is missing required asset {name}"))?;
    if matches.next().is_some() {
        bail!("release repeats asset {name}");
    }
    Ok(found)
}

fn download(client: &reqwest::blocking::Client, asset: &super::GitHubAsset) -> Result<Vec<u8>> {
    Ok(super::asset_request(client, asset)
        .send()?
        .error_for_status()?
        .bytes()?
        .to_vec())
}

pub(super) fn binary_asset(release: &super::GitHubRelease) -> Result<&super::GitHubAsset> {
    let name = super::get_asset_name();
    let archive = format!("{}.tar.gz", name.strip_suffix(".exe").unwrap_or(name));
    if release.assets.iter().any(|entry| entry.name == archive) {
        asset(release, &archive)
    } else {
        asset(release, name)
    }
}

/// Prepare a complete immutable version. No channel or user configuration is changed here.
pub(super) fn stage_release(
    client: &reqwest::blocking::Client,
    release: &super::GitHubRelease,
    binary_asset: &super::GitHubAsset,
    binary_bytes: &[u8],
    version: &str,
    versions: &Path,
) -> Result<PathBuf> {
    stage_package(
        &|name| download(client, asset(release, name)?),
        release,
        AgentInput::Published(binary_asset, binary_bytes),
        version,
        version,
        versions,
    )
}

/// A locally compiled executable is trusted as source code, not falsely compared to published binary bytes.
pub(super) fn stage_source(
    client: &reqwest::blocking::Client,
    release: &super::GitHubRelease,
    binary: &Path,
    agent_version: &str,
    directory_version: &str,
    versions: &Path,
) -> Result<PathBuf> {
    let bytes = std::fs::read(binary)?;
    stage_package(
        &|name| download(client, asset(release, name)?),
        release,
        AgentInput::Local(&bytes),
        agent_version,
        directory_version,
        versions,
    )
}

enum AgentInput<'a> {
    Published(&'a super::GitHubAsset, &'a [u8]),
    Local(&'a [u8]),
}

/// Offline installer entry: use the same checks and staging as downloaded releases.
pub(super) fn stage_local(
    directory: &Path,
    version: &str,
    source_binary: Option<&Path>,
    versions: &Path,
) -> Result<PathBuf> {
    validate_version(version)?;
    let root = std::fs::symlink_metadata(directory)?;
    if !root.is_dir() || root.file_type().is_symlink() {
        bail!("release assets must be in a real directory");
    }
    let read_asset = |name: &str| -> Result<Vec<u8>> {
        let path = directory.join(name);
        let metadata =
            std::fs::symlink_metadata(&path).with_context(|| format!("release is missing required asset {name}"))?;
        if !metadata.file_type().is_file() {
            bail!("release asset is not a regular file: {name}");
        }
        Ok(std::fs::read(path)?)
    };
    let binary_name = super::get_asset_name();
    let archive_name = format!("{}.tar.gz", binary_name.trim_end_matches(".exe"));
    let name = if directory.join(&archive_name).try_exists()? {
        archive_name
    } else {
        binary_name.to_owned()
    };
    let binary_asset = super::GitHubAsset {
        name,
        browser_download_url: String::new(),
        api_url: String::new(),
        _size: 0,
    };
    let release = super::GitHubRelease {
        tag_name: format!("v{version}"),
        _name: None,
        _html_url: String::new(),
        _published_at: None,
        _target_commitish: String::new(),
        assets: Vec::new(),
    };
    let bytes = match source_binary {
        Some(path) => {
            if !std::fs::symlink_metadata(path)?.file_type().is_file() {
                bail!("source executable is not a regular file");
            }
            std::fs::read(path)?
        }
        None => read_asset(&binary_asset.name)?,
    };
    let input = if source_binary.is_some() {
        AgentInput::Local(&bytes)
    } else {
        AgentInput::Published(&binary_asset, &bytes)
    };
    stage_package(&read_asset, &release, input, version, version, versions)
}

fn stage_package(
    read_asset: &dyn Fn(&str) -> Result<Vec<u8>>,
    release: &super::GitHubRelease,
    input: AgentInput<'_>,
    version: &str,
    directory_version: &str,
    versions: &Path,
) -> Result<PathBuf> {
    validate_version(version)?;
    validate_version(directory_version)?;
    if release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name) != version {
        bail!("release tag and agent version differ");
    }
    let sums = String::from_utf8(read_asset("SHA256SUMS")?)?;
    let (binary_asset, binary_bytes) = match input {
        AgentInput::Published(asset, bytes) => {
            super::verify_asset_checksum_text(&sums, &asset.name, bytes)?;
            (Some(asset), bytes)
        }
        AgentInput::Local(bytes) => (None, bytes),
    };

    std::fs::create_dir_all(versions)?;
    let metadata = std::fs::symlink_metadata(versions)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("version install root is not a real directory");
    }
    let staging = tempfile::Builder::new()
        .prefix(".release-stage-")
        .tempdir_in(versions)?;
    let binary = staging.path().join(crate::build::binary_name());
    if let Some(binary_asset) = binary_asset.filter(|asset| asset.name.ends_with(".tar.gz")) {
        let files = extract_archive(binary_bytes, staging.path())?;
        let base = binary_asset.name.trim_end_matches(".tar.gz");
        let windows_name = format!("{base}.exe");
        let candidates: Vec<_> = files
            .iter()
            .filter(|path| {
                path.parent() == Some(staging.path())
                    && path
                        .file_name()
                        .is_some_and(|name| name == base || name == windows_name.as_str())
            })
            .collect();
        if candidates.len() != 1 {
            bail!("agent archive must contain exactly one platform executable");
        }
        if candidates[0] != &binary {
            std::fs::rename(candidates[0], &binary)?;
        }
        for path in files {
            if path.extension().is_some_and(|extension| extension == "bin") {
                horon_agent_platform::set_permissions_executable(&path)?;
            }
        }
    } else {
        let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(&binary)?;
        output.write_all(binary_bytes)?;
        output.sync_all()?;
    }
    horon_agent_platform::set_permissions_executable(&binary)?;
    super::patch_termux_interpreter_if_needed(&binary)?;

    let destination = versions.join(directory_version);
    match std::fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            let installed = destination.join(crate::build::binary_name());
            if super::digest::file_sha256(&binary)? != super::digest::file_sha256(&installed)? {
                bail!("the installed agent version already contains different bytes");
            }
        }
        Ok(_) => bail!("the installed version path is not a real directory"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::rename(staging.path(), &destination)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(destination.join(crate::build::binary_name()))
}

pub(super) fn validate_version(version: &str) -> Result<()> {
    if version.is_empty()
        || version == "."
        || version == ".."
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-+".contains(&byte))
    {
        bail!("release version is not a safe version directory name");
    }
    Ok(())
}

/// Extract only fresh regular files and directories; never follow archive links.
pub(super) fn extract_archive(bytes: &[u8], destination: &Path) -> Result<Vec<PathBuf>> {
    let reader = flate2::read::GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(reader);
    let mut seen = std::collections::BTreeSet::new();
    let mut files = Vec::new();
    let mut total = 0_u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        if seen.len() >= 4096 {
            bail!("release archive has too many entries");
        }
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            bail!("release archive contains a link or unsupported entry");
        }
        let raw = entry.path_bytes();
        let raw = std::str::from_utf8(&raw).context("release archive path is not UTF-8")?;
        let name = if kind.is_dir() { raw.trim_end_matches('/') } else { raw };
        let name = name.trim_start_matches("./");
        if kind.is_dir() && name == "." {
            if !seen.insert(".".to_owned()) {
                bail!("release archive repeats its root directory");
            }
            continue;
        }
        if name.is_empty()
            || name.contains(['\\', ':'])
            || name
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
            || Path::new(name)
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
            || !seen.insert(name.to_owned())
        {
            bail!("release archive contains an unsafe or duplicate path");
        }
        let path = destination.join(name);
        if kind.is_dir() {
            std::fs::create_dir_all(&path)?;
            continue;
        }
        let size = entry.size();
        total = total.checked_add(size).context("release archive size overflow")?;
        if size > 512 * 1024 * 1024 || total > 2 * 1024 * 1024 * 1024 {
            bail!("release archive exceeds the extraction budget");
        }
        std::fs::create_dir_all(path.parent().context("archive file has no parent")?)?;
        let mut output = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
        let copied = std::io::copy(&mut entry, &mut output)?;
        if copied != size {
            bail!("release archive file was truncated");
        }
        output.flush()?;
        files.push(path);
    }
    if files.is_empty() {
        bail!("release archive has no files");
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// A release asset directory holding one tarred agent binary and its checksums.
    ///
    /// Returns the archive's file name, which is what SHA256SUMS must name.
    fn release_directory(directory: &Path, binary: &[u8]) -> String {
        let name = super::super::get_asset_name();
        let base = name.trim_end_matches(".exe");
        let archive_name = format!("{base}.tar.gz");
        let bytes = archive(&[(base, tar::EntryType::Regular)], binary);
        let sums = format!("{:x}  {archive_name}\n", Sha256::digest(&bytes));
        std::fs::write(directory.join(&archive_name), &bytes).unwrap();
        std::fs::write(directory.join("SHA256SUMS"), sums).unwrap();
        archive_name
    }

    fn archive(entries: &[(&str, tar::EntryType)], contents: &[u8]) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (name, kind) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o644);
            header.set_entry_type(*kind);
            let bytes: &[u8] = if kind.is_dir() { b"" } else { contents };
            header.set_size(bytes.len() as u64);
            header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
            header.set_cksum();
            builder.append(&header, bytes).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn stages_a_directory_release_without_any_plugin_bundle() {
        let assets = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        let binary = b"agent-binary";
        release_directory(assets.path(), binary);

        let installed = stage_local(assets.path(), "1.2.3", None, versions.path()).unwrap();

        assert_eq!(
            installed,
            versions.path().join("1.2.3").join(crate::build::binary_name())
        );
        assert_eq!(std::fs::read(&installed).unwrap(), binary);
        assert!(!installed.parent().unwrap().join("bundled-plugins").exists());
        // Staging the same release again reuses the identical installed version.
        assert_eq!(
            stage_local(assets.path(), "1.2.3", None, versions.path()).unwrap(),
            installed
        );
    }

    #[test]
    fn rejects_binary_digest_mismatch() {
        let assets = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        let archive_name = release_directory(assets.path(), b"agent-binary");
        let wrong = format!("{}  {archive_name}\n", "a".repeat(64));
        std::fs::write(assets.path().join("SHA256SUMS"), &wrong).unwrap();

        let error = stage_local(assets.path(), "1.2.3", None, versions.path()).unwrap_err();

        assert!(error.to_string().contains("mismatch"), "{error}");
        assert!(!versions.path().join("1.2.3").exists());
        // A digest that is not even lowercase hex is refused the same way.
        std::fs::write(assets.path().join("SHA256SUMS"), "not-a-digest\n").unwrap();
        assert!(stage_local(assets.path(), "1.2.3", None, versions.path()).is_err());
    }

    #[test]
    fn an_existing_installed_version_with_other_bytes_is_not_overwritten() {
        let assets = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        release_directory(assets.path(), b"agent-binary");
        let installed = stage_local(assets.path(), "1.2.3", None, versions.path()).unwrap();
        std::fs::write(&installed, b"different-bytes").unwrap();

        let error = stage_local(assets.path(), "1.2.3", None, versions.path()).unwrap_err();

        assert!(error.to_string().contains("different bytes"), "{error}");
        assert_eq!(std::fs::read(&installed).unwrap(), b"different-bytes");
    }

    #[test]
    fn archive_extraction_refuses_escape_links_and_duplicates() {
        for entries in [
            vec![("../escape", tar::EntryType::Regular)],
            vec![("/absolute", tar::EntryType::Regular)],
            vec![("lib\\escape", tar::EntryType::Regular)],
            vec![("link", tar::EntryType::Symlink)],
            vec![("link", tar::EntryType::Link)],
            vec![("file", tar::EntryType::Regular), ("file", tar::EntryType::Regular)],
            vec![("file", tar::EntryType::Regular), ("./file", tar::EntryType::Regular)],
        ] {
            let directory = tempfile::tempdir().unwrap();
            assert!(extract_archive(&archive(&entries, b"x"), directory.path()).is_err());
        }
        let directory = tempfile::tempdir().unwrap();
        let paths = extract_archive(
            &archive(
                &[
                    ("./", tar::EntryType::Directory),
                    ("./lib/library", tar::EntryType::Regular),
                ],
                b"x",
            ),
            directory.path(),
        )
        .unwrap();
        assert_eq!(paths, vec![directory.path().join("lib/library")]);
        assert_eq!(std::fs::read(&paths[0]).unwrap(), b"x");
    }
}
