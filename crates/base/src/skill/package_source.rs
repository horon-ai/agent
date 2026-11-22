//! Package skills: the lowest-priority skill source.
//!
//! A plugin declares its skills as files, so they load with the package
//! generation that captured the installed copy instead of from a directory
//! scan. Two rules keep the operator in charge:
//!
//! - The public name is `<package id>--<declared name>`, so a package cannot
//!   claim a bare name that another source already uses.
//! - When even that name is taken, the package entry is the one shadowed. An
//!   instruction a person wrote is never replaced by a package, and the package
//!   copy is not renamed into a namespace either, because it already had one.
//!
//! Bodies stay lazy: the entry points at the installed `SKILL.md`, and the body
//! cache reads it on first use and again whenever that file's identity moves.

use super::body;
use super::{Skill, SkillRegistry};
use crate::packages::PackageTextResources;

impl SkillRegistry {
    /// Compose the package skills of `resources` over `base`.
    ///
    /// Every name `base` already claims stays; a package skill is inserted only
    /// under a free public name. The result is a copy, so the shared global
    /// registry is never mutated by a package, and a removal simply stops
    /// contributing entries to the next composition.
    pub fn with_package_skills(base: &Self, resources: &PackageTextResources) -> Self {
        let mut registry = base.clone();
        for skill in resources.skills() {
            if registry.contains(&skill.name) {
                crate::logging::debug(&format!(
                    "Skills: package '{}' skill '{}' is shadowed by an existing skill",
                    skill.package_id, skill.name
                ));
                continue;
            }
            registry.insert_skill(Skill {
                name: skill.name.clone(),
                description: skill.description.clone(),
                allowed_tools: skill.allowed_tools.clone(),
                path: skill.path.clone(),
                body: body::new_cache(),
            });
        }
        registry
    }
}
