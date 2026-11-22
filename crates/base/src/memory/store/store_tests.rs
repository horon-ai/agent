//! Tests for the store: CRUD, versions, rebuild, corruption, delete semantics,
//! isolation and concurrent writers.

use super::tests_support::{TempRoot, at, fact, page, preference};
use super::{MemoryWiki, SearchQuery, document};
use horon_agent_memory_types::MemoryCategory;
use horon_agent_memory_types::MemoryPage;
use horon_agent_memory_types::page::{PageId, PageKind};

fn open(root: &TempRoot) -> MemoryWiki {
    MemoryWiki::open(root.path()).expect("open the wiki")
}

fn id(raw: &str) -> PageId {
    PageId::parse(raw).expect("valid id")
}

fn search_ids(wiki: &MemoryWiki, text: &str) -> Vec<String> {
    wiki.search(&SearchQuery {
        text: text.to_string(),
        limit: Some(10),
        ..SearchQuery::default()
    })
    .expect("search")
    .hits
    .into_iter()
    .map(|hit| hit.page.id)
    .collect()
}

#[test]
fn create_read_update_and_delete_a_page() {
    let root = TempRoot::new("crud");
    let mut wiki = open(&root);
    let mut page = preference(
        "mem_1001_a",
        "아침 회의 선호",
        "아침 9시 이전 회의를 선호한다.",
        1_700_000_000,
    );
    page.tags = vec!["schedule".to_string()];
    let outcome = wiki.write_page(&page).expect("write");
    assert_eq!(outcome.path, "preferences/mem_1001_a.md");
    assert_eq!(outcome.revision, 1);

    let file = root.path().join("wiki").join("preferences").join("mem_1001_a.md");
    let text = std::fs::read_to_string(&file).expect("page file");
    assert_eq!(text, document::encode(&page).expect("encode"));
    assert!(file.starts_with(root.path().join("wiki")), "inside the wiki root");

    let read_back = wiki.page(&id("mem_1001_a")).expect("page").expect("present");
    assert_eq!(read_back, page);
    assert_eq!(search_ids(&wiki, "아침 회의"), vec!["mem_1001_a"]);

    let mut updated = page.clone();
    updated.body = "아침 10시 회의도 괜찮다.".to_string();
    updated.updated_at = at(1_700_000_500);
    let outcome = wiki.write_page(&updated).expect("update");
    assert_eq!(outcome.revision, 2);
    assert_eq!(
        wiki.page(&id("mem_1001_a")).expect("page").expect("present").body,
        updated.body
    );
    let versions = wiki.versions(&id("mem_1001_a")).expect("versions");
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].rev, 1);
    assert_eq!(versions[0].body, "아침 9시 이전 회의를 선호한다.");
    assert_eq!(versions[1].rev, 2);
    assert_eq!(versions[1].body, updated.body);

    let deleted = wiki.delete_page(&id("mem_1001_a")).expect("delete");
    assert!(deleted.file_removed);
    assert!(deleted.rows_removed);
    assert!(!file.exists(), "the markdown file is gone");
    assert!(wiki.page(&id("mem_1001_a")).expect("page").is_none());
    assert!(wiki.versions(&id("mem_1001_a")).expect("versions").is_empty());
    assert!(wiki.list(&super::ListFilter::default()).expect("list").is_empty());
    assert!(search_ids(&wiki, "아침 회의").is_empty());
    assert_eq!(wiki.stats().expect("stats").pages, 0);
}

#[cfg(unix)]
#[test]
fn page_files_and_directories_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let root = TempRoot::new("perms");
    let mut wiki = open(&root);
    wiki.write_page(&fact("mem_1002_b", "배포", "배포는 화요일이다.", 1_700_000_000))
        .expect("write");
    let file = root.path().join("wiki").join("facts").join("mem_1002_b.md");
    let file_mode = std::fs::metadata(&file).expect("metadata").permissions().mode();
    assert_eq!(file_mode & 0o777, 0o600, "the page file is owner-only");
    let dir_mode = std::fs::metadata(root.path().join("wiki"))
        .expect("metadata")
        .permissions()
        .mode();
    assert_eq!(dir_mode & 0o777, 0o700, "the wiki directory is owner-only");
}

#[test]
fn changing_the_kind_moves_the_file() {
    let root = TempRoot::new("move");
    let mut wiki = open(&root);
    let mut page = fact("mem_1101_c", "배포", "배포는 화요일이다.", 1_700_000_000);
    wiki.write_page(&page).expect("write");
    let old_path = root.path().join("wiki").join("facts").join("mem_1101_c.md");
    assert!(old_path.exists());

    page.kind = PageKind::Preference;
    page.category = MemoryCategory::Preference;
    wiki.write_page(&page).expect("move");
    assert!(!old_path.exists(), "the old kind directory no longer holds the page");
    let new_path = root.path().join("wiki").join("preferences").join("mem_1101_c.md");
    assert!(new_path.exists());
    assert_eq!(
        wiki.page(&id("mem_1101_c")).expect("page").expect("present").kind,
        PageKind::Preference
    );
}

#[test]
fn a_deleted_page_does_not_come_back_after_a_rebuild() {
    let root = TempRoot::new("no-resurrection");
    let mut wiki = open(&root);
    wiki.write_page(&fact("mem_1201_d", "배포", "배포는 화요일이다.", 1_700_000_000))
        .expect("write");
    wiki.write_page(&fact(
        "mem_1202_e",
        "테스트",
        "테스트는 격리 홈에서 돈다.",
        1_700_000_000,
    ))
    .expect("write");
    wiki.delete_page(&id("mem_1201_d")).expect("delete");
    let report = wiki.reindex().expect("reindex");
    assert_eq!(report.indexed, 1);
    let listed: Vec<String> = wiki
        .list(&super::ListFilter::default())
        .expect("list")
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(listed, vec!["mem_1202_e"]);
    assert!(search_ids(&wiki, "배포").is_empty());
    drop(wiki);

    // A second process sees the same truth.
    let mut reopened = open(&root);
    assert!(reopened.page(&id("mem_1201_d")).expect("page").is_none());
    reopened.reindex().expect("reindex");
    assert!(search_ids(&reopened, "배포").is_empty());
}

#[test]
fn deleting_the_index_and_rebuilding_gives_the_same_results() {
    let root = TempRoot::new("rebuild");
    let mut wiki = open(&root);
    let mut first = preference("mem_1301_f", "아침 회의 선호", "아침 회의를 선호한다.", 1_700_000_000);
    first.tags = vec!["schedule".to_string()];
    wiki.write_page(&first).expect("write");
    wiki.write_page(&fact(
        "mem_1302_g",
        "배포 창구",
        "배포는 화요일과 목요일이다.",
        1_700_000_100,
    ))
    .expect("write");
    let mut third = page(
        "mem_1303_h",
        PageKind::Event,
        MemoryCategory::Fact,
        "디자인 리뷰",
        "디자인 리뷰는 11월 24일이다.",
        1_700_000_200,
    );
    third.tags = vec!["schedule".to_string()];
    wiki.write_page(&third).expect("write");

    let before_by_query: Vec<Vec<String>> = ["아침 회의", "배포", "디자인"]
        .iter()
        .map(|text| search_ids(&wiki, text))
        .collect();
    let before_listed: Vec<String> = wiki
        .list(&super::ListFilter::default())
        .expect("list")
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert!(!before_by_query[0].is_empty());
    drop(wiki);

    let index = root.path().join("index.sqlite");
    std::fs::remove_file(&index).expect("remove the index");
    let wiki = open(&root);
    let after_by_query: Vec<Vec<String>> = ["아침 회의", "배포", "디자인"]
        .iter()
        .map(|text| search_ids(&wiki, text))
        .collect();
    let after_listed: Vec<String> = wiki
        .list(&super::ListFilter::default())
        .expect("list")
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(before_by_query, after_by_query);
    assert_eq!(before_listed, after_listed);
}

#[test]
fn a_corrupt_index_is_rebuilt_from_the_files() {
    let root = TempRoot::new("corrupt");
    let mut wiki = open(&root);
    wiki.write_page(&fact("mem_1401_i", "배포", "배포는 화요일이다.", 1_700_000_000))
        .expect("write");
    drop(wiki);
    std::fs::write(root.path().join("index.sqlite"), b"this is not a database").expect("corrupt the index");
    let wiki = open(&root);
    assert_eq!(search_ids(&wiki, "배포"), vec!["mem_1401_i"]);
    assert_eq!(wiki.stats().expect("stats").pages, 1);
}

#[test]
fn an_index_from_another_schema_version_is_rebuilt() {
    let root = TempRoot::new("foreign-version");
    let mut wiki = open(&root);
    wiki.write_page(&fact("mem_1501_j", "배포", "배포는 화요일이다.", 1_700_000_000))
        .expect("write");
    drop(wiki);

    let connection = rusqlite::Connection::open(root.path().join("index.sqlite")).expect("open raw");
    connection
        .pragma_update(None, "user_version", 99i64)
        .expect("set a foreign version");
    drop(connection);

    let wiki = open(&root);
    assert_eq!(wiki.stats().expect("stats").pages, 1);
    assert_eq!(search_ids(&wiki, "배포"), vec!["mem_1501_j"]);
}

#[test]
fn a_malformed_page_is_reported_and_left_alone() {
    let root = TempRoot::new("malformed");
    let mut wiki = open(&root);
    wiki.write_page(&fact("mem_1601_k", "배포", "배포는 화요일이다.", 1_700_000_000))
        .expect("write");
    drop(wiki);

    let broken = root.path().join("wiki").join("facts").join("mem_1602_l.md");
    std::fs::write(&broken, "이 파일에는 frontmatter 가 없습니다\n").expect("write the broken page");
    let mut wiki = open(&root);
    let report = wiki.last_report();
    assert_eq!(report.skipped.len(), 1, "{report:?}");
    assert!(report.skipped[0].path.ends_with("mem_1602_l.md"));
    assert!(broken.exists(), "a malformed page is never deleted");
    assert_eq!(search_ids(&wiki, "배포"), vec!["mem_1601_k"]);

    // Fixing the file indexes it on the next reconcile.
    std::fs::write(&broken, "---\ntitle: \"고친 페이지\"\nkind: \"fact\"\n---\n고친 본문\n").expect("fix");
    let report = wiki.resync().expect("resync");
    assert_eq!(report.indexed, 1);
    assert_eq!(wiki.stats().expect("stats").pages, 2);
}

#[test]
fn a_symlinked_page_file_is_never_indexed_or_read() {
    let root = TempRoot::new("symlink");
    let outside = TempRoot::new("symlink-outside");
    let mut wiki = open(&root);
    wiki.write_page(&fact("mem_1701_m", "배포", "배포는 화요일이다.", 1_700_000_000))
        .expect("write");
    drop(wiki);

    let target = outside.path().join("outside.md");
    std::fs::write(
        &target,
        "---\nid: \"mem_9999_z\"\ntitle: \"바깥 페이지\"\nkind: \"fact\"\n---\n바깥 본문\n",
    )
    .expect("write the outside page");
    let link = root.path().join("wiki").join("facts").join("mem_9999_z.md");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&target, &link).expect("symlink");
    #[cfg(not(unix))]
    return;

    let wiki = open(&root);
    assert_eq!(wiki.stats().expect("stats").pages, 1, "the symlink is not a page");
    assert!(wiki.page(&id("mem_9999_z")).expect("page").is_none());
    assert!(link.exists(), "the symlink itself is left alone");
    let outside_text = std::fs::read_to_string(&target).expect("outside file");
    assert!(outside_text.contains("바깥 본문"), "the target is untouched");
}

#[test]
fn recorded_paths_that_escape_the_wiki_are_refused() {
    use super::safe_join;

    let root = TempRoot::new("escape");
    assert!(safe_join(root.path(), "../outside.md").is_err());
    assert!(safe_join(root.path(), "/etc/passwd").is_err());
    assert!(safe_join(root.path(), "a/../../b.md").is_err());
    assert!(safe_join(root.path(), "facts/mem_1_a.md").is_ok());
}

#[test]
fn concurrent_writers_lose_no_page() {
    let root = TempRoot::new("concurrency");
    {
        let mut wiki = open(&root);
        wiki.write_page(&fact("mem_1800_p", "시작", "시작 페이지", 1_700_000_000))
            .expect("write");
    }
    std::thread::scope(|scope| {
        for worker in 0..4 {
            let path = root.path().to_path_buf();
            scope.spawn(move || {
                let mut wiki = MemoryWiki::open(&path).expect("open");
                for slot in 0..5 {
                    let id = format!("mem_18{worker}{slot}_q");
                    let page = fact(
                        &id,
                        "동시 쓰기",
                        &format!("동시 쓰기 페이지 {worker}-{slot}"),
                        1_700_000_000 + i64::from(worker * 10 + slot),
                    );
                    wiki.write_page(&page).expect("write");
                }
            });
        }
    });

    let wiki = open(&root);
    let rows = wiki.list(&super::ListFilter::default()).expect("list");
    assert_eq!(rows.len(), 21, "the starting page plus twenty concurrent pages");
    assert_eq!(wiki.stats().expect("stats").pages, 21);
}

#[test]
fn secure_delete_leaves_no_body_bytes_behind() {
    let root = TempRoot::new("secure-delete");
    let marker = "SECRET-MARKER-7f3c9a";
    let mut wiki = open(&root);
    let mut page = fact("mem_1901_r", "민감한 사실", marker, 1_700_000_000);
    page.tags = vec![marker.to_string()];
    wiki.write_page(&page).expect("write");
    drop(wiki);

    let index = root.path().join("index.sqlite");
    let with_body = std::fs::read(&index).expect("read the index");
    assert!(
        contains(&with_body, marker.as_bytes()),
        "the marker is in the index before the delete, so the check below is meaningful"
    );

    let mut wiki = open(&root);
    wiki.delete_page(&id("mem_1901_r")).expect("delete");
    drop(wiki);

    for path in [
        root.path().join("index.sqlite"),
        root.path().join("index.sqlite-wal"),
        root.path().join("index.sqlite-shm"),
    ] {
        if let Ok(bytes) = std::fs::read(&path) {
            assert!(
                !contains(&bytes, marker.as_bytes()),
                "{} still holds the deleted body",
                path.display()
            );
        }
    }
    // The limit: WAL and VACUUM reclaim what SQLite allocated, but the
    // filesystem, the SSD and any backup snapshot are outside this guarantee.
    assert!(!root.path().join("wiki").join("facts").join("mem_1901_r.md").exists());
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

#[test]
fn listing_filters_by_kind_project_tag_active_and_limit() {
    let root = TempRoot::new("listing");
    let mut wiki = open(&root);
    let mut alpha = preference("mem_2001_s", "선호", "선호 본문", 1_700_000_000);
    alpha.project = Some("alpha".to_string());
    alpha.tags = vec!["release".to_string()];
    wiki.write_page(&alpha).expect("write");
    let mut superseded = fact("mem_2002_t", "사실", "사실 본문", 1_700_000_100);
    superseded.active = false;
    superseded.superseded_by = Some("mem_2001_s".to_string());
    wiki.write_page(&superseded).expect("write");

    let all = wiki.list(&super::ListFilter::default()).expect("list");
    assert_eq!(all.len(), 1, "inactive pages are left out by default");
    let with_inactive = wiki
        .list(&super::ListFilter {
            include_inactive: true,
            ..super::ListFilter::default()
        })
        .expect("list");
    assert_eq!(with_inactive.len(), 2);

    let only_preferences = wiki
        .list(&super::ListFilter {
            kinds: vec![PageKind::Preference],
            ..super::ListFilter::default()
        })
        .expect("list");
    assert_eq!(only_preferences.len(), 1);
    assert_eq!(only_preferences[0].id, "mem_2001_s");

    let only_alpha = wiki
        .list(&super::ListFilter {
            project: Some("alpha".to_string()),
            ..super::ListFilter::default()
        })
        .expect("list");
    assert_eq!(only_alpha.len(), 1);

    let only_tagged = wiki
        .list(&super::ListFilter {
            tags: vec!["release".to_string()],
            ..super::ListFilter::default()
        })
        .expect("list");
    assert_eq!(only_tagged.len(), 1);

    let limited = wiki
        .list(&super::ListFilter {
            include_inactive: true,
            limit: Some(1),
            ..super::ListFilter::default()
        })
        .expect("list");
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].id, "mem_2002_t", "newest first");
}

#[test]
fn a_page_round_trips_through_entry_and_back() {
    use horon_agent_memory_types::MemoryEntry;

    let mut entry = MemoryEntry::new(MemoryCategory::Custom("idea".to_string()), "아이디어 메모");
    entry.id = "skill:deploy checklist".to_string();
    entry.tags = vec!["release".to_string()];
    entry.access_count = 3;
    let page = MemoryPage::from_entry(&entry, None).expect("page");
    let root = TempRoot::new("entry");
    let mut wiki = open(&root);
    wiki.write_page(&page).expect("write");
    let read_back = wiki.page(&page.id).expect("page").expect("present");
    let entry_again = read_back.to_entry();
    assert_eq!(entry_again.id, entry.id);
    assert_eq!(entry_again.category, MemoryCategory::Custom("idea".to_string()));
    assert_eq!(entry_again.content, entry.content);
    assert_eq!(entry_again.tags, entry.tags);
    assert_eq!(entry_again.access_count, 3);
}
