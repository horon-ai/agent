//! Package text resources (`SKILL.md`, rules, prompts) as session inputs.
//!
//! Every case installs real plugin packages into a temporary horon home through
//! the real catalog and manifest validation, then drives the loaders a session
//! uses. Nothing here reads or writes the operator's home: `HOME` and
//! `HORON_HOME` both point at the case's temporary directory while it runs.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use horon_agent_base::packages::{
    InstallRequest, PackageRecord, PackageScope, PackageStore, PackageTextCache, PackageTextResources,
};
use horon_agent_base::prompt::{
    SplitSystemPrompt, append_package_rules, build_system_prompt_split, load_base_system_prompt,
};
use horon_agent_base::skill::SkillRegistry;
use horon_agent_platform::fs::make_owner_writable;
use horon_agent_platform::set_permissions_executable;
use tempfile::{TempDir, tempdir};

// ---------------------------------------------------------------------------
// Isolated home
// ---------------------------------------------------------------------------

static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A temporary `HOME` and `HORON_HOME`, held for the duration of one case.
///
/// The temporary home is the package store, the global skill and prompt source
/// and the `AGENTS.md` ancestor all at once. The environment is process-wide, so
/// the lock serializes the cases that set it.
struct TempHome {
    dir: TempDir,
    previous_home: Option<std::ffi::OsString>,
    previous_horon_home: Option<std::ffi::OsString>,
    _lock: MutexGuard<'static, ()>,
}

/// Serialize a case that spawns a child process with the cases that redirect
/// `HOME` and `HORON_HOME`, so no spawn observes the environment mid-change.
pub(super) fn hold_env() -> MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

impl TempHome {
    fn new() -> Self {
        let lock = hold_env();
        let dir = tempdir().expect("a temporary home");
        let previous_home = std::env::var_os("HOME");
        let previous_horon_home = std::env::var_os("HORON_HOME");
        unsafe {
            std::env::set_var("HOME", dir.path());
            std::env::set_var("HORON_HOME", dir.path());
        }
        Self {
            dir,
            previous_home,
            previous_horon_home,
            _lock: lock,
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn store(&self) -> PackageStore {
        PackageStore::for_horon_home(self.path())
    }

    fn workspace(&self, name: &str) -> PathBuf {
        let workspace = self.path().join(name);
        fs::create_dir_all(&workspace).expect("create the workspace");
        workspace
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        unsafe {
            match self.previous_home.take() {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match self.previous_horon_home.take() {
                Some(value) => std::env::set_var("HORON_HOME", value),
                None => std::env::remove_var("HORON_HOME"),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Plugin fixtures
// ---------------------------------------------------------------------------

fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => panic!("package_text needs a supported runner, got {os}/{arch}"),
    }
}

fn binary_name() -> &'static str {
    if std::env::consts::OS == "windows" {
        "bin/child.exe"
    } else {
        "bin/child"
    }
}

/// A minimal executable whose header satisfies the host's native-target check.
///
/// These packages are installed, never started, so the bytes only need to be the
/// kind of file the validator requires.
fn write_binary(path: &Path, target: &str) {
    let bytes = if target.ends_with("-darwin") {
        mach_o(target.starts_with("aarch64-"))
    } else if target.ends_with("-gnu") {
        elf(target.starts_with("aarch64-"))
    } else {
        pe(target.starts_with("aarch64-"))
    };
    fs::write(path, bytes).expect("write the fixture executable");
    set_permissions_executable(path).expect("mark the fixture executable");
}

fn mach_o(arm64: bool) -> Vec<u8> {
    let cpu: u32 = if arm64 { 0x0100_000c } else { 0x0100_0007 };
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&0xfeed_facfu32.to_le_bytes());
    bytes.extend_from_slice(&cpu.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&24u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    // LC_BUILD_VERSION with platform 1 (macOS).
    bytes.extend_from_slice(&0x32u32.to_le_bytes());
    bytes.extend_from_slice(&24u32.to_le_bytes());
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 32]);
    bytes
}

fn elf(arm64: bool) -> Vec<u8> {
    let machine: u16 = if arm64 { 183 } else { 62 };
    let loader: &[u8] = if arm64 {
        b"/lib/ld-linux-aarch64.so.1\0"
    } else {
        b"/lib64/ld-linux-x86-64.so.2\0"
    };
    let program_offset: usize = 64;
    let interpreter_offset: u64 = 120;
    let mut bytes = vec![0u8; interpreter_offset as usize];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[7] = 0;
    bytes[16..18].copy_from_slice(&2u16.to_le_bytes());
    bytes[18..20].copy_from_slice(&machine.to_le_bytes());
    bytes[20..24].copy_from_slice(&[1, 0, 0, 0]);
    bytes[24..32].copy_from_slice(&0x0040_1000u64.to_le_bytes());
    bytes[32..40].copy_from_slice(&(program_offset as u64).to_le_bytes());
    bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
    bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
    bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
    bytes[program_offset..program_offset + 4].copy_from_slice(&3u32.to_le_bytes());
    bytes[program_offset + 8..program_offset + 16].copy_from_slice(&interpreter_offset.to_le_bytes());
    bytes[program_offset + 32..program_offset + 40].copy_from_slice(&(loader.len() as u64).to_le_bytes());
    bytes.extend_from_slice(loader);
    bytes
}

fn pe(arm64: bool) -> Vec<u8> {
    let machine: u16 = if arm64 { 0xaa64 } else { 0x8664 };
    let mut bytes = vec![0u8; 64];
    bytes[..2].copy_from_slice(b"MZ");
    bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
    bytes.extend_from_slice(b"PE\0\0");
    bytes.extend_from_slice(&machine.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 12]);
    bytes.extend_from_slice(&112u16.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&0x020bu16.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 110]);
    bytes.extend_from_slice(&[0u8; 40]);
    bytes
}

/// What one fixture plugin declares.
#[derive(Default)]
struct Spec<'a> {
    /// `(declared frontmatter name, body)`.
    skills: Vec<(&'a str, &'a str)>,
    /// `(file name under `resources/rules/`, text)`.
    rules: Vec<(&'a str, &'a str)>,
    /// `(file name under `resources/prompts/`, text)`.
    prompts: Vec<(&'a str, &'a str)>,
    /// Extra `rules` entries written verbatim, for path-escape cases.
    raw_rules: Vec<&'a str>,
}

fn quoted_list(prefix: &str, entries: &[String]) -> String {
    if entries.is_empty() {
        String::new()
    } else {
        let list = entries
            .iter()
            .map(|entry| format!("\"{entry}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{prefix} = [{list}]\n")
    }
}

/// A plugin package directory that the installer accepts.
fn plugin_source(id: &str, version: &str, spec: &Spec<'_>) -> TempDir {
    let root = tempdir().expect("fixture directory");
    let extension = root.path().join("extension");
    fs::create_dir_all(extension.join("bin")).expect("create the fixture binary directory");
    write_binary(
        &extension
            .join("bin")
            .join(Path::new(binary_name()).file_name().unwrap()),
        host_target(),
    );
    fs::write(
        extension.join("extension.toml"),
        format!(
            r#"
schema_version = 1
id = "{id}"
version = "{version}"
name = "Package {id}"
protocol = "1.0"
[targets]
{target} = "{binary}"
"#,
            target = host_target(),
            binary = binary_name(),
        ),
    )
    .expect("write extension.toml");

    let mut skill_paths = Vec::new();
    for (name, body) in &spec.skills {
        let path = format!("resources/skills/{name}/SKILL.md");
        let file = root.path().join(&path);
        fs::create_dir_all(file.parent().unwrap()).expect("create the skill directory");
        fs::write(
            &file,
            format!("---\nname: {name}\ndescription: Package {id} skill {name}.\n---\n\n{body}\n"),
        )
        .expect("write SKILL.md");
        skill_paths.push(path);
    }
    let mut rule_paths = Vec::new();
    for (name, text) in &spec.rules {
        let path = format!("resources/rules/{name}");
        write_resource(root.path(), &path, text);
        rule_paths.push(path);
    }
    rule_paths.extend(spec.raw_rules.iter().map(|raw| (*raw).to_owned()));
    let mut prompt_paths = Vec::new();
    for (name, text) in &spec.prompts {
        let path = format!("resources/prompts/{name}");
        write_resource(root.path(), &path, text);
        prompt_paths.push(path);
    }
    let resources = format!(
        "{}{}{}",
        quoted_list("skills", &skill_paths),
        quoted_list("rules", &rule_paths),
        quoted_list("prompts", &prompt_paths),
    );
    fs::write(
        root.path().join("plugin.toml"),
        format!(
            r#"
schema_version = 1
id = "{id}"
version = "{version}"
name = "Package {id}"
extension = "extension/extension.toml"

[resources]
{resources}"#,
        ),
    )
    .expect("write plugin.toml");
    root
}

fn write_resource(root: &Path, relative: &str, text: &str) {
    let file = root.join(relative);
    fs::create_dir_all(file.parent().unwrap()).expect("create the resource directory");
    fs::write(file, text).expect("write the resource file");
}

fn install(store: &PackageStore, source: &Path, scope: PackageScope) -> PackageRecord {
    store
        .install(InstallRequest::new(source, scope))
        .expect("install the fixture plugin")
}

fn install_replacing(store: &PackageStore, source: &Path, scope: PackageScope) -> PackageRecord {
    store
        .install(InstallRequest::new(source, scope).replacing())
        .expect("replace the fixture plugin")
}

fn records(store: &PackageStore) -> Vec<PackageRecord> {
    store.catalog().expect("read the catalog").packages().cloned().collect()
}

fn load(
    cache: &PackageTextCache,
    store: &PackageStore,
    workspace: Option<&Path>,
    records: &[PackageRecord],
) -> Arc<PackageTextResources> {
    cache.load(store, workspace, records.iter())
}

// ---------------------------------------------------------------------------
// Generation visibility
// ---------------------------------------------------------------------------

#[test]
fn package_text_resources_follow_the_generation_that_lists_them() {
    let home = TempHome::new();
    let store = home.store();
    let workspace = home.workspace("workspace");
    let cache = PackageTextCache::new();

    let global = plugin_source(
        "user-alpha",
        "1.0.0",
        &Spec {
            skills: vec![("greeting", "Greet in the alpha package.")],
            rules: vec![("greeting.md", "RULE_ALPHA_MARKER")],
            prompts: vec![("greeting.md", "PROMPT_ALPHA_MARKER")],
            ..Spec::default()
        },
    );
    let project = plugin_source(
        "proj-beta",
        "1.0.0",
        &Spec {
            skills: vec![("helper", "Help in the beta package.")],
            ..Spec::default()
        },
    );
    install(&store, global.path(), PackageScope::Global);
    install(
        &store,
        project.path(),
        PackageScope::project(&workspace).expect("project scope"),
    );

    // The turn captured before either install lists no package, so it exposes
    // nothing even though the store already holds both.
    let before = load(&cache, &store, Some(&workspace), &[]);
    assert!(before.is_empty(), "a generation with no package exposes no resource");

    let after = load(&cache, &store, Some(&workspace), &records(&store));
    let names: Vec<&str> = after.skills().map(|skill| skill.name.as_str()).collect();
    assert_eq!(names, ["proj-beta--helper", "user-alpha--greeting"]);
    assert_eq!(after.prompt("user-alpha--greeting"), Some("PROMPT_ALPHA_MARKER"));
    assert_eq!(after.rules().count(), 1);

    // The user-scoped package follows the session anywhere; the project-scoped
    // one appears only for its own canonical workspace.
    let elsewhere = tempdir().expect("another project");
    let foreign = load(&cache, &store, Some(elsewhere.path()), &records(&store));
    let foreign_names: Vec<&str> = foreign.skills().map(|skill| skill.name.as_str()).collect();
    assert_eq!(foreign_names, ["user-alpha--greeting"]);
}

#[test]
fn package_text_skill_is_shadowed_by_an_authored_skill_of_its_public_name() {
    let home = TempHome::new();
    let store = home.store();
    let workspace = home.workspace("workspace");
    let authored = workspace.join(".horon/skills/authored");
    fs::create_dir_all(&authored).expect("create the authored skill");
    fs::write(
        authored.join("SKILL.md"),
        "---\nname: user-alpha--greeting\ndescription: Authored greeting.\n---\n\nAUTHORED_BODY\n",
    )
    .expect("write the authored skill");

    let colliding = plugin_source(
        "user-alpha",
        "1.0.0",
        &Spec {
            skills: vec![("greeting", "PACKAGE_GREETING_BODY")],
            ..Spec::default()
        },
    );
    let other = plugin_source(
        "proj-beta",
        "1.0.0",
        &Spec {
            skills: vec![("helper", "Help in the beta package.")],
            ..Spec::default()
        },
    );
    install(&store, colliding.path(), PackageScope::Global);
    install(&store, other.path(), PackageScope::Global);
    let resources = load(&PackageTextCache::new(), &store, Some(&workspace), &records(&store));

    let base = SkillRegistry::load_project_overlay(Some(&workspace)).expect("read the project overlay");
    assert!(
        base.contains("user-alpha--greeting"),
        "the authored skill owns the name first"
    );
    let composed = SkillRegistry::with_package_skills(&base, &resources);

    let winner = composed
        .get("user-alpha--greeting")
        .expect("the authored skill survives the package");
    assert_eq!(winner.description, "Authored greeting.");
    assert!(
        winner.path.ends_with("authored/SKILL.md"),
        "the authored skill keeps its own file: {}",
        winner.path.display()
    );
    assert_eq!(
        winner.body().expect("read the authored body").as_ref(),
        "AUTHORED_BODY",
        "the package entry never replaces the authored body"
    );

    let package_skill = composed
        .get("proj-beta--helper")
        .expect("a package skill without a collision is listed");
    assert_eq!(
        package_skill.body().expect("read the installed body").as_ref(),
        "Help in the beta package."
    );
    assert!(
        composed
            .list()
            .iter()
            .filter(|skill| skill.name == "proj-beta--helper")
            .count()
            == 1
    );
}

// ---------------------------------------------------------------------------
// Rules
// ---------------------------------------------------------------------------

#[test]
fn package_text_rules_are_labelled_and_leave_agents_md_unchanged() {
    let home = TempHome::new();
    let store = home.store();
    let workspace = home.workspace("workspace");
    let agents = "AGENTS_MARKER_INSTRUCTIONS\n";
    fs::write(workspace.join("AGENTS.md"), agents).expect("write AGENTS.md");

    let source = plugin_source(
        "rule-alpha",
        "1.0.0",
        &Spec {
            rules: vec![("style.md", "RULE_STYLE_MARKER\n- be short")],
            ..Spec::default()
        },
    );
    install(&store, source.path(), PackageScope::Global);
    let resources = load(&PackageTextCache::new(), &store, None, &records(&store));

    let (plain, plain_info) = build_system_prompt_split(None, &[], None, Some(&workspace));
    assert!(plain.static_part.contains("AGENTS_MARKER_INSTRUCTIONS"));
    assert!(!plain.static_part.contains("RULE_STYLE_MARKER"));

    let mut with_rules = plain.clone();
    append_package_rules(&mut with_rules, &resources);

    assert!(
        with_rules.static_part.starts_with(&plain.static_part),
        "the package block only appends; every earlier section keeps its bytes"
    );
    assert!(with_rules.static_part.contains("# Package Rules"));
    assert!(
        with_rules
            .static_part
            .contains("## Package rule-alpha (resources/rules/style.md)")
    );
    assert!(with_rules.static_part.contains("RULE_STYLE_MARKER"));
    assert_eq!(
        with_rules.static_part.matches("AGENTS_MARKER_INSTRUCTIONS").count(),
        1,
        "AGENTS.md is still contributed exactly once"
    );
    assert_eq!(with_rules.dynamic_part, plain.dynamic_part);
    assert_eq!(plain_info.project_agents_md_chars, agents.len());

    // Rules are never the replacement hook: neither the base prompt nor the
    // package rule path is what `load_base_system_prompt` reads.
    let base = load_base_system_prompt(Some(&workspace));
    assert!(!base.contains("RULE_STYLE_MARKER"));
    assert!(!base.contains("AGENTS_MARKER_INSTRUCTIONS"));
}

// ---------------------------------------------------------------------------
// Escapes, citations and tampering
// ---------------------------------------------------------------------------

#[test]
fn package_text_rejects_escaping_paths_citations_and_tampered_copies() {
    let home = TempHome::new();
    let store = home.store();
    fs::write(home.path().join("outside-secret.txt"), "CITATION_OUTSIDE_SECRET").expect("write the outside file");

    // A declared path that leaves the package, or names an absolute path, fails
    // install: the file outside the copy is never read.
    for raw in ["../outside-secret.txt", "/etc/passwd"] {
        let escaped = plugin_source(
            "escape-alpha",
            "1.0.0",
            &Spec {
                raw_rules: vec![raw],
                ..Spec::default()
            },
        );
        assert!(
            store
                .install(InstallRequest::new(escaped.path(), PackageScope::Global))
                .is_err(),
            "{raw} must not install as a package resource"
        );
    }

    // A citation inside package text stays literal: installing the package does
    // not expand `@path`, so it cannot pull an outside file into the prompt.
    let citation = "See @../outside-secret.txt and @/etc/passwd for more.";
    let source = plugin_source(
        "cite-alpha",
        "1.0.0",
        &Spec {
            skills: vec![("cite", citation)],
            rules: vec![("cite.md", citation)],
            prompts: vec![("cite.md", citation)],
            ..Spec::default()
        },
    );
    let record = install(&store, source.path(), PackageScope::Global);
    let cache = PackageTextCache::new();
    let resources = cache.load(&store, None, std::iter::once(&record));

    let mut split = SplitSystemPrompt::default();
    append_package_rules(&mut split, &resources);
    assert!(split.static_part.contains(citation), "the citation is kept as text");
    assert!(
        !split.static_part.contains("CITATION_OUTSIDE_SECRET"),
        "no file outside the package reaches the prompt"
    );
    assert_eq!(resources.prompt("cite-alpha--cite"), Some(citation));

    let composed = SkillRegistry::with_package_skills(&SkillRegistry::default(), &resources);
    assert_eq!(
        composed
            .get("cite-alpha--cite")
            .expect("the package skill is listed")
            .body()
            .expect("read the installed body")
            .as_ref(),
        citation
    );

    // Editing the installed copy after install leaves the recorded hash wrong,
    // so the next load rejects that package's resources.
    let digest = record.digest.clone().expect("the record names its copy");
    let rule = store.store_dir().join(digest).join("resources/rules/cite.md");
    let permissions = fs::metadata(&rule).expect("inspect the published rule").permissions();
    make_owner_writable(&rule, permissions).expect("thaw the published rule");
    fs::write(&rule, "TAMPERED_RULE_TEXT").expect("tamper with the published rule");

    let after = cache.load(&store, None, std::iter::once(&record));
    assert!(
        after.rejection("cite-alpha").is_some(),
        "a copy whose bytes moved is rejected: {:?}",
        after.rejection("cite-alpha")
    );
    assert!(after.package("cite-alpha").is_none());
    assert!(after.rules().next().is_none());
    assert!(after.prompt("cite-alpha--cite").is_none());
}

#[test]
fn package_text_prompts_are_only_returned_to_a_command_that_names_them() {
    let home = TempHome::new();
    let store = home.store();
    let workspace = home.workspace("workspace");
    fs::write(workspace.join("AGENTS.md"), "AGENTS_MARKER_PROMPT_CASE\n").expect("write AGENTS.md");

    let source = plugin_source(
        "prompt-alpha",
        "1.0.0",
        &Spec {
            rules: vec![("system-prompt.md", "RULE_BASE_LOOKALIKE_MARKER")],
            prompts: vec![
                ("review.md", "PROMPT_REVIEW_MARKER"),
                ("system-prompt.md", "PROMPT_BASE_LOOKALIKE_MARKER"),
            ],
            ..Spec::default()
        },
    );
    install(&store, source.path(), PackageScope::Global);
    let resources = load(&PackageTextCache::new(), &store, Some(&workspace), &records(&store));

    let (mut split, _info) = build_system_prompt_split(None, &[], None, Some(&workspace));
    append_package_rules(&mut split, &resources);
    assert!(split.static_part.contains("RULE_BASE_LOOKALIKE_MARKER"));
    for marker in ["PROMPT_REVIEW_MARKER", "PROMPT_BASE_LOOKALIKE_MARKER"] {
        assert!(
            !split.static_part.contains(marker),
            "{marker} is not injected automatically"
        );
        assert!(
            !split.dynamic_part.contains(marker),
            "{marker} is not injected automatically"
        );
    }
    assert!(
        !load_base_system_prompt(Some(&workspace)).contains("PROMPT_BASE_LOOKALIKE_MARKER"),
        "a prompt named like the base prompt never becomes one"
    );

    assert_eq!(resources.prompt("prompt-alpha--review"), Some("PROMPT_REVIEW_MARKER"));
    assert_eq!(
        resources.prompt("prompt-alpha--system-prompt"),
        Some("PROMPT_BASE_LOOKALIKE_MARKER")
    );
    assert_eq!(resources.prompt("prompt-alpha--missing"), None);
}

// ---------------------------------------------------------------------------
// Removal and replacement
// ---------------------------------------------------------------------------

#[test]
fn package_text_removal_and_replacement_drop_only_the_owning_package() {
    let home = TempHome::new();
    let store = home.store();
    let cache = PackageTextCache::new();

    let alpha = plugin_source(
        "alpha-one",
        "1.0.0",
        &Spec {
            skills: vec![("greeting", "Alpha greeting.")],
            rules: vec![("style.md", "RULE_ALPHA_V1")],
            prompts: vec![("review.md", "PROMPT_ALPHA_V1")],
            ..Spec::default()
        },
    );
    let beta = plugin_source(
        "beta-one",
        "1.0.0",
        &Spec {
            skills: vec![("helper", "Beta helper.")],
            rules: vec![("style.md", "RULE_BETA")],
            ..Spec::default()
        },
    );
    install(&store, alpha.path(), PackageScope::Global);
    install(&store, beta.path(), PackageScope::Global);

    let first = load(&cache, &store, None, &records(&store));
    let beta_before = first.package("beta-one").expect("beta is loaded");
    assert_eq!(first.prompt("alpha-one--review"), Some("PROMPT_ALPHA_V1"));

    store.remove("alpha-one").expect("remove alpha");
    let second = load(&cache, &store, None, &records(&store));
    assert!(
        second.package("alpha-one").is_none(),
        "a removed package contributes nothing"
    );
    assert!(second.package("beta-one").is_some());
    assert_eq!(second.prompt("alpha-one--review"), None);
    assert!(second.skills().all(|skill| skill.package_id != "alpha-one"));
    assert!(
        Arc::ptr_eq(&beta_before, &second.package("beta-one").expect("beta is still loaded")),
        "an untouched package keeps the cached text it already had"
    );

    let alpha_v2 = plugin_source(
        "alpha-one",
        "2.0.0",
        &Spec {
            skills: vec![("greeting-v2", "Alpha greeting, second version.")],
            rules: vec![("style.md", "RULE_ALPHA_V2")],
            ..Spec::default()
        },
    );
    install_replacing(&store, alpha_v2.path(), PackageScope::Global);
    let third = load(&cache, &store, None, &records(&store));
    let alpha_after = third.package("alpha-one").expect("the replacement is loaded");
    assert_eq!(alpha_after.rules[0].text, "RULE_ALPHA_V2");
    assert!(
        alpha_after
            .skills
            .iter()
            .any(|skill| skill.name == "alpha-one--greeting-v2")
    );
    assert_eq!(
        third.prompt("alpha-one--review"),
        None,
        "the replaced prompt set is gone"
    );
    assert!(
        Arc::ptr_eq(&beta_before, &third.package("beta-one").expect("beta is still loaded")),
        "a replacement rebuilds only the replaced package"
    );
}
