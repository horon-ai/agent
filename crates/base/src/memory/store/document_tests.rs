//! Tests for the page document: encode/decode round trips, defaults, path
//! layout and validation.

use super::document;
use super::tests_support::{at, page};
use horon_agent_memory_types::MemoryCategory;
use horon_agent_memory_types::page::{MemoryPage, PageId, PageKind, PageLink, PageLinkKind};
use horon_agent_memory_types::{Reinforcement, TrustLevel};
use horon_agent_time::Timestamp;
use std::path::PathBuf;

fn full_page() -> MemoryPage {
    let mut page = page(
        "goal:6f9619ff-8b86-d011-b42d-00c04fc964ff",
        PageKind::Preference,
        MemoryCategory::Preference,
        "민열님은 아침 회의를 선호",
        "민열님은 아침 9시 이전 회의를 선호한다.\n---\n둘째 문단",
        1_700_000_000,
    );
    page.tags = vec!["schedule".to_string(), "meeting".to_string()];
    page.entities = vec!["민열".to_string(), "SURI".to_string()];
    page.links = vec![
        PageLink::relates_to(PageId::parse("mem_2_b").expect("id"), 0.25),
        PageLink::new(PageLinkKind::Supersedes, PageId::parse("mem_3_c").expect("id")),
    ];
    page.project = Some("horon-agent".to_string());
    page.valid_from = Some("2026-11-01".to_string());
    page.valid_to = Some("2026-12-31".to_string());
    page.confidence = 0.8;
    page.source = Some("conversation/2026-11-21".to_string());
    page.updated_at = at(1_700_000_100);
    page.access_count = 7;
    page.trust = TrustLevel::High;
    page.strength = 9;
    page.active = false;
    page.superseded_by = Some("mem_4_d".to_string());
    page.reinforcements = vec![Reinforcement {
        session_id: "session-1".to_string(),
        message_index: 3,
        timestamp: at(1_700_000_050),
    }];
    page
}

#[test]
fn encode_and_decode_round_trip_is_byte_identical() {
    let page = full_page();
    let first = document::encode(&page).expect("encode");
    let decoded = document::decode(&first, "unused-stem", at(0)).expect("decode");
    assert_eq!(decoded, page);
    let second = document::encode(&decoded).expect("encode again");
    assert_eq!(first, second);
    assert!(first.starts_with("---\n"), "frontmatter comes first");
    assert!(first.contains("title: \"민열님은 아침 회의를 선호\"\n"));
    assert!(first.ends_with("둘째 문단"), "the body keeps its own `---` line");
}

#[test]
fn a_hand_written_page_takes_its_id_from_the_file_name_and_its_time_from_mtime() {
    let text = "---\ntitle: \"손으로 쓴 페이지\"\nkind: \"fact\"\n---\n본문\n";
    let page = document::decode(
        text,
        "mem_9_z",
        Timestamp::from_timestamp_millis(1_700_000_000_000).expect("ts"),
    )
    .expect("decode");
    assert_eq!(page.id.as_str(), "mem_9_z");
    assert_eq!(page.kind, PageKind::Fact);
    assert_eq!(page.category, MemoryCategory::Fact);
    assert_eq!(page.title, "손으로 쓴 페이지");
    assert_eq!(page.body, "본문\n");
    assert_eq!(page.confidence, 1.0);
    assert_eq!(page.strength, 1);
    assert!(page.active);
    assert_eq!(page.created_at, page.updated_at);
}

#[test]
fn a_hostile_title_cannot_add_a_field_or_close_the_block_early() {
    let mut page = page("mem_1_a", PageKind::Fact, MemoryCategory::Fact, "제목", "본문", 1);
    page.title = "제목\"\nkind: event\n---\n가짜 본문".to_string();
    let text = document::encode(&page).expect("encode");
    let decoded = document::decode(&text, "mem_1_a", at(0)).expect("decode");
    assert_eq!(decoded.title, page.title);
    assert_eq!(decoded.kind, PageKind::Fact);
    assert_eq!(decoded.body, "본문");
}

#[test]
fn unknown_fields_and_other_schema_versions_are_refused() {
    let text = "---\nschema: 1\ntitle: \"t\"\nkind: \"fact\"\nevil: \"x\"\n---\n";
    let error = document::decode(text, "mem_1_a", at(0)).expect_err("unknown field");
    assert!(format!("{error}").contains("evil"), "{error}");

    let text = "---\nschema: 2\ntitle: \"t\"\nkind: \"fact\"\n---\n";
    let error = document::decode(text, "mem_1_a", at(0)).expect_err("schema");
    assert!(format!("{error}").contains("schema 2"), "{error}");

    for bad_schema in [
        "schema: \"2\"",
        "schema: \"1\"",
        "schema: 2.0",
        "schema: 1.0",
        "schema: true",
        "schema: null",
        "schema: []",
    ] {
        let text = format!("---\n{bad_schema}\ntitle: \"t\"\nkind: \"fact\"\n---\n");
        let error = document::decode(&text, "mem_1_a", at(0)).expect_err(&format!("{bad_schema} must be refused"));
        assert!(
            format!("{error}").contains("schema"),
            "unexpected error for {bad_schema}: {error}"
        );
    }
}

#[test]
fn pages_without_a_kind_are_refused() {
    let text = "---\ntitle: \"t\"\n---\n";
    assert!(document::decode(text, "mem_1_a", at(0)).is_err());
}

#[test]
fn page_paths_follow_the_kind_project_and_month_layout() {
    let mut preference = page("mem_1_a", PageKind::Preference, MemoryCategory::Preference, "t", "b", 1);
    assert_eq!(
        document::relative_path(&preference).expect("path"),
        PathBuf::from("preferences/mem_1_a.md")
    );

    preference.project = Some("horon-agent".to_string());
    assert_eq!(
        document::relative_path(&preference).expect("path"),
        PathBuf::from("preferences/horon-agent/mem_1_a.md")
    );

    let event = page(
        "mem_2_b",
        PageKind::Event,
        MemoryCategory::Fact,
        "t",
        "b",
        1_700_000_000,
    );
    assert_eq!(
        document::relative_path(&event).expect("path"),
        PathBuf::from("events/2023/11/mem_2_b.md")
    );

    let encoded = page(
        "goal:6f9619ff-8b86-d011-b42d-00c04fc964ff",
        PageKind::Entity,
        MemoryCategory::Entity,
        "t",
        "b",
        1,
    );
    let path = document::relative_path(&encoded).expect("path");
    let file = path.file_name().and_then(|name| name.to_str()).expect("name");
    assert!(file.starts_with('~'), "{file} must be the encoded id");
    assert_eq!(path.parent().expect("parent"), std::path::Path::new("entities"));
}

#[test]
fn validation_refuses_pages_that_would_break_the_store() {
    let mut cases: Vec<(&str, MemoryPage)> = Vec::new();

    let mut empty_title = page("mem_1_a", PageKind::Fact, MemoryCategory::Fact, "t", "b", 1);
    empty_title.title = "   ".to_string();
    cases.push(("empty title", empty_title));

    let mut nul_title = page("mem_1_a", PageKind::Fact, MemoryCategory::Fact, "t", "b", 1);
    nul_title.title = "제목\u{0}".to_string();
    cases.push(("NUL title", nul_title));

    let mut bad_date = page("mem_1_a", PageKind::Fact, MemoryCategory::Fact, "t", "b", 1);
    bad_date.valid_from = Some("2026/11/01".to_string());
    cases.push(("bad date", bad_date));

    let mut reversed = page("mem_1_a", PageKind::Fact, MemoryCategory::Fact, "t", "b", 1);
    reversed.valid_from = Some("2026-12-01".to_string());
    reversed.valid_to = Some("2026-11-01".to_string());
    cases.push(("reversed window", reversed));

    let mut bad_confidence = page("mem_1_a", PageKind::Fact, MemoryCategory::Fact, "t", "b", 1);
    bad_confidence.confidence = 1.5;
    cases.push(("confidence", bad_confidence));

    let mut self_link = page("mem_1_a", PageKind::Fact, MemoryCategory::Fact, "t", "b", 1);
    self_link.links = vec![PageLink::relates_to(PageId::parse("mem_1_a").expect("id"), 0.5)];
    cases.push(("self link", self_link));

    let mut weightless = page("mem_1_a", PageKind::Fact, MemoryCategory::Fact, "t", "b", 1);
    weightless.links = vec![PageLink::new(
        PageLinkKind::RelatesTo,
        PageId::parse("mem_2_b").expect("id"),
    )];
    cases.push(("relates_to without weight", weightless));

    let mut weighted = page("mem_1_a", PageKind::Fact, MemoryCategory::Fact, "t", "b", 1);
    weighted.links = vec![PageLink {
        kind: PageLinkKind::Supersedes,
        dst: PageId::parse("mem_2_b").expect("id"),
        weight: Some(0.5),
    }];
    cases.push(("weight on supersedes", weighted));

    let mut active_superseded = page("mem_1_a", PageKind::Fact, MemoryCategory::Fact, "t", "b", 1);
    active_superseded.superseded_by = Some("mem_2_b".to_string());
    cases.push(("active but superseded", active_superseded));

    let mut bad_category = page(
        "mem_1_a",
        PageKind::Custom,
        MemoryCategory::Custom("Fact".to_string()),
        "t",
        "b",
        1,
    );
    bad_category.category = MemoryCategory::Custom("Fact".to_string());
    cases.push(("uppercase custom category", bad_category));

    let mut builtin_category = page(
        "mem_1_a",
        PageKind::Custom,
        MemoryCategory::Custom("idea".to_string()),
        "t",
        "b",
        1,
    );
    builtin_category.category = MemoryCategory::Custom("fact".to_string());
    cases.push(("built-in custom category", builtin_category));

    let mut bad_project = page("mem_1_a", PageKind::Fact, MemoryCategory::Fact, "t", "b", 1);
    bad_project.project = Some("../escape".to_string());
    cases.push(("project traversal", bad_project));

    for (name, page) in cases {
        assert!(document::validate(&page).is_err(), "{name} must be refused");
    }
}

#[test]
fn an_empty_body_is_a_valid_page_with_an_empty_bod() {
    let page = page("mem_1_a", PageKind::Fact, MemoryCategory::Fact, "t", "", 1);
    let text = document::encode(&page).expect("encode");
    let decoded = document::decode(&text, "mem_1_a", at(0)).expect("decode");
    assert_eq!(decoded.body, "");
    assert_eq!(decoded, page);
}
