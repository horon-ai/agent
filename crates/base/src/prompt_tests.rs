use super::*;

/// Verify the default system prompt does NOT identify as "Claude Code"
/// It's fine to say "powered by Claude" but not "Claude Code" (Anthropic's product)
#[test]
fn test_default_system_prompt_no_claude_code_identity() {
    let prompt = DEFAULT_SYSTEM_PROMPT.to_lowercase();

    assert!(
        !prompt.contains("claude code"),
        "DEFAULT_SYSTEM_PROMPT should NOT identify as 'Claude Code'. Found in system_prompt.md"
    );
    assert!(
        !prompt.contains("claude-code"),
        "DEFAULT_SYSTEM_PROMPT should NOT contain 'claude-code'. Found in system_prompt.md"
    );
}

/// Verify skill prompts don't accidentally introduce "Claude Code" identity
#[test]
fn test_skill_prompt_integration() {
    // Test that a skill prompt is properly appended and doesn't break anything
    let skill_prompt = "You are helping with a debugging task.";
    let prompt = build_system_prompt(Some(skill_prompt), &[]);

    // The prompt should contain our default system prompt
    // Identity line reworded in 24838926e ("docs: refine base agent guidance").
    assert!(prompt.contains("You are HoronAgent"));

    // The prompt should contain the skill prompt
    assert!(prompt.contains(skill_prompt));

    // The base prompt parts (excluding user-provided instruction files) should NOT contain
    // "Claude Code". We check DEFAULT_SYSTEM_PROMPT separately since user files may
    // legitimately contain it.
    let default_lower = DEFAULT_SYSTEM_PROMPT.to_lowercase();
    assert!(
        !default_lower.contains("claude code"),
        "DEFAULT_SYSTEM_PROMPT should NOT identify as 'Claude Code'"
    );
}

#[test]
fn skill_description_collapses_whitespace_without_clipping_short_text() {
    assert_eq!(
        clip_skill_description("  Build\n\tand   test the project.  "),
        "Build and test the project."
    );
}

#[test]
fn skill_description_clips_to_the_character_limit_with_ellipsis() {
    let clipped = clip_skill_description(&"a".repeat(SKILL_DESC_MAX_CHARS + 20));

    assert_eq!(clipped.chars().count(), SKILL_DESC_MAX_CHARS);
    assert!(clipped.ends_with('…'));
}

#[test]
fn skill_description_clipping_is_utf8_safe() {
    let clipped = clip_skill_description(&"ж".repeat(SKILL_DESC_MAX_CHARS + 20));

    assert_eq!(clipped.chars().count(), SKILL_DESC_MAX_CHARS);
    assert_eq!(
        clipped.chars().filter(|character| *character == 'ж').count(),
        SKILL_DESC_MAX_CHARS - 1
    );
    assert!(clipped.ends_with('…'));
}

#[test]
fn full_and_split_prompt_builders_use_the_same_one_line_skill_descriptions() {
    let skills = vec![SkillInfo {
        name: "example".to_string(),
        description: format!("First line\n\t{}", "д".repeat(SKILL_DESC_MAX_CHARS + 20)),
    }];
    let expected = build_available_skills_section(&skills).expect("skills section");

    let (full, full_info) = build_system_prompt_full(None, &skills, None, None);
    let (split, split_info) = build_system_prompt_split(None, &skills, None, None);

    assert!(full.contains(&expected));
    assert!(split.static_part.contains(&expected));
    assert_eq!(full_info.skills_chars, expected.len());
    assert_eq!(split_info.skills_chars, expected.len());
    assert!(expected.contains("First line "));
    assert!(!expected.contains("First line\n"));
    let entry = expected
        .lines()
        .find(|line| line.starts_with("- `/example `"))
        .expect("example skill entry");
    assert!(entry.ends_with('…'));
}

#[test]
fn skill_snapshot_budget_zero_skills_renders_no_section() {
    assert!(build_available_skills_section(&[]).is_none());
    assert_eq!(omitted_skill_count(&[]), 0);
}

#[test]
fn skill_snapshot_budget_small_list_is_byte_identical_to_the_previous_format() {
    let infos = vec![
        SkillInfo {
            name: "beta".to_string(),
            description: "Second skill".to_string(),
        },
        SkillInfo {
            name: "alpha".to_string(),
            description: "First\n\tskill".to_string(),
        },
    ];
    let expected = concat!(
        "# Available Skills\n\n",
        "You can read a skill with the read tool at `skill://<name>`, or the user can invoke it with \
         `/skill:<name>` or `/<name>`:\n",
        "\n- `/alpha ` - First skill",
        "\n- `/beta ` - Second skill",
        "\n\nWhen a user asks about available skills or capabilities, mention these skills."
    );

    let section = build_available_skills_section(&infos).expect("skills section");
    assert_eq!(section, expected);
    assert_eq!(omitted_skill_count(&infos), 0);
}

#[test]
fn skill_snapshot_budget_two_hundred_skills_fit_with_descriptions() {
    let infos: Vec<SkillInfo> = (0..200)
        .map(|index| SkillInfo {
            name: format!("skill-{index:04}"),
            description: "Do a thing with the project files and report back.".to_string(),
        })
        .collect();

    let section = build_available_skills_section(&infos).expect("skills section");
    assert!(section.len() <= SKILLS_SECTION_MAX_BYTES);
    assert_eq!(omitted_skill_count(&infos), 0);
    assert!(!section.contains("Descriptions are omitted"));
    assert_eq!(section.lines().filter(|line| line.starts_with("- `/")).count(), 200);
}

#[test]
fn skill_snapshot_budget_keeps_name_order_and_120_char_descriptions() {
    let long_description = "\u{0434}".repeat(400);
    let infos: Vec<SkillInfo> = (0..100)
        .rev()
        .map(|index| SkillInfo {
            name: format!("skill-{index:04}"),
            description: long_description.clone(),
        })
        .collect();

    let section = build_available_skills_section(&infos).expect("skills section");
    assert!(!section.contains("Descriptions are omitted"));
    let listed: Vec<&str> = section
        .lines()
        .filter(|line| line.starts_with("- `/") && !line.contains("skill_manage"))
        .map(|line| line.trim_start_matches("- `/").split(' ').next().unwrap_or_default())
        .collect();
    let mut sorted = listed.clone();
    sorted.sort_unstable();
    assert_eq!(listed, sorted);

    let description = section
        .lines()
        .find(|line| line.starts_with("- `/") && !line.contains("skill_manage"))
        .and_then(|line| line.split(" - ").nth(1))
        .expect("entry description");
    assert_eq!(description.chars().count(), SKILL_DESC_MAX_CHARS);
}

/// Names long enough that 1,000 of them exceed the budget even with every
/// description dropped, so the omission path is exercised at the required
/// 1,000-skill size.
fn padded_skill_infos(count: usize, prefix: &str) -> Vec<SkillInfo> {
    (0..count)
        .map(|index| SkillInfo {
            name: format!("{prefix}-{index:04}-padded-skill-name-for-budget-tests"),
            description: "Do a thing with the project files and report back.".to_string(),
        })
        .collect()
}

#[test]
fn skill_snapshot_budget_one_thousand_skills_drop_descriptions_then_truncate_names() {
    let infos = padded_skill_infos(1_000, "skill");

    let section = build_available_skills_section(&infos).expect("skills section");
    assert!(
        section.len() <= SKILLS_SECTION_MAX_BYTES,
        "section is {} bytes, over budget",
        section.len()
    );
    assert!(section.contains("Descriptions are omitted"));
    assert!(section.contains("skill://<name>"));

    let omitted = omitted_skill_count(&infos);
    assert!(omitted > 0, "1,000 padded names must exceed the name-only budget");
    assert!(
        section.contains(&format!("{omitted} more skills are installed")),
        "section must state the omitted count {omitted}"
    );
}

#[test]
fn skill_snapshot_budget_thousand_short_names_keep_every_name_without_descriptions() {
    let infos: Vec<SkillInfo> = (0..1_000)
        .map(|index| SkillInfo {
            name: format!("skill-{index:04}"),
            description: "Do a thing with the project files and report back.".to_string(),
        })
        .collect();

    let section = build_available_skills_section(&infos).expect("skills section");
    assert!(section.len() <= SKILLS_SECTION_MAX_BYTES);
    assert!(section.contains("Descriptions are omitted"));
    assert_eq!(omitted_skill_count(&infos), 0, "short names still all fit");
    assert!(!section.contains("more skills are installed"));
    assert_eq!(section.lines().filter(|line| line.starts_with("- `/")).count(), 1_000);
}

#[test]
fn skill_snapshot_budget_korean_names_stay_valid_utf8_under_budget() {
    let infos: Vec<SkillInfo> = (0..4_000)
        .map(|index| SkillInfo {
            name: format!("\u{d55c}\u{ae00}-\u{c2a4}\u{d0ac}-{index:04}"),
            description: "\u{d55c}\u{ad6d}\u{c5b4} \u{c124}\u{ba85}\u{c785}\u{b2c8}\u{b2e4}.".to_string(),
        })
        .collect();

    let section = build_available_skills_section(&infos).expect("skills section");
    assert!(section.len() <= SKILLS_SECTION_MAX_BYTES);
    assert!(std::str::from_utf8(section.as_bytes()).is_ok());
    for line in section
        .lines()
        .filter(|line| line.starts_with("- `/") && !line.contains("skill_manage"))
    {
        assert!(line.ends_with('`'), "partial skill entry: {line}");
    }
    let omitted = omitted_skill_count(&infos);
    assert!(omitted > 0);
    assert!(section.contains(&format!("{omitted} more skills are installed")));
}

#[test]
fn skill_snapshot_budget_long_names_truncate_on_a_character_boundary() {
    let infos: Vec<SkillInfo> = (0..200)
        .map(|index| SkillInfo {
            name: format!("{}-{index:02}", "\u{ae34}\u{c774}\u{b984}".repeat(60)),
            description: String::new(),
        })
        .collect();

    let section = build_available_skills_section(&infos).expect("skills section");
    assert!(section.len() <= SKILLS_SECTION_MAX_BYTES);
    assert!(std::str::from_utf8(section.as_bytes()).is_ok());
    let omitted = omitted_skill_count(&infos);
    assert!(omitted > 0);
    assert!(section.contains(&format!("{omitted} more skills are installed")));
}

#[test]
fn skill_snapshot_budget_multi_byte_names_never_split_a_character() {
    let infos: Vec<SkillInfo> = (0..5_000)
        .map(|index| SkillInfo {
            name: format!("\u{d55c}{index:05}"),
            description: String::new(),
        })
        .collect();

    let section = build_available_skills_section(&infos).expect("skills section");
    assert!(section.len() <= SKILLS_SECTION_MAX_BYTES);
    assert!(
        std::str::from_utf8(section.as_bytes()).is_ok(),
        "clipping must not split a UTF-8 character"
    );
    assert!(omitted_skill_count(&infos) > 0);
}

#[test]
fn skill_snapshot_budget_bounded_section_reaches_both_prompt_builders() {
    let infos = padded_skill_infos(1_000, "skill");
    let section = build_available_skills_section(&infos).expect("skills section");

    let (full, full_info) = build_system_prompt_full(None, &infos, None, None);
    assert!(full.contains(&section));
    assert_eq!(full_info.skills_chars, section.len());

    let (split, split_info) = build_system_prompt_split(None, &infos, None, None);
    assert!(split.static_part.contains(&section));
    assert_eq!(split_info.skills_chars, section.len());
}

#[test]
fn test_load_agents_md_files_uses_sandboxed_global_files() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("HORON_HOME");
    let temp = tempfile::TempDir::new().unwrap();
    crate::env::set_var("HORON_HOME", temp.path());
    std::fs::create_dir_all(temp.path().join("external")).unwrap();

    std::fs::write(
        temp.path().join("external/AGENTS.md"),
        "sandboxed global agents instructions",
    )
    .unwrap();

    let project_dir = tempfile::TempDir::new().unwrap();
    let (content, info) = load_agents_md_files_from_dir(Some(project_dir.path()));

    assert!(info.has_global_agents_md);
    let content = content.expect("global instructions content");
    assert!(content.contains("# Global Instructions (~/AGENTS.md)"));
    assert!(!content.contains("~/.AGENTS.md"));
    assert!(content.contains("sandboxed global agents instructions"));

    let sandboxed_home = temp.path().join("external");
    let (content, info) = load_agents_md_files_from_dir(Some(&sandboxed_home));
    let content = content.expect("deduplicated home instructions");
    assert!(info.has_project_agents_md);
    assert!(!info.has_global_agents_md);
    assert!(content.contains("# Project Instructions (AGENTS.md)"));
    assert!(!content.contains("# Global Instructions (~/AGENTS.md)"));
    assert_eq!(content.matches("sandboxed global agents instructions").count(), 1);

    if let Some(prev_home) = prev_home {
        crate::env::set_var("HORON_HOME", prev_home);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[test]
fn agents_md_same_canonical_file_is_loaded_only_as_project_instructions() {
    let project_dir = tempfile::TempDir::new().unwrap();
    let agents_md = project_dir.path().join("AGENTS.md");
    std::fs::write(&agents_md, "shared instructions").unwrap();

    let (content, info) = load_agents_md_files_from_dirs(project_dir.path(), Some(&agents_md));
    let content = content.expect("project instructions");

    assert!(info.has_project_agents_md);
    assert!(!info.has_global_agents_md);
    assert!(content.contains("# Project Instructions (AGENTS.md)"));
    assert!(!content.contains("# Global Instructions (~/AGENTS.md)"));
    assert_eq!(content.matches("shared instructions").count(), 1);
}

#[test]
fn agents_md_distinct_project_and_global_files_are_both_loaded() {
    let project_dir = tempfile::TempDir::new().unwrap();
    let global_dir = tempfile::TempDir::new().unwrap();
    let global_agents_md = global_dir.path().join("AGENTS.md");
    std::fs::write(project_dir.path().join("AGENTS.md"), "project instructions").unwrap();
    std::fs::write(&global_agents_md, "global instructions").unwrap();

    let (content, info) = load_agents_md_files_from_dirs(project_dir.path(), Some(&global_agents_md));
    let content = content.expect("project and global instructions");

    assert!(info.has_project_agents_md);
    assert!(info.has_global_agents_md);
    assert!(content.contains("project instructions"));
    assert!(content.contains("global instructions"));
}

#[test]
fn captured_agents_md_keeps_split_prompt_stable_after_file_write() {
    // The prompt reads `HORON_HOME` and `HOME`; tests that repoint them
    // hold this lock, so reading without it sees another test's home.
    let _guard = crate::storage::lock_test_env();
    let project_dir = tempfile::TempDir::new().unwrap();
    let agents_md = project_dir.path().join("AGENTS.md");
    std::fs::write(&agents_md, "original session instructions").unwrap();
    let snapshot = load_agents_md_files_from_dirs(project_dir.path(), None);

    let (before, _) =
        build_system_prompt_split_with_agents_md(None, &[], None, Some(project_dir.path()), snapshot.clone());
    std::fs::write(&agents_md, "instructions written during the session").unwrap();
    let (after, _) = build_system_prompt_split_with_agents_md(None, &[], None, Some(project_dir.path()), snapshot);

    assert_eq!(before.static_part, after.static_part);
    assert!(after.static_part.contains("original session instructions"));
    assert!(!after.static_part.contains("instructions written during the session"));

    // A new session/workspace boundary captures a fresh snapshot rather than
    // pinning the old instructions forever.
    let fresh_snapshot = load_agents_md_files_from_dirs(project_dir.path(), None);
    let (next_session, _) =
        build_system_prompt_split_with_agents_md(None, &[], None, Some(project_dir.path()), fresh_snapshot);
    assert!(
        next_session
            .static_part
            .contains("instructions written during the session")
    );
    assert_ne!(before.static_part, next_session.static_part);
}

#[test]
fn agents_md_missing_global_file_keeps_project_instructions() {
    let project_dir = tempfile::TempDir::new().unwrap();
    let global_dir = tempfile::TempDir::new().unwrap();
    let missing_global_agents_md = global_dir.path().join("missing-AGENTS.md");
    std::fs::write(project_dir.path().join("AGENTS.md"), "project only").unwrap();

    let (content, info) = load_agents_md_files_from_dirs(project_dir.path(), Some(&missing_global_agents_md));
    let content = content.expect("project instructions");

    assert!(info.has_project_agents_md);
    assert!(!info.has_global_agents_md);
    assert!(content.contains("project only"));
    assert!(!content.contains("# Global Instructions (~/AGENTS.md)"));
}

#[cfg(unix)]
#[test]
fn agents_md_symlink_alias_is_deduplicated_by_canonical_file_path() {
    use std::os::unix::fs::symlink;

    let project_dir = tempfile::TempDir::new().unwrap();
    let global_dir = tempfile::TempDir::new().unwrap();
    let global_agents_md = global_dir.path().join("AGENTS.md");
    std::fs::write(&global_agents_md, "symlinked instructions").unwrap();
    symlink(&global_agents_md, project_dir.path().join("AGENTS.md")).unwrap();

    let (content, info) = load_agents_md_files_from_dirs(project_dir.path(), Some(&global_agents_md));
    let content = content.expect("project instructions through symlink");

    assert!(info.has_project_agents_md);
    assert!(!info.has_global_agents_md);
    assert_eq!(content.matches("symlinked instructions").count(), 1);
}

#[test]
fn test_session_context_includes_time_timezone_and_system_info() {
    let context = build_session_context(None, &[]);
    assert!(context.contains("# Session Context"));
    assert!(context.contains("Time: "));
    assert!(context.contains("Timezone: "));
    assert!(context.contains("OS: "));
    assert!(context.contains("Architecture: "));
    assert!(context.contains("HoronAgent version: "));
    assert!(!context.contains("Working directory: "));
    assert!(!context.contains("Git:"));
}

#[test]
fn session_datetime_uses_the_supplied_local_date_time_and_offset() {
    use chrono::TimeZone;

    let local = chrono::FixedOffset::east_opt(5 * 60 * 60 + 30 * 60)
        .unwrap()
        .with_ymd_and_hms(2026, 8, 23, 0, 10, 9)
        .unwrap();

    assert_eq!(
        format_session_datetime(local),
        ["Date: 2026-08-23", "Time: 00:10:09", "Timezone: +05:30",]
    );
}

#[test]
fn session_datetime_formats_utc_fallback_deterministically() {
    use chrono::TimeZone;

    let utc = chrono::Utc.with_ymd_and_hms(2026, 8, 22, 18, 40, 6).unwrap();

    assert_eq!(
        format_session_datetime(utc),
        ["Date: 2026-08-22", "Time: 18:40:06", "Timezone: UTC"]
    );
}

#[test]
fn test_split_prompt_does_not_inject_session_context_per_turn() {
    let (split, _info) = build_system_prompt_split(None, &[], None, None);
    assert!(!split.dynamic_part.contains("# Session Context"));
    assert!(!split.dynamic_part.contains("Time: "));
    assert!(!split.dynamic_part.contains("Timezone: UTC"));
}

#[test]
fn sponsored_discovery_is_not_injected_into_the_system_prompt() {
    let (split, _) = build_system_prompt_split(None, &[], None, None);
    assert!(!split.static_part.contains("Discoverable Tools"));
    assert!(!split.static_part.contains("integration_tools"));
}

#[test]
fn test_prompt_overlay_files_are_loaded_from_project_and_global_horon_agent_dirs() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("HORON_HOME");
    let temp = tempfile::TempDir::new().unwrap();
    crate::env::set_var("HORON_HOME", temp.path());
    std::fs::create_dir_all(temp.path()).unwrap();
    std::fs::write(
        temp.path().join("prompt-overlay.md"),
        "global prompt overlay instructions",
    )
    .unwrap();

    let project_dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(project_dir.path().join(".horon")).unwrap();
    std::fs::write(
        project_dir.path().join(".horon/prompt-overlay.md"),
        "project prompt overlay instructions",
    )
    .unwrap();

    let direct = load_prompt_overlay_files_from_dir(Some(project_dir.path()));

    assert!(direct.0.is_some(), "expected prompt overlay content");
    let direct_content = direct.0.unwrap();
    assert!(
        direct_content.contains("project prompt overlay instructions"),
        "expected project prompt overlay content"
    );
    assert!(
        direct_content.contains("global prompt overlay instructions"),
        "expected global prompt overlay content"
    );

    let (prompt, info) = build_system_prompt_full(None, &[], None, Some(project_dir.path()));
    assert!(prompt.contains("project prompt overlay instructions"));
    assert!(prompt.contains("global prompt overlay instructions"));
    assert!(info.prompt_overlay_chars > 0);

    if let Some(prev_home) = prev_home {
        crate::env::set_var("HORON_HOME", prev_home);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[test]
fn test_preferred_tools_files_are_loaded_from_project_and_global_horon_agent_dirs() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("HORON_HOME");
    let temp = tempfile::TempDir::new().unwrap();
    crate::env::set_var("HORON_HOME", temp.path());
    std::fs::create_dir_all(temp.path()).unwrap();
    std::fs::write(
        temp.path().join("preferred-tools.md"),
        "global preferred tools instructions",
    )
    .unwrap();

    let project_dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir_all(project_dir.path().join(".horon")).unwrap();
    std::fs::write(
        project_dir.path().join(".horon/preferred-tools.md"),
        "project preferred tools instructions",
    )
    .unwrap();

    let direct = load_preferred_tools_files_from_dir(Some(project_dir.path()));

    assert!(direct.0.is_some(), "expected preferred tools content");
    let direct_content = direct.0.unwrap();
    assert!(
        direct_content.contains("Project Preferred Tools (.horon/preferred-tools.md)"),
        "expected project preferred tools section heading"
    );
    assert!(
        direct_content.contains("project preferred tools instructions"),
        "expected project preferred tools content"
    );
    assert!(
        direct_content.contains("Global Preferred Tools (~/.horon/preferred-tools.md)"),
        "expected global preferred tools section heading"
    );
    assert!(
        direct_content.contains("global preferred tools instructions"),
        "expected global preferred tools content"
    );

    let (prompt, info) = build_system_prompt_full(None, &[], None, Some(project_dir.path()));
    assert!(prompt.contains("project preferred tools instructions"));
    assert!(prompt.contains("global preferred tools instructions"));
    assert!(info.preferred_tools_chars > 0);

    let (split, split_info) = build_system_prompt_split(None, &[], None, Some(project_dir.path()));
    assert!(split.static_part.contains("project preferred tools instructions"));
    assert!(split.static_part.contains("global preferred tools instructions"));
    assert!(split_info.preferred_tools_chars > 0);

    if let Some(prev_home) = prev_home {
        crate::env::set_var("HORON_HOME", prev_home);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[test]
fn test_swarm_prompt_prefers_project_then_global_then_default() {
    let _guard = crate::storage::lock_test_env();
    let prev_home = std::env::var_os("HORON_HOME");
    let temp = tempfile::TempDir::new().unwrap();
    crate::env::set_var("HORON_HOME", temp.path());
    std::fs::create_dir_all(temp.path()).unwrap();

    let project_dir = tempfile::TempDir::new().unwrap();

    // No override files: built-in default.
    let prompt = load_swarm_prompt(Some(project_dir.path()));
    assert_eq!(prompt, DEFAULT_SWARM_PROMPT.trim());

    // Global override wins over the default.
    std::fs::write(temp.path().join("swarm-prompt.md"), "global swarm routing").unwrap();
    let prompt = load_swarm_prompt(Some(project_dir.path()));
    assert_eq!(prompt, "global swarm routing");

    // Project override wins over global.
    std::fs::create_dir_all(project_dir.path().join(".horon")).unwrap();
    std::fs::write(
        project_dir.path().join(".horon/swarm-prompt.md"),
        "project swarm routing",
    )
    .unwrap();
    let prompt = load_swarm_prompt(Some(project_dir.path()));
    assert_eq!(prompt, "project swarm routing");

    // A blank project file falls through to global instead of going empty.
    std::fs::write(project_dir.path().join(".horon/swarm-prompt.md"), "   \n").unwrap();
    let prompt = load_swarm_prompt(Some(project_dir.path()));
    assert_eq!(prompt, "global swarm routing");

    if let Some(prev_home) = prev_home {
        crate::env::set_var("HORON_HOME", prev_home);
    } else {
        crate::env::remove_var("HORON_HOME");
    }
}

#[test]
fn test_default_swarm_prompt_mentions_model_and_list_models() {
    assert!(DEFAULT_SWARM_PROMPT.contains("list_models"));
    assert!(DEFAULT_SWARM_PROMPT.contains("model"));
    assert!(DEFAULT_SWARM_PROMPT.contains("effort"));
    assert!(DEFAULT_SWARM_PROMPT.contains("only the root session may spawn agents"));
    assert!(DEFAULT_SWARM_PROMPT.contains("swarm-deep"));
}

#[test]
fn split_prompt_estimated_tokens_is_positive_when_populated() {
    let (split, _info) = build_system_prompt_split(None, &[], None, None);
    assert!(split.chars() > 0);
    assert!(split.estimated_tokens() > 0);
}

#[test]
fn swarm_effort_directive_is_appended_only_for_swarm_sentinel() {
    assert!(is_swarm_effort("swarm"));
    assert!(is_swarm_effort("  Swarm "));
    assert!(!is_swarm_effort("xhigh"));

    let mut split = SplitSystemPrompt {
        static_part: "base".to_string(),
        dynamic_part: String::new(),
    };
    append_swarm_effort_directive(&mut split, Some("xhigh"));
    assert!(!split.dynamic_part.contains("Swarm Effort"));

    append_swarm_effort_directive(&mut split, Some("swarm"));
    assert!(split.dynamic_part.contains("# Swarm Effort"));
    assert!(split.dynamic_part.contains("swarm` tool"));

    // None / empty effort should not inject.
    let mut other = SplitSystemPrompt::default();
    append_swarm_effort_directive(&mut other, None);
    assert!(other.dynamic_part.is_empty());
}

#[test]
fn swarm_deep_effort_injects_task_graph_directive() {
    use crate::prompt::is_deep_swarm_effort;

    assert!(is_swarm_effort("swarm-deep"));
    assert!(is_deep_swarm_effort("swarm-deep"));
    assert!(is_deep_swarm_effort("  Swarm-Deep "));
    assert!(!is_deep_swarm_effort("swarm"));
    assert!(!is_deep_swarm_effort("xhigh"));

    // Deep sentinel injects the DAG-first task-graph directive, not the light one.
    let mut split = SplitSystemPrompt::default();
    append_swarm_effort_directive(&mut split, Some("swarm-deep"));
    assert!(split.dynamic_part.contains("# Deep Task Graph"));
    assert!(split.dynamic_part.contains("swarm task_graph"));
    assert!(!split.dynamic_part.contains("# Swarm Effort"));

    // Light sentinel still injects the fan-out directive, not the deep one.
    let mut light = SplitSystemPrompt::default();
    append_swarm_effort_directive(&mut light, Some("swarm"));
    assert!(light.dynamic_part.contains("# Swarm Effort"));
    assert!(!light.dynamic_part.contains("# Deep Task Graph"));
}

#[test]
fn classify_effort_distinguishes_reasoning_from_swarm_modes() {
    use crate::prompt::{EffortKind, classify_effort, is_swarm_mode_effort};

    // Plain reasoning levels are not swarm modes.
    for level in ["none", "minimal", "low", "medium", "high", "xhigh", "max"] {
        assert_eq!(classify_effort(level), EffortKind::Reasoning, "{level}");
        assert!(!is_swarm_mode_effort(level), "{level}");
    }

    assert_eq!(classify_effort("swarm"), EffortKind::SwarmLight);
    assert_eq!(classify_effort("swarm-deep"), EffortKind::SwarmDeep);
    assert!(is_swarm_mode_effort("swarm"));
    assert!(is_swarm_mode_effort("  Swarm-Deep "));
    assert!(EffortKind::SwarmLight.is_swarm_mode());
    assert!(EffortKind::SwarmDeep.is_swarm_mode());
    assert!(!EffortKind::Reasoning.is_swarm_mode());
}

#[test]
fn project_system_prompt_file_replaces_default_base_prompt() {
    use crate::prompt::load_base_system_prompt;

    let dir = std::env::temp_dir().join(format!("horon-agent-sysprompt-{}", std::process::id()));
    let horon_agent_dir = dir.join(".horon");
    std::fs::create_dir_all(&horon_agent_dir).unwrap();
    std::fs::write(horon_agent_dir.join("system-prompt.md"), "You are a custom agent.\n").unwrap();

    assert_eq!(load_base_system_prompt(Some(&dir)), "You are a custom agent.");

    let (prompt, _info) = build_system_prompt_full(None, &[], None, Some(&dir));
    assert!(prompt.contains("You are a custom agent."));
    assert!(!prompt.contains("HoronAgent is open source"));

    // Empty override falls back to the built-in default.
    std::fs::write(horon_agent_dir.join("system-prompt.md"), "   \n").unwrap();
    assert_eq!(load_base_system_prompt(Some(&dir)), DEFAULT_SYSTEM_PROMPT);

    std::fs::remove_dir_all(&dir).ok();
}

// Restore HORON_HOME even when a regression assertion panics.
fn with_prompt_guidance_home(test: impl FnOnce(&Path)) {
    let _guard = crate::storage::lock_test_env();
    struct RestoreHome(Option<std::ffi::OsString>);
    impl Drop for RestoreHome {
        fn drop(&mut self) {
            match self.0.take() {
                Some(value) => crate::env::set_var("HORON_HOME", value),
                None => crate::env::remove_var("HORON_HOME"),
            }
        }
    }
    let _restore = RestoreHome(std::env::var_os("HORON_HOME"));
    let home = tempfile::TempDir::new().unwrap();
    let horon_agent_dir = home.path().join(".horon");
    std::fs::create_dir(&horon_agent_dir).unwrap();
    crate::env::set_var("HORON_HOME", &horon_agent_dir);
    test(home.path());
}

#[test]
fn prompt_guidance_same_project_and_global_paths_are_loaded_once() {
    with_prompt_guidance_home(|home| {
        let overlay = "shared overlay marker\n";
        let tools = "shared preferred tools marker\n";
        std::fs::write(home.join(".horon/prompt-overlay.md"), overlay).unwrap();
        std::fs::write(home.join(".horon/preferred-tools.md"), tools).unwrap();

        let (full, full_info) = build_system_prompt_full(None, &[], None, Some(home));
        let (split, split_info) = build_system_prompt_split(None, &[], None, Some(home));
        for prompt in [&full, &split.static_part] {
            assert_eq!(prompt.matches(overlay.trim()).count(), 1);
            assert_eq!(prompt.matches(tools.trim()).count(), 1);
            assert!(prompt.contains("Project Prompt Overlay"));
            assert!(prompt.contains("Project Preferred Tools"));
            assert!(!prompt.contains("Global Prompt Overlay"));
            assert!(!prompt.contains("Global Preferred Tools"));
        }
        for info in [full_info, split_info] {
            assert_eq!(info.prompt_overlay_chars, overlay.len());
            assert_eq!(info.preferred_tools_chars, tools.len());
        }
    });
}

#[cfg(unix)]
#[test]
fn prompt_guidance_symlink_aliases_are_loaded_once() {
    with_prompt_guidance_home(|home| {
        let project = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(project.path().join(".horon")).unwrap();
        for name in ["prompt-overlay.md", "preferred-tools.md"] {
            let global = home.join(".horon").join(name);
            std::fs::write(&global, "shared symlink guidance").unwrap();
            std::os::unix::fs::symlink(&global, project.path().join(".horon").join(name)).unwrap();
        }
        for (content, size) in [
            load_prompt_overlay_files_from_dir(Some(project.path())),
            load_preferred_tools_files_from_dir(Some(project.path())),
        ] {
            let content = content.unwrap();
            assert_eq!(content.matches("shared symlink guidance").count(), 1);
            assert!(content.starts_with("# Project"));
            assert!(!content.contains("# Global"));
            assert_eq!(size, "shared symlink guidance".len());
        }
    });
}

#[test]
fn prompt_guidance_distinct_files_with_identical_contents_are_both_loaded() {
    with_prompt_guidance_home(|home| {
        let project = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(project.path().join(".horon")).unwrap();
        for name in ["prompt-overlay.md", "preferred-tools.md"] {
            for root in [home, project.path()] {
                std::fs::write(root.join(".horon").join(name), "identical guidance").unwrap();
            }
        }
        for (content, size) in [
            load_prompt_overlay_files_from_dir(Some(project.path())),
            load_preferred_tools_files_from_dir(Some(project.path())),
        ] {
            let content = content.unwrap();
            assert_eq!(content.matches("identical guidance").count(), 2);
            assert!(content.find("# Project").unwrap() < content.find("# Global").unwrap());
            assert_eq!(size, 2 * "identical guidance".len());
        }
    });
}

#[test]
fn prompt_guidance_missing_or_unreadable_project_keeps_global_content() {
    with_prompt_guidance_home(|home| {
        let project = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(project.path().join(".horon")).unwrap();
        for name in ["prompt-overlay.md", "preferred-tools.md"] {
            std::fs::write(home.join(".horon").join(name), "global only").unwrap();
        }
        for unreadable in [false, true] {
            if unreadable {
                for name in ["prompt-overlay.md", "preferred-tools.md"] {
                    std::fs::write(project.path().join(".horon").join(name), [0xff]).unwrap();
                }
            }
            for (content, size) in [
                load_prompt_overlay_files_from_dir(Some(project.path())),
                load_preferred_tools_files_from_dir(Some(project.path())),
            ] {
                let content = content.unwrap();
                assert_eq!(content.matches("global only").count(), 1);
                assert!(content.starts_with("# Global"));
                assert!(!content.contains("# Project"));
                assert_eq!(size, "global only".len());
            }
        }
    });
}

#[test]
fn foreign_rules_section_is_absent_unless_enabled() {
    let _guard = crate::storage::lock_test_env();
    let project = tempfile::TempDir::new().unwrap();
    std::fs::write(project.path().join(".cursorrules"), "cursor body").unwrap();

    let (off, _) = build_system_prompt_split_with_capabilities(
        None,
        &[],
        None,
        Some(project.path()),
        PromptCapabilities { foreign_rules: false },
    );
    assert!(!off.static_part.contains("cursor body"));
    assert!(!off.static_part.contains("# Imported Foreign Rules"));

    let (on, info) = build_system_prompt_split_with_capabilities(
        None,
        &[],
        None,
        Some(project.path()),
        PromptCapabilities { foreign_rules: true },
    );
    assert!(on.static_part.contains("# Imported Foreign Rules"));
    assert!(on.static_part.contains("cursor body"));
    assert!(info.foreign_rules_chars > 0);
    assert_eq!(
        info.foreign_rules_chars,
        info.prompt_prefix_chars() - info.system_prompt_chars
    );
}

#[test]
fn additional_tools_section_is_absent_when_empty_and_lists_rows_by_name() {
    assert!(
        build_additional_tools_section(&[]).is_none(),
        "a session that lists nothing adds no bytes to the cached prefix"
    );

    let rows = vec![
        (
            "undo".to_string(),
            "Reverse the newest file mutation of this session.".to_string(),
        ),
        ("ask".to_string(), "Ask the user a structured question.".to_string()),
    ];
    let section = build_additional_tools_section(&rows).expect("a non-empty index renders");

    assert!(section.contains("# Additional tools"), "{section}");
    assert!(
        section.contains("- ask: Ask the user a structured question."),
        "{section}"
    );
    assert!(
        section.contains("- undo: Reverse the newest file mutation of this session."),
        "{section}"
    );
    let ask = section.find("- ask:").expect("the ask row");
    let undo = section.find("- undo:").expect("the undo row");
    assert!(ask < undo, "rows are sorted by name, not by insertion order");
}

/// The shared one-line helper is what both the index and `toolbox list` print:
/// a description written for its own schema collapses to its first sentence,
/// and what still overruns the bound is clipped.
#[test]
fn one_line_description_keeps_the_first_line_and_clips_the_rest() {
    assert_eq!(
        one_line_description("\n\nFirst line.\nSecond line is never listed."),
        "First line."
    );
    assert_eq!(one_line_description(""), "");

    let long = "x".repeat(ONE_LINE_DESCRIPTION_MAX_CHARS + 40);
    let clipped = one_line_description(&long);
    assert_eq!(clipped.chars().count(), ONE_LINE_DESCRIPTION_MAX_CHARS);
    assert!(clipped.ends_with("..."), "{clipped}");
}

/// The first sentence ends the line before the bound does, so a description
/// that explains itself in two sentences costs only the first one.
#[test]
fn one_line_description_stops_at_the_first_sentence() {
    assert_eq!(
        one_line_description("Names one tool. Everything after it belongs to the schema."),
        "Names one tool."
    );
    assert_eq!(
        one_line_description("Serves HTTP/SSE.v2 endpoints without a terminator"),
        "Serves HTTP/SSE.v2 endpoints without a terminator",
        "a dot that is not followed by a space does not end a sentence"
    );

    let long_sentence = format!("{} tail.", "x".repeat(ONE_LINE_DESCRIPTION_MAX_CHARS + 20));
    let clipped = one_line_description(&long_sentence);
    assert_eq!(clipped.chars().count(), ONE_LINE_DESCRIPTION_MAX_CHARS);
    assert!(clipped.ends_with("..."), "{clipped}");
}

/// A row in the index carries exactly the line `toolbox list` carries, so the
/// two surfaces cannot disagree about the same tool.
#[test]
fn the_index_row_carries_the_clipped_line() {
    let description = "Names one tool. Everything after it belongs to the schema.";
    let rows = vec![("probe".to_string(), one_line_description(description))];
    let section = build_additional_tools_section(&rows).expect("a non-empty index renders");
    assert!(section.contains("- probe: Names one tool."), "{section}");
    assert!(!section.contains("belongs to the schema"), "{section}");
}

#[test]
fn foreign_rules_section_follows_native_agents_md() {
    let project = tempfile::TempDir::new().unwrap();
    std::fs::write(project.path().join("AGENTS.md"), "NATIVE AGENTS BODY").unwrap();
    std::fs::write(project.path().join(".cursorrules"), "foreign body").unwrap();

    let (prompt, _) = build_system_prompt_full_with_capabilities(
        None,
        &[],
        None,
        Some(project.path()),
        PromptCapabilities { foreign_rules: true },
    );
    let native = prompt.find("NATIVE AGENTS BODY").expect("native AGENTS.md body");
    let foreign = prompt.find("foreign body").expect("foreign rule body");
    assert!(native < foreign, "AGENTS.md must stay ahead of imported rules");
    assert_eq!(prompt.matches("NATIVE AGENTS BODY").count(), 1);
}
