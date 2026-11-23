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
            if registry.skills.contains_key(&skill.name) {
                if std::env::var_os("HORON_AGENT_TRACE").is_some() {
                    crate::logging::debug(&format!(
                        "Skills: package '{}' skill '{}' is shadowed by an existing skill",
                        skill.package_id, skill.name
                    ));
                }
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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::packages::{InstallRequest, PackageScope, PackageStore, PackageTextCache};
    use crate::skill::NO_SKILLS_ENV;
    use crate::skill::test_support::test_skill;

    fn write_plugin_with_skill(root: &Path, id: &str, skill_name: &str, body: &str) {
        let ext_dir = root.join("native");
        fs::create_dir_all(ext_dir.join("bin")).expect("bin dir");
        let bin_name = if std::env::consts::OS == "windows" {
            "bin/child.exe"
        } else {
            "bin/child"
        };
        fs::copy(std::env::current_exe().expect("current exe"), ext_dir.join(bin_name)).expect("copy bin");
        let target = horon_agent_platform::fs::native_package_target().expect("native target");
        fs::write(
            ext_dir.join("extension.toml"),
            format!(
                "schema_version = 1\nid = \"{id}\"\nversion = \"1.0.0\"\nname = \"Pkg\"\nprotocol = \"1.0\"\n\n[targets]\n{target} = \"{bin_name}\"\n"
            ),
        )
        .expect("extension.toml");

        let skill_dir = root.join("skills").join(skill_name);
        fs::create_dir_all(&skill_dir).expect("skill dir");
        fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {skill_name}\ndescription: Package skill\n---\n\n{body}\n"),
        )
        .expect("SKILL.md");

        fs::write(
            root.join("plugin.toml"),
            format!(
                "schema_version = 1\nid = \"{id}\"\nversion = \"1.0.0\"\nname = \"Pkg\"\nextension = \"native/extension.toml\"\n\n[resources]\nskills = [\"skills/{skill_name}/SKILL.md\"]\n"
            ),
        )
        .expect("plugin.toml");
    }

    #[test]
    fn with_package_skills_does_not_overwrite_hidden_user_skill_under_no_skills() {
        let _lock = crate::storage::lock_test_env();
        let pkg_src = tempfile::tempdir().expect("pkg src");
        write_plugin_with_skill(pkg_src.path(), "pkg-alpha", "greeting", "PACKAGE_BODY");
        let store_dir = tempfile::tempdir().expect("store dir");
        let store = PackageStore::new(store_dir.path());
        let record = store
            .install(InstallRequest::new(pkg_src.path(), PackageScope::Global))
            .expect("install plugin");
        let resources = PackageTextCache::new().load(&store, None, std::iter::once(&record));

        let mut base = SkillRegistry::default();
        base.insert_skill(test_skill("pkg-alpha--greeting", "User greeting", "USER_BODY"));

        struct NoSkillsGuard(Option<std::ffi::OsString>);
        impl NoSkillsGuard {
            fn enable() -> Self {
                let prev = std::env::var_os(NO_SKILLS_ENV);
                crate::env::set_var(NO_SKILLS_ENV, "1");
                Self(prev)
            }
        }
        impl Drop for NoSkillsGuard {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(val) => crate::env::set_var(NO_SKILLS_ENV, val),
                    None => crate::env::remove_var(NO_SKILLS_ENV),
                }
            }
        }

        let composed = {
            let _no_skills = NoSkillsGuard::enable();
            let composed = SkillRegistry::with_package_skills(&base, &resources);
            assert!(composed.list().is_empty());
            composed
        };

        let preserved = composed
            .get("pkg-alpha--greeting")
            .expect("user skill must remain intact after NO_SKILLS clears");
        assert_eq!(preserved.description, "User greeting");
        assert_eq!(preserved.body().expect("body").as_ref(), "USER_BODY");
    }
}
