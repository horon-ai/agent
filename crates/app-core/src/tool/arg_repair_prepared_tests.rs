//! The prepared-schema cache: a schema is compiled once, found again without
//! its JSON text being built, and never served for a different schema.

use super::{PREPARED_CAP, PreparedCache, fingerprint, prepared_at, repair_recorded};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

fn count_schema(kind: &str) -> Value {
    json!({
        "type": "object",
        "properties": { "count": { "type": kind } },
        "required": ["count"]
    })
}

fn entries(cache: &PreparedCache) -> usize {
    cache.lock().expect("the test cache is not poisoned").len()
}

#[test]
fn a_schema_seen_again_is_not_prepared_again() {
    let cache = PreparedCache::default();
    let schema = count_schema("integer");
    let first = prepared_at(&cache, fingerprint(&schema), &schema);
    // A tool builds its schema afresh on every call, so the second lookup
    // arrives as a different value with the same content.
    let rebuilt = count_schema("integer");
    let second = prepared_at(&cache, fingerprint(&rebuilt), &rebuilt);

    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(entries(&cache), 1);
}

#[test]
fn equal_schemas_share_a_fingerprint_and_different_ones_do_not() {
    let integer = count_schema("integer");
    assert_eq!(fingerprint(&integer), fingerprint(&count_schema("integer")));
    assert_ne!(fingerprint(&integer), fingerprint(&count_schema("string")));
    assert_ne!(fingerprint(&json!({"a": 1})), fingerprint(&json!({"a": "1"})));
    assert_ne!(fingerprint(&json!(["ab", "c"])), fingerprint(&json!(["a", "bc"])));
}

#[test]
fn two_schemas_under_one_key_never_share_a_validator() {
    const SHARED_KEY: u64 = 7;
    let cache = PreparedCache::default();
    let integer = count_schema("integer");
    let string = count_schema("string");
    let text = json!({ "count": "5" });

    let for_integer = prepared_at(&cache, SHARED_KEY, &integer);
    let for_string = prepared_at(&cache, SHARED_KEY, &string);
    let for_integer_again = prepared_at(&cache, SHARED_KEY, &integer);

    assert_eq!(for_integer.source, integer);
    assert_eq!(for_string.source, string);
    assert_eq!(for_integer_again.source, integer);
    let valid = |prepared: &super::Prepared| {
        prepared
            .validator
            .as_ref()
            .expect("an object schema compiles")
            .is_valid(&text)
    };
    assert!(!valid(&for_integer));
    assert!(valid(&for_string));
    assert!(!valid(&for_integer_again));
    assert_eq!(entries(&cache), 1, "the key holds the schema seen last");
}

#[test]
fn a_tool_whose_schema_changes_is_repaired_against_the_new_one() {
    let sent = json!({ "count": "5" });

    let as_integer = repair_recorded(&count_schema("integer"), sent.clone());
    assert_eq!(as_integer.input, json!({ "count": 5 }));
    assert_eq!(as_integer.original, Some(sent.clone()));
    assert!(!as_integer.changes.is_empty());

    let as_string = repair_recorded(&count_schema("string"), sent.clone());
    assert_eq!(as_string.input, sent);
    assert_eq!(as_string.original, None);
    assert!(as_string.changes.is_empty());

    let as_integer_again = repair_recorded(&count_schema("integer"), sent.clone());
    assert_eq!(as_integer_again.input, json!({ "count": 5 }));
}

#[test]
fn a_schema_that_is_not_an_object_passes_input_through() {
    let cache = PreparedCache::default();
    let schema = json!("not a schema");
    let prepared = prepared_at(&cache, fingerprint(&schema), &schema);
    assert!(prepared.validator.is_none());

    let sent = json!({ "count": "5" });
    let repaired = repair_recorded(&schema, sent.clone());
    assert_eq!(repaired.input, sent);
    assert_eq!(repaired.original, None);
}

#[test]
fn a_full_cache_is_emptied_before_the_next_schema() {
    let cache: PreparedCache = Mutex::default();
    for index in 0..PREPARED_CAP {
        // Not object-shaped, so nothing is compiled.
        let schema = json!(index);
        prepared_at(&cache, fingerprint(&schema), &schema);
    }
    assert_eq!(entries(&cache), PREPARED_CAP);

    let one_more = json!("one more");
    prepared_at(&cache, fingerprint(&one_more), &one_more);
    assert_eq!(entries(&cache), 1);
}
