//! Tests for frontmatter-only discovery and lazy bodies.

use super::{SkillRegistry, body, frontmatter};
use std::path::Path;

fn write_skill(dir: &Path, name: &str, body: &str) {
    let skill_dir = dir.join(name);
    std::fs::create_dir_all(&skill_dir).expect("create skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: Skill {name}\n---\n\n{body}\n"),
    )
    .expect("write skill");
}

#[test]
fn catalog_reads_frontmatter_only() {
    let temp = tempfile::tempdir().expect("tempdir");
    let skills_dir = temp.path().join("skills");
    let body = "b".repeat(8 * 1024);
    for index in 0..200 {
        write_skill(&skills_dir, &format!("skill-{index:03}"), &body);
    }

    // The process-wide counters are shared with the other tests running in
    // parallel, so this asserts on the measurement the call itself returns.
    let mut registry = SkillRegistry::default();
    let stats = registry.load_from_dir_with_stats(&skills_dir).expect("load skills");

    assert_eq!(registry.list().len(), 200);
    assert_eq!(stats.files, 200, "each SKILL.md is opened once during discovery");
    assert!(
        stats.bytes <= 200 * 4096,
        "discovery must read frontmatter-sized chunks, read {} bytes for 200 skills",
        stats.bytes
    );

    let skill = registry.get("skill-042").expect("skill");

    // Discovery left the body on disk, so the first load reads it and every
    // later load is served from the cache. Both facts come from the call's own
    // result rather than the shared process-wide counter.
    let (len, first_read) = body::with_cached(&skill.path, &skill.body, |cached, read| (cached.body().len(), read))
        .expect("first body read");
    assert_eq!(len, body.len());
    assert!(first_read, "the first body() call reads the file");

    let (_, second_read) = body::with_cached(&skill.path, &skill.body, |cached, read| (cached.body().len(), read))
        .expect("cached body read");
    assert!(!second_read, "a warm body() call must not open the file again");
}

/// The structural gate (todo 7) reads the process-wide counter, so discovery
/// must keep recording there even though this module measures locally. A delta
/// is monotone, so a parallel test can only add to it.
#[test]
fn discovery_records_the_global_counter() {
    // The only test in this module that reads the process-wide counter, so it
    // holds the lock the later request-boundary tests will also take.
    let _guard = super::test_support::counter_lock();
    let temp = tempfile::tempdir().expect("tempdir");
    let skills_dir = temp.path().join("skills");
    let body = "c".repeat(8 * 1024);
    write_skill(&skills_dir, "counted", &body);

    let before = horon_agent_perf_counters::snapshot();
    let mut registry = SkillRegistry::default();
    registry.load_from_dir(&skills_dir).expect("load skills");
    let after = horon_agent_perf_counters::snapshot();

    assert!(
        after.skill_file_reads - before.skill_file_reads >= 1,
        "discovery must record the file read for the structural gate"
    );
    assert!(
        after.skill_bytes_read - before.skill_bytes_read >= 4096,
        "the structural gate reads the byte counter, so discovery must record the bytes it read"
    );
}

#[cfg(unix)]
#[test]
fn edited_body_is_reread() {
    let temp = tempfile::tempdir().expect("tempdir");
    let skills_dir = temp.path().join("skills");
    write_skill(&skills_dir, "live", "first-body");
    let path = skills_dir.join("live/SKILL.md");

    let mut registry = SkillRegistry::default();
    registry.load_from_dir(&skills_dir).expect("load skills");
    let skill = registry.get("live").expect("skill").clone();
    assert_eq!(skill.body().expect("body").as_ref(), "first-body");

    let restored = std::fs::metadata(&path).expect("metadata").modified().expect("mtime");
    std::fs::write(&path, "---\nname: live\ndescription: Skill live\n---\n\nother-body\n").expect("rewrite skill");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .and_then(|file| file.set_modified(restored))
        .expect("restore mtime");

    assert_eq!(
        skill.body().expect("reread body").as_ref(),
        "other-body",
        "a same-size edit with a restored mtime must still be re-read"
    );
}

#[test]
fn oversized_frontmatter_falls_back() {
    let temp = tempfile::tempdir().expect("tempdir");
    let skills_dir = temp.path().join("skills");
    let skill_dir = skills_dir.join("huge-head");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    let description = "d".repeat(20 * 1024);
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: huge-head\ndescription: {description}\n---\n\nbody\n"),
    )
    .expect("write skill");

    let mut registry = SkillRegistry::default();
    let stats = registry.load_from_dir_with_stats(&skills_dir).expect("load skills");
    let skill = registry.get("huge-head").expect("skill beyond the frontmatter cap");
    assert_eq!(skill.description.len(), description.len());
    assert_eq!(skill.body().expect("body").as_ref(), "body");
    assert!(
        stats.bytes > frontmatter::FRONTMATTER_MAX as u64,
        "a frontmatter past the cap falls back to a full read"
    );
}

#[test]
fn unclosed_frontmatter_errors() {
    let temp = tempfile::tempdir().expect("tempdir");
    let skills_dir = temp.path().join("skills");
    let skill_dir = skills_dir.join("unclosed");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    let path = skill_dir.join("SKILL.md");
    std::fs::write(&path, "---\nname: unclosed\ndescription: never closed\n\nbody\n").expect("write skill");

    let error = frontmatter::read_frontmatter(&path).expect_err("unclosed frontmatter must fail");
    assert!(
        error.to_string().contains("Unclosed frontmatter"),
        "expected the pre-existing error, got: {error}"
    );

    let mut registry = SkillRegistry::default();
    registry.load_from_dir(&skills_dir).expect("load skills");
    assert!(!registry.contains("unclosed"));
}

#[test]
fn invalid_yaml_still_counts_the_bytes_read() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("SKILL.md");
    let content = "---\nname: broken\ndescription: Triggers: invalid yaml\n---\n\nbody\n";
    std::fs::write(&path, content).expect("write skill");

    let (result, stats) = frontmatter::read_frontmatter_measured(&path);
    assert!(result.is_err(), "invalid YAML must fail");
    assert_eq!(stats.files, 1, "the failing read is still one opened file");
    assert_eq!(
        stats.bytes,
        content.len() as u64,
        "the bytes read before the YAML error must be reported"
    );
}

#[test]
fn nbsp_led_frontmatter_stays_in_the_first_chunk() {
    let temp = tempfile::tempdir().expect("tempdir");
    let skills_dir = temp.path().join("skills");
    let skill_dir = skills_dir.join("nbsp-led");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    let body = "b".repeat(32 * 1024);
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("\u{a0}---\nname: nbsp-led\ndescription: Non-breaking space lead\n---\n\n{body}\n"),
    )
    .expect("write skill");

    let mut registry = SkillRegistry::default();
    let stats = registry.load_from_dir_with_stats(&skills_dir).expect("load skills");
    let skill = registry.get("nbsp-led").expect("skill after a non-ASCII space");
    assert_eq!(skill.description, "Non-breaking space lead");
    assert_eq!(skill.body().expect("body").len(), body.len());
    assert!(
        stats.bytes <= 4096,
        "a non-ASCII leading space must not force a full read, read {} bytes",
        stats.bytes
    );

    // The whole-file parse must agree with the chunked one.
    let content = std::fs::read_to_string(skill_dir.join("SKILL.md")).expect("read skill");
    let whole = frontmatter::parse_text(&content).expect("whole-file parse");
    assert_eq!(whole.frontmatter.description, skill.description);
    assert_eq!(whole.body, body);
}

#[test]
fn invalid_utf8_body_listed_and_errors_lazily() {
    let temp = tempfile::tempdir().expect("tempdir");
    let skills_dir = temp.path().join("skills");
    let skill_dir = skills_dir.join("bad-utf8");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    let mut bytes = b"---\nname: bad-utf8\ndescription: Bad UTF-8 body\n---\n\n".to_vec();
    bytes.extend_from_slice(&[0xff, 0xfe, 0xfd]);
    bytes.extend_from_slice(b"\n");
    std::fs::write(skill_dir.join("SKILL.md"), bytes).expect("write skill");

    let mut registry = SkillRegistry::default();
    registry.load_from_dir(&skills_dir).expect("load skills");
    let skill = registry
        .get("bad-utf8")
        .expect("an unreadable body must not hide the skill");

    let error = skill.body().expect_err("the body decodes lazily and fails");
    assert!(
        error.to_string().contains("UTF-8") || error.to_string().contains("utf-8"),
        "expected a clean UTF-8 error, got: {error}"
    );
    assert!(skill.get_prompt().is_err());
}

#[test]
fn deleted_file_errors() {
    let temp = tempfile::tempdir().expect("tempdir");
    let skills_dir = temp.path().join("skills");
    write_skill(&skills_dir, "gone", "body");
    let path = skills_dir.join("gone/SKILL.md");

    let mut registry = SkillRegistry::default();
    registry.load_from_dir(&skills_dir).expect("load skills");
    let skill = registry.get("gone").expect("skill").clone();
    assert_eq!(skill.body().expect("body").as_ref(), "body");

    std::fs::remove_file(&path).expect("delete skill file");
    assert!(
        skill.body().is_err(),
        "a deleted file must report an error, not stale text"
    );
    assert!(skill.get_prompt().is_err());
    assert!(skill.as_memory_entry().is_err());
}
