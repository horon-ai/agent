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

/// Name of the optional interactive-client archive for a platform, derived from
/// the agent asset name. `None` means the platform publishes no such archive.
fn tui_asset_name(agent_asset_name: &str) -> Option<String> {
    let stem = agent_asset_name.strip_suffix(".tar.gz").unwrap_or(agent_asset_name);
    let stem = stem.strip_suffix(".exe").unwrap_or(stem);
    let suffix = stem.strip_prefix("horon-agent")?;
    if suffix.contains("freebsd") || suffix.contains("unknown") {
        return None;
    }
    Some(format!("horon-tui{suffix}.tar.gz"))
}

/// The interactive client is found beside the agent, so it keeps the bare stem.
fn tui_binary_name() -> &'static str {
    if crate::build::binary_name().ends_with(".exe") {
        "horon-tui.exe"
    } else {
        "horon-tui"
    }
}

/// Where the optional interactive-client archive came from, before it is checked.
enum TuiSource {
    /// Nothing to install: source builds, and asset directories without a TUI.
    NotRequested,
    /// The release publishes no interactive client for this platform.
    Missing {
        asset: String,
    },
    Archive {
        asset: String,
        bytes: Vec<u8>,
    },
    Failed {
        asset: String,
        reason: String,
    },
}

/// What happened to the interactive client, for one warning line at the end.
#[derive(Debug, PartialEq, Eq)]
enum TuiOutcome {
    NotRequested,
    Installed,
    AlreadyPresent,
    Missing { asset: String },
    Skipped { asset: String, reason: String },
}

/// The interactive client after verification: either already decided, or
/// extracted beside the agent and waiting to be moved into the version.
enum StagedTui {
    Settled(TuiOutcome),
    Ready {
        asset: String,
        files: Vec<PathBuf>,
        stage: tempfile::TempDir,
    },
}

/// The single warning a partly failed interactive-client install may print.
fn tui_warning(outcome: &TuiOutcome) -> Option<String> {
    match outcome {
        TuiOutcome::Missing { asset } => Some(format!(
            "release does not publish {asset}; installed horon-agent without the interactive client"
        )),
        TuiOutcome::Skipped { asset, reason } => {
            Some(format!("could not install {asset} beside horon-agent: {reason}"))
        }
        TuiOutcome::NotRequested | TuiOutcome::Installed | TuiOutcome::AlreadyPresent => None,
    }
}

/// Mirror the warning to the terminal and the log; the install itself succeeded.
fn announce_tui(outcome: &TuiOutcome) {
    if let Some(text) = tui_warning(outcome) {
        eprintln!("warning: {text}");
        crate::logging::warn(&text);
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
    let tui = release_tui_source(client, release);
    let (binary, _tui) = stage_package(
        &|name| download(client, asset(release, name)?),
        release,
        AgentInput::Published(binary_asset, binary_bytes),
        tui,
        version,
        version,
        versions,
    )?;
    Ok(binary)
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
    // A source build pairs with the client built from the same tree, so a
    // released interactive client must never be staged beside it.
    let (installed, _tui) = stage_package(
        &|name| download(client, asset(release, name)?),
        release,
        AgentInput::Local(&bytes),
        TuiSource::NotRequested,
        agent_version,
        directory_version,
        versions,
    )?;
    Ok(installed)
}

enum AgentInput<'a> {
    Published(&'a super::GitHubAsset, &'a [u8]),
    Local(&'a [u8]),
}

/// Download the optional interactive client named by the release, if any.
fn release_tui_source(client: &reqwest::blocking::Client, release: &super::GitHubRelease) -> TuiSource {
    let Some(name) = tui_asset_name(super::get_asset_name()) else {
        return TuiSource::NotRequested;
    };
    let Some(entry) = release.assets.iter().find(|entry| entry.name == name) else {
        return TuiSource::Missing { asset: name };
    };
    match download(client, entry) {
        Ok(bytes) => TuiSource::Archive { asset: name, bytes },
        Err(error) => TuiSource::Failed {
            asset: name,
            reason: format!("{error:#}"),
        },
    }
}

/// Find the optional interactive client in an offline asset directory.
///
/// Absence is not a failure: an older release, or an older installer that never
/// fetched the archive, installs the headless agent alone.
fn directory_tui_source(directory: &Path) -> Result<TuiSource> {
    let Some(name) = tui_asset_name(super::get_asset_name()) else {
        return Ok(TuiSource::NotRequested);
    };
    let path = directory.join(&name);
    if !path.try_exists()? {
        return Ok(TuiSource::NotRequested);
    }
    let metadata = std::fs::symlink_metadata(&path)?;
    if !metadata.file_type().is_file() {
        return Ok(TuiSource::Failed {
            asset: name,
            reason: "archive is not a regular file".to_owned(),
        });
    }
    match std::fs::read(&path) {
        Ok(bytes) => Ok(TuiSource::Archive { asset: name, bytes }),
        Err(error) => Ok(TuiSource::Failed {
            asset: name,
            reason: error.to_string(),
        }),
    }
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
    let tui = if source_binary.is_some() {
        TuiSource::NotRequested
    } else {
        directory_tui_source(directory)?
    };
    let (binary, _tui) = stage_package(&read_asset, &release, input, tui, version, version, versions)?;
    Ok(binary)
}

fn stage_package(
    read_asset: &dyn Fn(&str) -> Result<Vec<u8>>,
    release: &super::GitHubRelease,
    input: AgentInput<'_>,
    tui: TuiSource,
    version: &str,
    directory_version: &str,
    versions: &Path,
) -> Result<(PathBuf, TuiOutcome)> {
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

    let tui = stage_tui(tui, &sums, versions);
    let destination = versions.join(directory_version);
    let outcome = match std::fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            let installed = destination.join(crate::build::binary_name());
            if super::digest::file_sha256(&binary)? != super::digest::file_sha256(&installed)? {
                bail!("the installed agent version already contains different bytes");
            }
            // An existing version is immutable, so the client is only added when absent.
            settle_tui(tui, |asset, files| place_tui_files(&destination, asset, files))
        }
        Ok(_) => bail!("the installed version path is not a real directory"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let outcome = settle_tui(tui, |asset, files| {
                move_tui_files(files, staging.path())
                    .map(|()| TuiOutcome::Installed)
                    .map_err(|error| (asset.to_owned(), error))
            });
            std::fs::rename(staging.path(), &destination)?;
            outcome
        }
        Err(error) => return Err(error.into()),
    };
    announce_tui(&outcome);
    Ok((destination.join(crate::build::binary_name()), outcome))
}

/// Move the staged client into `destination` only when the target name is free.
fn place_tui_files(
    destination: &Path,
    asset: &str,
    files: Vec<PathBuf>,
) -> Result<TuiOutcome, (String, anyhow::Error)> {
    let mut moved = Vec::new();
    for path in files {
        let name = match path.file_name() {
            Some(name) => name.to_owned(),
            None => continue,
        };
        let target = destination.join(&name);
        match target.try_exists() {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => return Err((asset.to_owned(), error.into())),
        }
        if let Err(error) = std::fs::rename(&path, &target) {
            rollback_tui(&moved);
            return Err((asset.to_owned(), error.into()));
        }
        moved.push(target);
    }
    if moved.is_empty() {
        Ok(TuiOutcome::AlreadyPresent)
    } else {
        Ok(TuiOutcome::Installed)
    }
}

/// Move every staged file under one directory, all or nothing.
fn move_tui_files(files: Vec<PathBuf>, destination: &Path) -> Result<()> {
    let mut moved = Vec::new();
    for path in files {
        let name = path
            .file_name()
            .context("staged interactive client has no name")?
            .to_owned();
        let target = destination.join(name);
        if let Err(error) = std::fs::rename(&path, &target) {
            rollback_tui(&moved);
            return Err(error.into());
        }
        moved.push(target);
    }
    Ok(())
}

fn rollback_tui(moved: &[PathBuf]) {
    for path in moved {
        if let Err(error) = std::fs::remove_file(path) {
            crate::logging::warn(&format!("could not remove {}: {error}", path.display()));
        }
    }
}

/// Turn the verified archive into files ready to place, or into the reason it was skipped.
fn stage_tui(source: TuiSource, sums: &str, versions: &Path) -> StagedTui {
    let (asset, bytes) = match source {
        TuiSource::NotRequested => return StagedTui::Settled(TuiOutcome::NotRequested),
        TuiSource::Missing { asset } => return StagedTui::Settled(TuiOutcome::Missing { asset }),
        TuiSource::Failed { asset, reason } => return StagedTui::Settled(TuiOutcome::Skipped { asset, reason }),
        TuiSource::Archive { asset, bytes } => (asset, bytes),
    };
    let skip = |reason: String| {
        StagedTui::Settled(TuiOutcome::Skipped {
            asset: asset.clone(),
            reason,
        })
    };
    if let Err(error) = super::verify_asset_checksum_text(sums, &asset, &bytes) {
        return skip(format!("{error:#}"));
    }
    let stage = match tempfile::Builder::new().prefix(".tui-stage-").tempdir_in(versions) {
        Ok(stage) => stage,
        Err(error) => return skip(error.to_string()),
    };
    let files = match extract_archive(&bytes, stage.path()) {
        Ok(files) => files,
        Err(error) => return skip(format!("{error:#}")),
    };
    let base = asset.trim_end_matches(".tar.gz");
    let payload = format!("{base}.bin");
    let windows = format!("{base}.exe");
    let mut candidate = None;
    for path in &files {
        let name = match path.file_name().and_then(|name| name.to_str()) {
            Some(name) if path.parent() == Some(stage.path()) => name.to_owned(),
            _ => return skip("archive contains a nested path".to_owned()),
        };
        if name == base || name == windows {
            if candidate.replace(path.clone()).is_some() {
                return skip("archive must contain exactly one interactive client executable".to_owned());
            }
        } else if name != payload {
            return skip(format!("archive contains an unexpected member: {name}"));
        }
    }
    let Some(candidate) = candidate else {
        return skip("archive must contain exactly one interactive client executable".to_owned());
    };
    let renamed = stage.path().join(tui_binary_name());
    if let Err(error) = std::fs::rename(&candidate, &renamed) {
        return skip(error.to_string());
    }
    let files: Vec<PathBuf> = files
        .into_iter()
        .map(|path| if path == candidate { renamed.clone() } else { path })
        .collect();
    for path in &files {
        if let Err(error) = horon_agent_platform::set_permissions_executable(path) {
            return skip(error.to_string());
        }
    }
    StagedTui::Ready { asset, files, stage }
}

/// Resolve a staged client through `place`, turning a failure into one warning.
fn settle_tui<F>(staged: StagedTui, place: F) -> TuiOutcome
where
    F: FnOnce(&str, Vec<PathBuf>) -> Result<TuiOutcome, (String, anyhow::Error)>,
{
    match staged {
        StagedTui::Settled(outcome) => outcome,
        StagedTui::Ready { asset, files, stage } => {
            let outcome = match place(&asset, files) {
                Ok(outcome) => outcome,
                Err((asset, error)) => TuiOutcome::Skipped {
                    asset,
                    reason: format!("{error:#}"),
                },
            };
            drop(stage);
            outcome
        }
    }
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

    /// A tarred archive whose members each carry their own bytes.
    fn archive_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        for (name, contents) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_mode(0o755);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(contents.len() as u64);
            header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
            header.set_cksum();
            builder.append(&header, *contents).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    /// Add an interactive-client archive to an existing asset directory.
    fn add_tui_archive(directory: &Path, members: &[(&str, &[u8])]) -> String {
        let name = tui_asset_name(super::super::get_asset_name()).unwrap();
        let bytes = archive_with(members);
        let mut sums = std::fs::read_to_string(directory.join("SHA256SUMS")).unwrap();
        sums.push_str(&format!("{:x}  {name}\n", Sha256::digest(&bytes)));
        std::fs::write(directory.join(&name), &bytes).unwrap();
        std::fs::write(directory.join("SHA256SUMS"), sums).unwrap();
        name
    }

    /// The interactive client's own member name for the running platform.
    fn tui_tarball_member() -> String {
        tui_asset_name(super::super::get_asset_name())
            .unwrap()
            .trim_end_matches(".tar.gz")
            .to_owned()
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
    fn the_interactive_client_lands_beside_the_agent() {
        let assets = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        release_directory(assets.path(), b"agent-binary");
        let member = tui_tarball_member();
        add_tui_archive(assets.path(), &[(&member, b"tui-binary")]);

        let installed = stage_local(assets.path(), "1.2.3", None, versions.path()).unwrap();

        let directory = installed.parent().unwrap();
        assert_eq!(directory, versions.path().join("1.2.3"));
        assert_eq!(std::fs::read(&installed).unwrap(), b"agent-binary");
        assert_eq!(std::fs::read(directory.join(tui_binary_name())).unwrap(), b"tui-binary");
    }

    #[test]
    fn a_compat_payload_beside_the_wrapper_is_kept_and_executable() {
        let assets = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        release_directory(assets.path(), b"agent-binary");
        let base = tui_tarball_member();
        let payload = format!("{base}.bin");
        add_tui_archive(assets.path(), &[(&base, b"wrapper"), (&payload, b"payload")]);

        let installed = stage_local(assets.path(), "1.2.3", None, versions.path()).unwrap();

        let directory = installed.parent().unwrap();
        let payload = directory.join(&payload);
        assert_eq!(std::fs::read(&payload).unwrap(), b"payload");
        assert!(horon_agent_platform::fs::is_executable(&payload));
        assert!(horon_agent_platform::fs::is_executable(
            &directory.join(tui_binary_name())
        ));
    }

    #[test]
    fn a_release_without_the_client_still_installs_the_agent() {
        let assets = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        release_directory(assets.path(), b"agent-binary");

        let installed = stage_local(assets.path(), "1.2.3", None, versions.path()).unwrap();

        assert_eq!(std::fs::read(&installed).unwrap(), b"agent-binary");
        assert!(!installed.parent().unwrap().join(tui_binary_name()).exists());
        assert_eq!(tui_warning(&TuiOutcome::NotRequested), None);
        assert_eq!(tui_warning(&TuiOutcome::Installed), None);
        assert_eq!(tui_warning(&TuiOutcome::AlreadyPresent), None);
    }

    #[test]
    fn a_client_with_a_bad_checksum_is_skipped_and_named() {
        let assets = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        release_directory(assets.path(), b"agent-binary");
        let member = tui_tarball_member();
        let name = add_tui_archive(assets.path(), &[(&member, b"tui-binary")]);
        let sums = std::fs::read_to_string(assets.path().join("SHA256SUMS")).unwrap();
        let zeroed = format!("{}  {name}", "0".repeat(64));
        let corrupted: String = sums
            .lines()
            .map(|line| {
                if line.ends_with(&name) {
                    format!("{zeroed}\n")
                } else {
                    format!("{line}\n")
                }
            })
            .collect();
        assert!(corrupted.contains(&zeroed));
        std::fs::write(assets.path().join("SHA256SUMS"), &corrupted).unwrap();

        let installed = stage_local(assets.path(), "1.2.3", None, versions.path()).unwrap();

        assert_eq!(std::fs::read(&installed).unwrap(), b"agent-binary");
        assert!(!installed.parent().unwrap().join(tui_binary_name()).exists());
        // The warning text is built from the real verification failure.
        let reason = super::super::verify_asset_checksum_text(&corrupted, &name, b"tui-binary")
            .unwrap_err()
            .to_string();
        let warning = tui_warning(&TuiOutcome::Skipped {
            asset: name.clone(),
            reason,
        })
        .unwrap();
        assert!(warning.contains(&name), "{warning}");
        assert!(warning.contains("could not install"), "{warning}");
        assert!(warning.contains("Checksum mismatch"), "{warning}");
    }

    #[test]
    fn a_client_archive_with_a_nested_member_is_skipped() {
        let assets = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        release_directory(assets.path(), b"agent-binary");
        add_tui_archive(assets.path(), &[("nested/horon-tui", b"tui-binary")]);

        let installed = stage_local(assets.path(), "1.2.3", None, versions.path()).unwrap();

        assert_eq!(std::fs::read(&installed).unwrap(), b"agent-binary");
        assert!(!installed.parent().unwrap().join(tui_binary_name()).exists());
    }

    #[test]
    fn a_client_is_added_to_an_already_installed_version() {
        let assets = tempfile::tempdir().unwrap();
        let versions = tempfile::tempdir().unwrap();
        release_directory(assets.path(), b"agent-binary");
        let installed = stage_local(assets.path(), "1.2.3", None, versions.path()).unwrap();
        let directory = installed.parent().unwrap().to_path_buf();
        let member = tui_tarball_member();
        add_tui_archive(assets.path(), &[(&member, b"tui-binary")]);

        stage_local(assets.path(), "1.2.3", None, versions.path()).unwrap();

        assert_eq!(std::fs::read(directory.join(tui_binary_name())).unwrap(), b"tui-binary");
        assert_eq!(
            std::fs::read(directory.join(crate::build::binary_name())).unwrap(),
            b"agent-binary"
        );
    }

    #[test]
    fn the_client_asset_name_mirrors_the_agent_asset_name() {
        assert_eq!(
            tui_asset_name("horon-agent-linux-x86_64").as_deref(),
            Some("horon-tui-linux-x86_64.tar.gz")
        );
        assert_eq!(
            tui_asset_name("horon-agent-macos-aarch64").as_deref(),
            Some("horon-tui-macos-aarch64.tar.gz")
        );
        assert_eq!(
            tui_asset_name("horon-agent-windows-x86_64.exe").as_deref(),
            Some("horon-tui-windows-x86_64.tar.gz")
        );
        assert_eq!(
            tui_asset_name("horon-agent-windows-aarch64.exe").as_deref(),
            Some("horon-tui-windows-aarch64.tar.gz")
        );
        assert_eq!(tui_asset_name("horon-agent-freebsd-x86_64"), None);
        assert_eq!(tui_asset_name("horon-agent-unknown"), None);
        assert_eq!(tui_asset_name("horon-other"), None);
        assert_eq!(tui_binary_name().trim_end_matches(".exe"), "horon-tui");
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
