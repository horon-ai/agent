//! What happens when two sources offer a skill with the same name.
//!
//! One name can be claimed once per level, so an alternative needs an address
//! the operator can still reach. These rules decide what happens:
//!
//! - A name that carries a path separator is refused: `/` belongs to the
//!   `<namespace>/<name>` form and to `skill://<name>/<path>`, so a frontmatter
//!   name must never be able to claim a namespaced address.
//! - Two files with the same bytes and the same frontmatter are one skill. The
//!   later copy is dropped without a warning, so an ordinary install that also
//!   ships a plugin copy does not fill the log.
//! - Different content keeps both: the earlier source (the priority table
//!   decides that order) keeps the bare name and the later one becomes
//!   `<namespace>/<name>`, or `<namespace>/<name>~2`, `~3` while that slot is
//!   taken.
//! - A managed skill never takes a namespace. The model may add a procedure,
//!   never a second identity for instructions a person wrote, so it stays out
//!   of the list and the collision is reported instead.
//!
//! The identical-copy test is the one place discovery opens a whole body. It
//! runs only when a name actually repeats, and the reads are counted like every
//! other skill file read.

use super::Skill;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// One skill offered by one source. The namespace it would take on a
/// collision is derived from its path and `source_id` only when a collision
/// actually happens, so a catalog without repeats pays nothing for it.
pub(super) struct Candidate {
    pub(super) skill: Skill,
    pub(super) source_id: &'static str,
    pub(super) managed: bool,
}

/// The surviving skills and one line per collision worth reporting.
pub(super) struct Resolution {
    pub(super) skills: Vec<Skill>,
    pub(super) warnings: Vec<String>,
}

/// Whether two skills declare the same frontmatter, apart from the name.
///
/// The name is not compared: a collision renames the skill it accepts, so
/// comparing `skill.name` would make a third copy of the same file look
/// different from the renamed second one. Callers match on the name the
/// frontmatter declared first.
fn same_frontmatter(a: &Skill, b: &Skill) -> bool {
    a.description == b.description && a.allowed_tools == b.allowed_tools
}

/// Size and digest of a whole file, read only to prove two candidates equal.
#[derive(PartialEq, Eq)]
struct Identity {
    size: u64,
    digest: [u8; 32],
}

impl Identity {
    fn read(path: &Path) -> Option<Self> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            // A file that cannot be read now is not proven equal: the caller
            // keeps both skills rather than merging them on a guess.
            Err(error) => {
                crate::logging::debug(&format!("Skills: cannot compare {} ({error})", path.display()));
                return None;
            }
        };
        horon_agent_perf_counters::record_skill_file_read(bytes.len() as u64);
        Some(Self {
            size: bytes.len() as u64,
            digest: Sha256::digest(&bytes).into(),
        })
    }
}

/// One accepted skill, under the name it took. `resolve` files it under the
/// frontmatter name even after a collision renames it, so a later copy
/// collides with the original name, not with the `<namespace>/<name>` address
/// this one already occupies.
struct Admitted {
    skill: Skill,
    identity: Option<Identity>,
}

impl Admitted {
    fn new(skill: Skill) -> Self {
        Self { skill, identity: None }
    }

    /// Prove this skill and `path` hold the same bytes, reading lazily.
    fn same_file_as(&mut self, path: &Path) -> bool {
        let Some(identity) = Identity::read(path) else {
            return false;
        };
        if self.identity.is_none() {
            self.identity = Identity::read(&self.skill.path);
        }
        self.identity.as_ref() == Some(&identity)
    }
}

/// Resolve same-name candidates, keeping the earlier one under the bare name.
///
/// Candidates must arrive highest priority first, and in path order inside one
/// source, so the result never depends on how a directory was enumerated.
pub(super) fn resolve(candidates: Vec<Candidate>) -> Resolution {
    let mut admitted: Vec<Admitted> = Vec::with_capacity(candidates.len());
    // Every accepted copy of a frontmatter name, first one first. A lookup here
    // replaces a scan of `admitted`, which made a large catalog quadratic.
    let mut by_name: HashMap<String, Vec<usize>> = HashMap::with_capacity(candidates.len());
    let mut taken: HashSet<String> = HashSet::with_capacity(candidates.len());
    let mut warnings = Vec::new();

    for candidate in candidates {
        let Candidate {
            mut skill,
            source_id,
            managed,
        } = candidate;
        let raw_name = skill.name.clone();
        if raw_name.contains(['/', '\\']) {
            warnings.push(format!(
                "Skills: name collision: \"{raw_name}\" from {} contains a path separator; skipping",
                skill.path.display()
            ));
            continue;
        }

        let Some(copies) = by_name.get(&raw_name) else {
            taken.insert(raw_name.clone());
            by_name.insert(raw_name, vec![admitted.len()]);
            admitted.push(Admitted::new(skill));
            continue;
        };
        let index = copies[0];

        // Identical means byte-equal and frontmatter-equal to any copy of this
        // name already accepted, not just the bare one: a third source shipping
        // the same file as the second is still one skill, not a new slot.
        if identical_to_any(&mut admitted, copies, &skill) {
            continue;
        }
        let path = skill.path.display().to_string();
        let existing_path = admitted[index].skill.path.display().to_string();
        if managed {
            warnings.push(format!(
                "Skills: name collision: \"{raw_name}\" from {path} is hidden by {existing_path}; \
                 managed skills never take a namespace"
            ));
            continue;
        }
        let name = free_slot(&namespace_for(&skill.path, source_id), &raw_name, &taken);
        warnings.push(format!(
            "Skills: name collision: \"{raw_name}\" from {path} differs from {existing_path}; \
             available as \"{name}\""
        ));
        skill.name = name.clone();
        taken.insert(name);
        if let Some(copies) = by_name.get_mut(&raw_name) {
            copies.push(admitted.len());
        }
        admitted.push(Admitted::new(skill));
    }

    let mut skills: Vec<Skill> = admitted.into_iter().map(|entry| entry.skill).collect();
    // A namespaced candidate can be accepted after a bare one from another
    // source, so the list is sorted like every other registry listing: two
    // merges of the same directories must produce the same order.
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    Resolution { skills, warnings }
}

/// Whether `candidate` repeats one of the accepted `copies` of its name,
/// compared by frontmatter and whole-file bytes.
fn identical_to_any(admitted: &mut [Admitted], copies: &[usize], candidate: &Skill) -> bool {
    copies.iter().any(|&index| {
        let entry = &mut admitted[index];
        same_frontmatter(&entry.skill, candidate) && entry.same_file_as(&candidate.path)
    })
}

/// `<namespace>/<raw>`, or the first free `~2`, `~3` suffix for that address.
fn free_slot(namespace: &str, raw: &str, taken: &HashSet<String>) -> String {
    let mut name = format!("{namespace}/{raw}");
    let mut suffix = 2;
    while taken.contains(&name) {
        name = format!("{namespace}/{raw}~{suffix}");
        suffix += 1;
    }
    name
}

/// Namespace for a skill found at `path` inside the source `source_id`.
///
/// The folder that owns `skills/` names the skill, so an installed plugin keeps
/// its own name. A marketplace cache names that folder
/// `<marketplace>___<plugin>___<version>`, and a plugin store often puts a
/// version below the plugin, so both are unwrapped. A dotted or empty name is
/// not an address anyone can type, and the source id is.
pub(super) fn namespace_for(path: &Path, source_id: &str) -> String {
    let segments: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect();
    let owner = match segments.iter().rposition(|segment| segment == "skills") {
        Some(0) | None => None,
        Some(index) => {
            let candidate = &segments[index - 1];
            if version_like(candidate) {
                index.checked_sub(2).map(|parent| segments[parent].as_str())
            } else {
                Some(candidate.as_str())
            }
        }
    };
    let owner = owner.unwrap_or_else(|| {
        segments
            .len()
            .checked_sub(3)
            .map(|index| segments[index].as_str())
            .unwrap_or("")
    });
    let owner = match owner.split("___").collect::<Vec<&str>>().as_slice() {
        [_, plugin, _] => *plugin,
        _ => owner,
    };
    if owner.is_empty() || owner.starts_with('.') {
        return source_id.to_string();
    }
    sanitize_namespace(owner).unwrap_or_else(|| source_id.to_string())
}

/// Whether a path segment is a version (`1.0.0`, `0.1`) rather than a name.
fn version_like(segment: &str) -> bool {
    let mut characters = segment.chars();
    characters.next().is_some_and(|ch| ch.is_ascii_digit()) && characters.all(|ch| ch.is_ascii_digit() || ch == '.')
}

/// Reduce a folder name to one typed token, or `None` when nothing is left.
///
/// A namespace is addressed as `/skill:<ns>/<name>` and `skill://<ns>/<name>`,
/// so whitespace and punctuation would split it into more than one token.
fn sanitize_namespace(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len());
    let mut pending_dash = false;
    for ch in raw.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '-' {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(ch);
        } else {
            pending_dash = true;
        }
    }
    (!out.is_empty()).then_some(out)
}

impl super::SkillRegistry {
    /// Warnings produced by the load that built this registry, already written
    /// to the log. `skill_manage list` shows them so a namespaced skill is
    /// explained where the operator looks for it.
    pub fn collision_warnings(&self) -> &[String] {
        &self.collision_warnings
    }

    /// Report one merge's warnings to the log and keep them for the list.
    ///
    /// Warnings accumulate: a registry holds the global load first and then the
    /// project overlay, and both collisions are real and both apply to the
    /// effective set. Identical lines are kept once so composing the same
    /// overlay twice cannot repeat a line.
    pub(super) fn record_collision_warnings(&mut self, warnings: Vec<String>) {
        for warning in &warnings {
            crate::logging::warn(warning);
        }
        self.extend_collision_warnings(&warnings);
    }

    /// Append `more`, keeping the first occurrence of each distinct line.
    pub(super) fn extend_collision_warnings(&mut self, more: &[String]) {
        if more.is_empty() {
            return;
        }
        let mut all = self.collision_warnings.to_vec();
        for warning in more {
            if !all.contains(warning) {
                all.push(warning.clone());
            }
        }
        self.collision_warnings = all.into();
    }

    /// Drop the warnings of the previous load, for a load that replaces it.
    pub(super) fn clear_collision_warnings(&mut self) {
        self.collision_warnings = Vec::new().into();
    }
}
