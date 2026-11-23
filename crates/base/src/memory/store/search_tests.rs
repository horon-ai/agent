//! Tests for search: streams, fusion, filters, determinism and injection.

use super::tests_support::{TempRoot, event, fact, page, preference};
use super::{MemoryWiki, SearchQuery};
use horon_agent_memory_types::MemoryCategory;
use horon_agent_memory_types::page::{PageKind, PageLink};

fn store(label: &str) -> (TempRoot, MemoryWiki) {
    let root = TempRoot::new(label);
    let wiki = MemoryWiki::open(root.path()).expect("open the wiki");
    (root, wiki)
}

fn ids(wiki: &MemoryWiki, text: &str) -> Vec<String> {
    query(
        wiki,
        SearchQuery {
            text: text.to_string(),
            ..SearchQuery::default()
        },
    )
    .hits
    .into_iter()
    .map(|hit| hit.page.id)
    .collect()
}

fn query(wiki: &MemoryWiki, mut query: SearchQuery) -> super::SearchResults {
    query.limit.get_or_insert(10);
    wiki.search(&query).expect("search")
}

#[test]
fn unspaced_korean_query_is_found_through_the_bigram_stream() {
    let (_root, mut wiki) = store("bigram");
    wiki.write_page(&preference(
        "mem_1001_a",
        "민열님은 아침 회의를 선호",
        "민열님은 아침 9시 이전 회의를 선호한다.",
        1_700_000_000,
    ))
    .expect("write");
    wiki.write_page(&fact(
        "mem_1002_b",
        "빌드 잠금",
        "무거운 빌드는 build-lock으로 하나만 돌린다.",
        1_700_000_100,
    ))
    .expect("write");

    let results = query(
        &wiki,
        SearchQuery {
            text: "아침회의선호".to_string(),
            ..SearchQuery::default()
        },
    );
    assert_eq!(results.hits.len(), 1, "only the preference matches");
    let hit = &results.hits[0];
    assert_eq!(hit.page.id, "mem_1001_a");
    assert_eq!(hit.ranks.bigram, Some(1), "the bigram stream found it");
    assert_eq!(
        hit.ranks.trigram, None,
        "a two-syllable word is below the trigram minimum"
    );
}

#[test]
fn a_three_character_substring_is_found_through_the_trigram_stream() {
    let (_root, mut wiki) = store("trigram");
    wiki.write_page(&fact(
        "mem_1003_c",
        "배포 창구",
        "SURI 프로덕션 배포는 화요일과 목요일 저녁에 한다.",
        1_700_000_000,
    ))
    .expect("write");

    let results = query(
        &wiki,
        SearchQuery {
            text: "로덕션".to_string(),
            ..SearchQuery::default()
        },
    );
    assert_eq!(results.hits.len(), 1);
    assert_eq!(results.hits[0].page.id, "mem_1003_c");
    assert_eq!(results.hits[0].ranks.trigram, Some(1), "a substring inside a word");
}

#[test]
fn a_linked_neighbour_of_a_hit_is_returned() {
    let (_root, mut wiki) = store("links");
    let mut neighbour = fact("mem_2002_b", "디자인 리드", "디자인 리드를 맡는다.", 1_700_000_000);
    neighbour.links = vec![PageLink::relates_to(
        horon_agent_memory_types::page::PageId::parse("mem_2001_b").expect("id"),
        0.5,
    )];
    wiki.write_page(&fact(
        "mem_2001_b",
        "배포 창구",
        "SURI 프로덕션 배포는 화요일과 목요일이다.",
        1_700_000_100,
    ))
    .expect("write");
    wiki.write_page(&neighbour).expect("write");

    let results = query(
        &wiki,
        SearchQuery {
            text: "배포".to_string(),
            ..SearchQuery::default()
        },
    );
    let ids: Vec<&str> = results.hits.iter().map(|hit| hit.page.id.as_str()).collect();
    assert_eq!(ids, vec!["mem_2001_b", "mem_2002_b"], "the direct hit comes first");
    assert!(results.hits[0].ranks.link.is_none());
    assert_eq!(
        results.hits[1].ranks.link,
        Some(1),
        "the neighbour entered through the link stream"
    );
}

#[test]
fn entities_match_even_when_the_body_does_not_name_them() {
    let (_root, mut wiki) = store("entities");
    let mut page = fact("mem_3001_c", "디자인 리드", "디자인 리드를 맡고 있다.", 1_700_000_000);
    page.entities = vec!["김서연".to_string(), "SURI".to_string()];
    wiki.write_page(&page).expect("write");

    let results = query(
        &wiki,
        SearchQuery {
            text: "김서연".to_string(),
            ..SearchQuery::default()
        },
    );
    assert_eq!(results.hits.len(), 1);
    assert_eq!(results.hits[0].ranks.entity, Some(1));
    // The entity column is also in the token index, which is why the page is
    // reachable by its entity name alone.
    assert_eq!(results.hits[0].ranks.tokens, Some(1));
    assert!(!results.hits[0].page.title.contains("김서연"));
    assert_eq!(results.hits[0].page.id, "mem_3001_c");
}

#[test]
fn kind_authority_orders_equally_relevant_pages() {
    let (_root, mut wiki) = store("authority");
    let body = "분기 계획 회의는 12월 2일 오전 10시에 열린다.";
    wiki.write_page(&event("mem_4001_d", "회의", body, 1_700_000_000))
        .expect("write");
    wiki.write_page(&preference("mem_4002_e", "회의", body, 1_700_000_000))
        .expect("write");

    let results = query(
        &wiki,
        SearchQuery {
            text: "분기 계획 회의".to_string(),
            ..SearchQuery::default()
        },
    );
    let ids: Vec<&str> = results.hits.iter().map(|hit| hit.page.id.as_str()).collect();
    assert_eq!(ids, vec!["mem_4002_e", "mem_4001_d"], "a preference outranks an event");
    assert_eq!(results.hits[0].ranks.authority, Some(1));
    assert_eq!(results.hits[1].ranks.authority, Some(2));
}

#[test]
fn recency_breaks_equal_relevance() {
    let (_root, mut wiki) = store("recency");
    let body = "테스트는 격리된 HOME에서 실행한다.";
    wiki.write_page(&fact("mem_5001_f", "격리", body, 1_700_000_000))
        .expect("write");
    wiki.write_page(&fact("mem_5002_g", "격리", body, 1_700_100_000))
        .expect("write");

    let results = query(
        &wiki,
        SearchQuery {
            text: "테스트 격리".to_string(),
            ..SearchQuery::default()
        },
    );
    let ids: Vec<&str> = results.hits.iter().map(|hit| hit.page.id.as_str()).collect();
    assert_eq!(ids, vec!["mem_5002_g", "mem_5001_f"], "the newer page wins the tie");
    assert_eq!(results.hits[0].ranks.recency, Some(1));
    assert_eq!(results.hits[1].ranks.recency, Some(2));
}

#[test]
fn filters_restrict_kind_project_tags_and_validity() {
    let (_root, mut wiki) = store("filters");
    let mut alpha = preference(
        "mem_6001_h",
        "배포 선호",
        "배포는 금요일 저녁을 선호한다.",
        1_700_000_000,
    );
    alpha.project = Some("alpha".to_string());
    alpha.tags = vec!["release".to_string()];
    alpha.valid_from = Some("2026-11-01".to_string());
    alpha.valid_to = Some("2026-11-30".to_string());
    wiki.write_page(&alpha).expect("write");
    let mut beta = fact("mem_6002_i", "배포 창구", "배포는 수요일과 금요일이다.", 1_700_000_000);
    beta.project = Some("beta".to_string());
    wiki.write_page(&beta).expect("write");

    let all = ids(&wiki, "배포");
    assert_eq!(all.len(), 2);

    let only_facts = query(
        &wiki,
        SearchQuery {
            text: "배포".to_string(),
            kinds: vec![PageKind::Fact],
            ..SearchQuery::default()
        },
    );
    assert_eq!(only_facts.hits.len(), 1);
    assert_eq!(only_facts.hits[0].page.id, "mem_6002_i");

    let only_alpha = query(
        &wiki,
        SearchQuery {
            text: "배포".to_string(),
            project: Some("alpha".to_string()),
            ..SearchQuery::default()
        },
    );
    assert_eq!(only_alpha.hits.len(), 1);
    assert_eq!(only_alpha.hits[0].page.id, "mem_6001_h");

    let only_tagged = query(
        &wiki,
        SearchQuery {
            text: "배포".to_string(),
            tags: vec!["release".to_string()],
            ..SearchQuery::default()
        },
    );
    assert_eq!(only_tagged.hits.len(), 1);
    assert_eq!(only_tagged.hits[0].page.id, "mem_6001_h");

    let in_window = query(
        &wiki,
        SearchQuery {
            text: "배포".to_string(),
            at: Some("2026-11-15".to_string()),
            ..SearchQuery::default()
        },
    );
    assert_eq!(in_window.hits.len(), 2, "both pages are valid on that date");

    let outside_window = query(
        &wiki,
        SearchQuery {
            text: "배포".to_string(),
            at: Some("2026-12-15".to_string()),
            ..SearchQuery::default()
        },
    );
    assert_eq!(outside_window.hits.len(), 1);
    assert_eq!(outside_window.hits[0].page.id, "mem_6002_i");
}

#[test]
fn a_bad_date_filter_is_refused() {
    let (_root, wiki) = store("bad-date");
    let error = wiki
        .search(&SearchQuery {
            text: "배포".to_string(),
            at: Some("2026/11/15".to_string()),
            ..SearchQuery::default()
        })
        .expect_err("bad date");
    assert!(format!("{error}").contains("YYYY-MM-DD"), "{error}");
}

#[test]
fn query_syntax_is_never_injected_into_sqlite() {
    let (_root, mut wiki) = store("injection");
    wiki.write_page(&fact("mem_7001_j", "배포 창구", "배포는 화요일이다.", 1_700_000_000))
        .expect("write");
    for text in [
        "\" OR *",
        "NEAR(배포 회의, 3)",
        "배포 -회의",
        "(((",
        "배포\"; DROP TABLE pages; --",
        "*",
        "^배포$",
    ] {
        let results = wiki
            .search(&SearchQuery {
                text: text.to_string(),
                limit: Some(5),
                ..SearchQuery::default()
            })
            .unwrap_or_else(|error| panic!("query `{text}` must not fail: {error}"));
        // The table is still there: the next search still works.
        assert!(wiki.stats().expect("stats").pages == 1);
        drop(results);
    }
    assert_eq!(ids(&wiki, "배포"), vec!["mem_7001_j"]);
}

#[test]
fn the_same_query_returns_the_same_order_every_time() {
    let (_root, mut wiki) = store("deterministic");
    for index in 0..5 {
        let mut page = fact(
            &format!("mem_800{index}_k"),
            "배포 창구",
            "배포는 화요일과 목요일에 한다.",
            1_700_000_000 + index,
        );
        if index == 0 {
            page.category = MemoryCategory::Fact;
        }
        wiki.write_page(&page).expect("write");
    }
    let first = ids(&wiki, "배포");
    let second = ids(&wiki, "배포");
    assert_eq!(first, second);
    assert_eq!(first.len(), 5);
}

#[test]
fn empty_and_whitespace_queries_return_nothing() {
    let (_root, mut wiki) = store("empty");
    wiki.write_page(&fact("mem_9001_l", "배포", "배포는 화요일이다.", 1_700_000_000))
        .expect("write");
    assert!(ids(&wiki, "").is_empty());
    assert!(ids(&wiki, "   ").is_empty());
}

#[test]
fn a_page_that_only_matches_in_the_title_is_still_found() {
    let (_root, mut wiki) = store("title");
    wiki.write_page(&page(
        "mem_9101_m",
        PageKind::Procedure,
        MemoryCategory::Lesson,
        "릴리스 체크리스트",
        "경고 0과 벤치마크 기준선을 확인한다.",
        1_700_000_000,
    ))
    .expect("write");
    assert_eq!(ids(&wiki, "릴리스"), vec!["mem_9101_m"]);
}
