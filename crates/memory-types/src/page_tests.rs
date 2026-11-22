//! Tests for the shared page model.

use crate::graph::EdgeKind;
use crate::page::{
    MemoryPage, PAGE_ID_MAX_BYTES, PageId, PageIdError, PageKind, PageLink, PageLinkKind, derive_title, is_iso_date,
    validate_project_slug,
};
use crate::{MemoryCategory, MemoryEntry, Reinforcement, TrustLevel};
use horon_agent_time::Timestamp;

fn ts(seconds: i64) -> Timestamp {
    Timestamp::from_timestamp_millis(seconds * 1_000).expect("in range")
}

fn id(raw: &str) -> PageId {
    PageId::parse(raw).expect("valid page id")
}

#[test]
fn plain_ids_are_used_verbatim_as_file_names() {
    for raw in ["mem_1732000000000_42", "goal-abc", "a.b_c-9", "0start"] {
        let page_id = id(raw);
        assert_eq!(page_id.file_stem(), raw, "{raw} is already a plain stem");
        assert_eq!(page_id.file_name(), format!("{raw}.md"));
    }
}

#[test]
fn encoded_ids_round_trip_through_the_file_name() {
    for raw in [
        "goal:6f9619ff-8b86-d011-b42d-00c04fc964ff",
        "skill:deploy checklist",
        "Skill:Manage",
        "기억_선호",
        "Mem-1.2",
    ] {
        let page_id = id(raw);
        let stem = page_id.file_stem();
        assert!(stem.starts_with('~'), "{raw} needs the encoded form, got {stem}");
        assert!(
            stem.chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '~'),
            "{stem} must be lowercase ASCII so case-insensitive filesystems cannot collide"
        );
        assert_eq!(PageId::from_file_stem(&stem), Ok(page_id));
    }
}

#[test]
fn ids_that_would_escape_the_wiki_are_rejected() {
    assert_eq!(PageId::parse(""), Err(PageIdError::Empty));
    assert_eq!(PageId::parse("."), Err(PageIdError::LeadingDot));
    assert_eq!(PageId::parse(".."), Err(PageIdError::LeadingDot));
    assert_eq!(PageId::parse(".hidden"), Err(PageIdError::LeadingDot));
    assert_eq!(PageId::parse("../etc/passwd"), Err(PageIdError::LeadingDot));
    assert_eq!(PageId::parse("a/b"), Err(PageIdError::Separator));
    assert_eq!(PageId::parse("/abs"), Err(PageIdError::Separator));
    assert_eq!(PageId::parse("a\\b"), Err(PageIdError::Separator));
    assert_eq!(PageId::parse("a\0b"), Err(PageIdError::Control));
    assert_eq!(PageId::parse("a\nb"), Err(PageIdError::Control));
    assert_eq!(
        PageId::parse(&"x".repeat(PAGE_ID_MAX_BYTES + 1)),
        Err(PageIdError::TooLong {
            len: PAGE_ID_MAX_BYTES + 1
        })
    );
}

#[test]
fn only_canonical_file_stems_are_read_back() {
    // Uppercase is not the plain form, so `~41` is the canonical stem of "A".
    assert_eq!(
        PageId::from_file_stem("~41").map(|id| id.as_str().to_string()),
        Ok("A".to_string())
    );
    // ... and "A" itself is not a stem this module writes.
    assert_eq!(PageId::from_file_stem("A"), Err(PageIdError::NotCanonical));
    // `~61` decodes to "a", which would have been written plainly.
    assert_eq!(PageId::from_file_stem("~61"), Err(PageIdError::NotCanonical));
    assert_eq!(PageId::from_file_stem("~zz"), Err(PageIdError::BadEncoding));
    assert_eq!(PageId::from_file_stem("~6"), Err(PageIdError::BadEncoding));
    assert_eq!(PageId::from_file_stem("~ff"), Err(PageIdError::BadEncoding));
    // A hidden stem is not a canonical stem, so it never names a page.
    assert_eq!(PageId::from_file_stem(".hidden"), Err(PageIdError::NotCanonical));
}

#[test]
fn every_category_maps_to_a_kind_and_back() {
    let cases = [
        (MemoryCategory::Fact, PageKind::Fact),
        (MemoryCategory::Preference, PageKind::Preference),
        (MemoryCategory::Entity, PageKind::Entity),
        (MemoryCategory::Correction, PageKind::Correction),
        (MemoryCategory::Lesson, PageKind::Lesson),
        (MemoryCategory::Custom("idea".to_string()), PageKind::Custom),
    ];
    for (category, kind) in cases {
        assert_eq!(PageKind::from_category(&category), kind);
        assert_eq!(kind.as_str(), kind.to_string());
        assert_eq!(PageKind::parse(kind.as_str()), Some(kind));
    }
    // A kind with no category of its own falls back to the closest one.
    assert_eq!(PageKind::Person.default_category(), MemoryCategory::Entity);
    assert_eq!(PageKind::Event.default_category(), MemoryCategory::Fact);
    assert_eq!(PageKind::Procedure.default_category(), MemoryCategory::Lesson);
    assert_eq!(
        PageKind::Custom.default_category(),
        MemoryCategory::Custom("custom".to_string())
    );
    // The category travels in its own field, so a custom name is not lost.
    let page = MemoryPage::draft(
        id("mem_1_a"),
        PageKind::Custom,
        MemoryCategory::Custom("idea".to_string()),
        "아이디어",
        "본문",
        ts(1),
    );
    assert_eq!(page.category, MemoryCategory::Custom("idea".to_string()));
}

#[test]
fn entry_round_trip_carries_every_non_derived_field() {
    let mut entry = MemoryEntry::new(MemoryCategory::Preference, "민열님은 아침 회의를 선호한다");
    entry.id = "goal:6f9619ff-8b86-d011-b42d-00c04fc964ff".to_string();
    entry.tags = vec!["schedule".to_string(), "meeting".to_string()];
    entry.refresh_search_text();
    entry.created_at = ts(1_700_000_000);
    entry.updated_at = ts(1_700_000_100);
    entry.access_count = 7;
    entry.source = Some("conversation/2026-11-21".to_string());
    entry.trust = TrustLevel::High;
    entry.strength = 9;
    entry.active = false;
    entry.superseded_by = Some("mem_2_b".to_string());
    entry.reinforcements = vec![Reinforcement {
        session_id: "session-1".to_string(),
        message_index: 3,
        timestamp: ts(1_700_000_050),
    }];
    entry.embedding = Some(vec![0.5, 0.25]);
    entry.embedding_model = Some("openai:text-embedding-3-small".to_string());
    entry.confidence = 0.42;

    let page = MemoryPage::from_entry(&entry, Some("horon-agent".to_string())).expect("page");
    assert_eq!(page.kind, PageKind::Preference);
    assert_eq!(page.title, "민열님은 아침 회의를 선호한다");
    assert_eq!(page.body, entry.content);
    assert_eq!(page.project, Some("horon-agent".to_string()));

    let back = page.to_entry();
    assert_eq!(back.id, entry.id);
    assert_eq!(back.category, entry.category);
    assert_eq!(back.content, entry.content);
    assert_eq!(back.tags, entry.tags);
    assert_eq!(back.created_at, entry.created_at);
    assert_eq!(back.updated_at, entry.updated_at);
    assert_eq!(back.access_count, 7);
    assert_eq!(back.source, entry.source);
    assert_eq!(back.trust, TrustLevel::High);
    assert_eq!(back.strength, 9);
    assert!(!back.active);
    assert_eq!(back.superseded_by, entry.superseded_by);
    assert_eq!(back.reinforcements.len(), 1);
    assert_eq!(back.reinforcements[0].session_id, "session-1");
    assert_eq!(back.reinforcements[0].message_index, 3);
    assert_eq!(back.confidence, 0.42);
    // Embeddings are derived and never stored in the wiki.
    assert!(back.embedding.is_none());
    assert!(back.embedding_model.is_none());
    assert_eq!(back.search_text, entry.search_text);
}

#[test]
fn title_is_the_first_non_blank_line_cut_to_the_limit() {
    assert_eq!(derive_title("\n\n  첫 줄  \n둘째 줄"), "첫 줄");
    assert_eq!(derive_title("   "), "(untitled memory)");
    assert_eq!(derive_title("한 줄"), "한 줄");
    let long = "가".repeat(500);
    let title = derive_title(&long);
    assert_eq!(title.chars().count(), 400);
}

#[test]
fn link_kinds_convert_to_graph_edges_and_back() {
    let dst = id("mem_2_b");
    let relates = PageLink::relates_to(dst.clone(), 0.25);
    assert_eq!(relates.traversal_weight(), 0.25);
    assert_eq!(
        PageLinkKind::RelatesTo.to_edge_kind(Some(0.25)),
        EdgeKind::RelatesTo { weight: 0.25 }
    );
    assert_eq!(
        PageLinkKind::from_edge_kind(&EdgeKind::RelatesTo { weight: 0.25 }),
        Some((PageLinkKind::RelatesTo, Some(0.25)))
    );

    for (kind, weight) in [
        (PageLinkKind::Supersedes, 0.9),
        (PageLinkKind::Contradicts, 0.3),
        (PageLinkKind::DerivedFrom, 0.7),
    ] {
        let link = PageLink::new(kind, dst.clone());
        assert_eq!(link.weight, None);
        assert_eq!(link.traversal_weight(), weight);
        let edge = kind.to_edge_kind(None);
        assert_eq!(PageLinkKind::from_edge_kind(&edge), Some((kind, None)));
        assert_eq!(PageLinkKind::parse(kind.as_str()), Some(kind));
    }

    // Tag and cluster edges are graph bookkeeping, not page links.
    assert_eq!(PageLinkKind::from_edge_kind(&EdgeKind::HasTag), None);
    assert_eq!(PageLinkKind::from_edge_kind(&EdgeKind::InCluster), None);
    // The relates_to default weight matches the graph's default.
    assert_eq!(
        PageLinkKind::RelatesTo.to_edge_kind(None),
        EdgeKind::RelatesTo { weight: 1.0 }
    );
}

#[test]
fn validity_window_uses_open_bounds() {
    let mut page = MemoryPage::draft(id("mem_1_a"), PageKind::Fact, MemoryCategory::Fact, "t", "b", ts(1));
    assert!(page.is_valid_at(None));
    assert!(page.is_valid_at(Some("2026-11-01")));
    page.valid_from = Some("2026-11-01".to_string());
    page.valid_to = Some("2026-11-30".to_string());
    assert!(!page.is_valid_at(Some("2026-10-31")));
    assert!(page.is_valid_at(Some("2026-11-01")));
    assert!(page.is_valid_at(Some("2026-11-30")));
    assert!(!page.is_valid_at(Some("2026-12-01")));
    page.valid_to = None;
    assert!(page.is_valid_at(Some("2030-01-01")));
}

#[test]
fn project_slugs_are_one_safe_path_component() {
    for slug in ["horon-agent", "horon_agent", "a", "0abc", &"p".repeat(64)] {
        assert!(validate_project_slug(slug).is_ok(), "{slug} should be accepted");
    }
    for slug in [
        "",
        "Horon",
        "-lead",
        ".",
        "..",
        "a/b",
        "a\\b",
        "a b",
        "가",
        &"p".repeat(65),
    ] {
        assert!(validate_project_slug(slug).is_err(), "{slug} should be rejected");
    }
}

#[test]
fn dates_must_be_iso_dates() {
    assert!(is_iso_date("2026-11-01"));
    assert!(!is_iso_date("2026-1-1"));
    assert!(!is_iso_date("2026-11-01T00:00:00Z"));
    assert!(!is_iso_date("20261101"));
    assert!(!is_iso_date("2026/11/01"));
}
