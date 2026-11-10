//! Installing a whole skill directory into the managed skills directory.
//!
//! `skill_manage install` has two sources: an endorsed catalog entry's fixed
//! repository and a git source the caller names. Both end the same way: one
//! directory under `~/.horon/managed-skills/<name>/` that the registry already
//! scans, plus one record in the install lock file that says where the
//! directory came from and at which commit.
//!
//! Nothing is written into the managed directory unless the whole install can
//! finish: the clone lands in a staging directory beside the target, the
//! `SKILL.md` is parsed and its name validated, and only then does one rename
//! move it into place. A failure removes the staging directory, so a refused
//! install leaves the managed directory exactly as it was.
//!
//! The source is passed to `git` as an argument and never through a shell: an
//! endorsed install runs the catalog's fixed repository, and a caller-supplied
//! source is whatever git accepts as a remote.

use crate::managed_skills;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;

/// File inside the managed directory that records every installed skill.
pub const LOCK_FILE: &str = "install-lock.json";

/// One row of the lock file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRecord {
    /// The skill name, which is also its directory name.
    pub name: String,
    /// The repository or path the skill came from.
    pub source: String,
    /// The requested branch, tag, or commit, when one was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    /// The commit the clone resolved to.
    pub commit: String,
}

/// What an install produced.
#[derive(Clone, Debug)]
pub struct InstalledSkill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub source: String,
    pub commit: String,
}

impl InstalledSkill {
    /// The lock row this install writes.
    fn record(&self, git_ref: Option<&str>) -> InstallRecord {
        InstallRecord {
            name: self.name.clone(),
            source: self.source.clone(),
            git_ref: git_ref.map(str::to_string),
            commit: self.commit.clone(),
        }
    }
}

/// The lock file's contents.
#[derive(Debug, Default, Serialize, Deserialize)]
struct LockFile {
    #[serde(default)]
    installs: Vec<InstallRecord>,
}

/// Installs the skill at `source` into the managed directory.
///
/// `git_ref` names a branch, tag, or commit; without it the remote's default
/// branch is used. `name_hint` selects the subdirectory to take when the
/// repository holds more than one skill.
pub fn install(source: &str, git_ref: Option<&str>, name_hint: Option<&str>) -> Result<InstalledSkill> {
    let source = source.trim();
    if source.is_empty() {
        bail!("An install needs a git source such as https://github.com/owner/repo.git.");
    }
    let dir = managed_skills::dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;

    let staging = dir.join(format!(".staging-{}-{}", std::process::id(), unique_suffix()));
    if staging.exists() {
        std::fs::remove_dir_all(&staging).ok();
    }
    let installed = clone_into(source, git_ref, name_hint, &staging);
    match installed {
        Ok(installed) => {
            let result = move_into_place(&installed, &dir);
            std::fs::remove_dir_all(&staging).ok();
            let installed = result?;
            write_lock(&dir, installed.record(git_ref))?;
            Ok(installed)
        }
        Err(error) => {
            std::fs::remove_dir_all(&staging).ok();
            Err(error)
        }
    }
}

/// Installs an endorsed catalog entry from the repository its source names.
///
/// The catalog records where the skill comes from as `owner/repo (note)`, so
/// the first token is the repository; anything else is reported as a catalog
/// entry this action cannot install rather than guessed at.
pub fn install_endorsed(entry: &crate::skill::EndorsedSkill) -> Result<InstalledSkill> {
    let repo = endorsed_repo(entry).ok_or_else(|| {
            anyhow::anyhow!(
                "The endorsed skill '{}' names the source \"{}\", which is not a git repository this action can install. {}",
                entry.name,
                entry.source,
                match entry.install {
                    Some(command) => format!("Install it with `{command}`."),
                    None => "Install it by hand into ~/.horon/managed-skills/.".to_string(),
                }
            )
        })?;
    install(&repo, None, Some(entry.name))
}

/// The git URL an endorsed entry's source names, when it names a repository.
///
/// The catalog writes the source as `owner/repo (note)`, so the first token is
/// the repository; a note that does not start with a repository returns `None`
/// and the caller reports the entry instead of guessing.
pub fn endorsed_repo(entry: &crate::skill::EndorsedSkill) -> Option<String> {
    let repo = entry.source.split_whitespace().next()?;
    (repo.split('/').count() == 2 && !repo.contains("://") && !repo.starts_with('.'))
        .then(|| format!("https://github.com/{repo}.git"))
}

/// Looks up an endorsed skill by name.
pub fn endorsed(name: &str) -> Option<&'static crate::skill::EndorsedSkill> {
    crate::skill::endorsed_skills().iter().find(|entry| entry.name == name)
}

fn unique_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

fn clone_into(source: &str, git_ref: Option<&str>, name_hint: Option<&str>, staging: &Path) -> Result<InstalledSkill> {
    let mut command = Command::new("git");
    command.arg("clone").arg("--depth").arg("1").arg("--quiet");
    if let Some(git_ref) = git_ref {
        command.arg("--branch").arg(git_ref);
    }
    command
        .arg("--")
        .arg(source)
        .arg(staging)
        // A credential prompt would hang a headless daemon; a private source
        // should fail with git's own error instead.
        .env("GIT_TERMINAL_PROMPT", "0");
    let output = command
        .output()
        .with_context(|| format!("cannot run git to install from {source}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("Cannot install from {source}: {}", stderr.trim());
    }

    let commit = git_output(staging, &["rev-parse", "HEAD"])
        .with_context(|| format!("cannot read the commit of the clone from {source}"))?;
    let (root, description) = locate_skill(staging, name_hint)?;
    let name = managed_skills::validate_name(&skill_name(&root)?)?;
    let target = managed_skills::skill_file(&name)?;
    if let Some(parent) = target.parent()
        && parent.exists()
    {
        bail!(
            "A skill named '{name}' already exists at {}. Installing over an existing skill is not what this action does; remove it first.",
            parent.display()
        );
    }
    Ok(InstalledSkill {
        name,
        description,
        path: root,
        source: source.to_string(),
        commit,
    })
}

fn git_output(dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .with_context(|| format!("cannot run git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The skill directory inside a clone, and its description.
///
/// A repository may hold one skill at its root, one skill in a directory named
/// after the hint, or exactly one subdirectory with a `SKILL.md`. Anything else
/// is ambiguous and refused with the candidates named.
fn locate_skill(clone: &Path, name_hint: Option<&str>) -> Result<(PathBuf, String)> {
    if clone.join("SKILL.md").is_file() {
        let description = crate::skill::frontmatter::read_frontmatter(&clone.join("SKILL.md"))?
            .0
            .description;
        return Ok((clone.to_path_buf(), description));
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(clone).with_context(|| format!("cannot list {}", clone.display()))? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if path.join("SKILL.md").is_file() {
            candidates.push(path);
        }
    }
    if let Some(hint) = name_hint
        && let Some(found) = candidates
            .iter()
            .find(|path| path.file_name().is_some_and(|name| name == hint))
    {
        let description = crate::skill::frontmatter::read_frontmatter(&found.join("SKILL.md"))?
            .0
            .description;
        return Ok((found.clone(), description));
    }
    match candidates.len() {
        1 => {
            let root = candidates.remove(0);
            let description = crate::skill::frontmatter::read_frontmatter(&root.join("SKILL.md"))?
                .0
                .description;
            Ok((root, description))
        }
        0 => bail!("The source holds no SKILL.md, at its root or in a subdirectory, so there is no skill to install."),
        _ => bail!(
            "The source holds {} skills and none is named '{}'; name the skill with `name` to choose one.",
            candidates.len(),
            name_hint.unwrap_or("<none>")
        ),
    }
}

fn skill_name(root: &Path) -> Result<String> {
    let (frontmatter, _) = crate::skill::frontmatter::read_frontmatter(&root.join("SKILL.md"))?;
    if frontmatter.name.trim().is_empty() {
        return match root.file_name() {
            Some(name) => Ok(name.to_string_lossy().to_string()),
            None => bail!("The skill has no name in its frontmatter and its directory has no name either."),
        };
    }
    Ok(frontmatter.name)
}

/// Moves the staged skill into its final directory and returns the record.
fn move_into_place(installed: &InstalledSkill, dir: &Path) -> Result<InstalledSkill> {
    let target = dir.join(&installed.name);
    if target.exists() {
        bail!(
            "A skill named '{}' already exists at {}. Installing over an existing skill is not what this action does; remove it first.",
            installed.name,
            target.display()
        );
    }
    std::fs::rename(&installed.path, &target)
        .with_context(|| format!("cannot move the installed skill into {}", target.display()))?;
    Ok(InstalledSkill {
        path: target,
        ..installed.clone()
    })
}

fn write_lock(dir: &Path, record: InstallRecord) -> Result<()> {
    let path = dir.join(LOCK_FILE);
    let mut lock = match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<LockFile>(&bytes)
            .with_context(|| format!("{} is not an install lock this action can extend", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => LockFile::default(),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    lock.installs.retain(|existing| existing.name != record.name);
    lock.installs.push(record);
    lock.installs.sort_by(|left, right| left.name.cmp(&right.name));
    let temporary = dir.join(format!(".{LOCK_FILE}.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(&lock)?;
    std::fs::write(&temporary, bytes).with_context(|| format!("cannot write {}", temporary.display()))?;
    std::fs::rename(&temporary, &path).with_context(|| format!("cannot replace {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
#[path = "skill_install_tests.rs"]
mod tests;
