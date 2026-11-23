//! The Korean assistant query evaluation.
//!
//! The fixture is a small curated set of synthetic memories and the keyword,
//! entity and unspaced-query forms a Korean-speaking user actually types. It is
//! not a public benchmark and no public number is cited: the point is to catch a
//! regression in Korean recall on this store, and to say out loud what the store
//! achieves today.
//!
//! A query counts as a hit at `k` when one of its expected page ids is in the
//! first `k` results. Recall is the fraction of queries that hit.

use super::tests_support::{TempRoot, at};
use super::{MemoryWiki, SearchQuery};
use horon_agent_memory_types::MemoryCategory;
use horon_agent_memory_types::page::{MemoryPage, PageId, PageKind};
use serde_json::Value;

const FIXTURE: &str = include_str!("korean_eval.json");

/// The measured floor: on 2026-11-23 the fixture scored 15/15 at each of k=1, 3
/// and 5. The floor is the measurement, so a ranking change that costs a query
/// fails here and must be re-measured instead of quietly lowering the bar.
const MIN_RECALL_AT_1: f64 = 1.0;
const MIN_RECALL_AT_3: f64 = 1.0;
const MIN_RECALL_AT_5: f64 = 1.0;

struct Fixture {
    pages: Vec<MemoryPage>,
    queries: Vec<(String, Vec<String>)>,
}

fn parse_fixture() -> Fixture {
    let root: Value = serde_json::from_str(FIXTURE).expect("the fixture is valid JSON");
    let pages = root["pages"]
        .as_array()
        .expect("pages")
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let id = value["id"].as_str().expect("id");
            let kind = PageKind::parse(value["kind"].as_str().expect("kind")).expect("known kind");
            let category: MemoryCategory = value["category"].as_str().expect("category").parse().expect("category");
            let mut page = MemoryPage::draft(
                PageId::parse(id).expect("page id"),
                kind,
                category,
                value["title"].as_str().expect("title"),
                value["body"].as_str().expect("body"),
                at(1_700_000_000 + index as i64 * 60),
            );
            page.tags = strings(&value["tags"]);
            page.entities = strings(&value["entities"]);
            page.project = value["project"].as_str().map(str::to_string);
            page
        })
        .collect();
    let queries = root["queries"]
        .as_array()
        .expect("queries")
        .iter()
        .map(|value| {
            (
                value["text"].as_str().expect("query text").to_string(),
                strings(&value["expect"]),
            )
        })
        .collect();
    Fixture { pages, queries }
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|item| item.as_str().expect("string").to_string())
        .collect()
}

fn recall_at(wiki: &MemoryWiki, queries: &[(String, Vec<String>)], k: usize) -> (f64, Vec<String>) {
    let mut hits = 0;
    let mut misses = Vec::new();
    for (text, expect) in queries {
        let results = wiki
            .search(&SearchQuery {
                text: text.clone(),
                limit: Some(k),
                ..SearchQuery::default()
            })
            .expect("search");
        let found = results
            .hits
            .iter()
            .any(|hit| expect.iter().any(|expected| expected == &hit.page.id));
        if found {
            hits += 1;
        } else {
            misses.push(text.clone());
        }
    }
    (hits as f64 / queries.len() as f64, misses)
}

#[test]
fn korean_assistant_queries_meet_the_recorded_recall() {
    let fixture = parse_fixture();
    assert_eq!(fixture.pages.len(), 14);
    assert_eq!(fixture.queries.len(), 15);

    let root = TempRoot::new("korean-eval");
    let mut wiki = MemoryWiki::open(root.path()).expect("open");
    for page in &fixture.pages {
        wiki.write_page(page).expect("write page");
    }

    let (recall_1, misses_1) = recall_at(&wiki, &fixture.queries, 1);
    let (recall_3, misses_3) = recall_at(&wiki, &fixture.queries, 3);
    let (recall_5, misses_5) = recall_at(&wiki, &fixture.queries, 5);

    println!(
        "Korean assistant query evaluation: {} queries, recall@1 {:.2}, recall@3 {:.2}, recall@5 {:.2}",
        fixture.queries.len(),
        recall_1,
        recall_3,
        recall_5
    );
    println!("  misses@1: {misses_1:?}");
    println!("  misses@3: {misses_3:?}");
    println!("  misses@5: {misses_5:?}");

    assert!(
        recall_1 >= MIN_RECALL_AT_1,
        "recall@1 dropped to {recall_1:.2} (floor {MIN_RECALL_AT_1:.2}); misses {misses_1:?}"
    );
    assert!(
        recall_3 >= MIN_RECALL_AT_3,
        "recall@3 dropped to {recall_3:.2} (floor {MIN_RECALL_AT_3:.2}); misses {misses_3:?}"
    );
    assert!(
        recall_5 >= MIN_RECALL_AT_5,
        "recall@5 dropped to {recall_5:.2} (floor {MIN_RECALL_AT_5:.2}); misses {misses_5:?}"
    );
    assert!(recall_1 <= recall_3 && recall_3 <= recall_5, "recall is monotonic in k");
}

#[test]
fn the_evaluation_is_stable_across_an_index_rebuild() {
    let fixture = parse_fixture();
    let root = TempRoot::new("korean-eval-rebuild");
    {
        let mut wiki = MemoryWiki::open(root.path()).expect("open");
        for page in &fixture.pages {
            wiki.write_page(page).expect("write page");
        }
    }
    let before = {
        let wiki = MemoryWiki::open(root.path()).expect("reopen");
        recall_at(&wiki, &fixture.queries, 5).0
    };
    std::fs::remove_file(root.path().join("index.sqlite")).expect("remove the index");
    let after = {
        let wiki = MemoryWiki::open(root.path()).expect("rebuild");
        recall_at(&wiki, &fixture.queries, 5).0
    };
    assert_eq!(before, after);
}
