use horon_agent_token_estimate::{TokenFamily, estimate_tokens_for_family};

const TOLERANCE: f64 = 0.15;

const KOREAN_PROSE: &str = "에이전트가 파일을 읽고 편집한 뒤 테스트를 실행합니다. 이 과정에서 컨텍스트가 얼마나 남았는지 정확히 알아야 압축을 제때 시작할 수 있습니다.";

const UNSPACED_KOREAN: &str = "에이전트가파일을읽고편집한뒤테스트를실행합니다이과정에서컨텍스트가얼마나남았는지정확히알아야압축을제때시작할수있습니다";

const ENGLISH_PROSE: &str = "The agent reads and edits files, then runs the tests. During that work it must know exactly how much context remains so compaction starts on time.";

const RUST_SAMPLE: &str = r"fn main() -> Result<()> {
    let cfg = Config::load()?;
    for item in cfg.items {
        process(&item)?;
    }
    Ok(())
}
";

const JSON_SAMPLE: &str =
    r#"{"schema":"pi.review.v1","findings":[{"file":"src/main.rs","line":42,"severity":"P1"}],"verdict":"BLOCK"}"#;

fn assert_within_15_percent(sample: &str, estimate: usize, measured: u32) {
    let low = measured as f64 * (1.0 - TOLERANCE);
    let high = measured as f64 * (1.0 + TOLERANCE);
    assert!(
        (estimate as f64) >= low && (estimate as f64) <= high,
        "{sample}: estimate {estimate}, measured {measured}"
    );
}

#[test]
fn korean_prose_within_15_percent() {
    assert_eq!(KOREAN_PROSE.chars().count(), 79);
    assert_within_15_percent(
        "korean_prose",
        estimate_tokens_for_family(KOREAN_PROSE, TokenFamily::O200k),
        44,
    );
    assert_within_15_percent(
        "korean_prose cl100k",
        estimate_tokens_for_family(KOREAN_PROSE, TokenFamily::Cl100k),
        72,
    );
}

#[test]
fn unspaced_korean_within_15_percent() {
    assert_eq!(UNSPACED_KOREAN.chars().count(), 59);
    assert_within_15_percent(
        "unspaced_korean",
        estimate_tokens_for_family(UNSPACED_KOREAN, TokenFamily::O200k),
        51,
    );
    assert_within_15_percent(
        "unspaced_korean cl100k",
        estimate_tokens_for_family(UNSPACED_KOREAN, TokenFamily::Cl100k),
        74,
    );
}

#[test]
fn english_prose_within_15_percent() {
    assert_eq!(ENGLISH_PROSE.chars().count(), 146);
    assert_within_15_percent(
        "english_prose",
        estimate_tokens_for_family(ENGLISH_PROSE, TokenFamily::O200k),
        30,
    );
}

#[test]
fn rust_sample_within_15_percent() {
    assert_eq!(RUST_SAMPLE.chars().count(), 129);
    assert_within_15_percent(
        "rust_sample",
        estimate_tokens_for_family(RUST_SAMPLE, TokenFamily::O200k),
        34,
    );
    assert_within_15_percent(
        "rust_sample cl100k",
        estimate_tokens_for_family(RUST_SAMPLE, TokenFamily::Cl100k),
        33,
    );
}

#[test]
fn json_sample_within_15_percent() {
    assert_eq!(JSON_SAMPLE.chars().count(), 105);
    assert_within_15_percent(
        "json_sample",
        estimate_tokens_for_family(JSON_SAMPLE, TokenFamily::O200k),
        33,
    );
    assert_within_15_percent(
        "json_sample cl100k",
        estimate_tokens_for_family(JSON_SAMPLE, TokenFamily::Cl100k),
        32,
    );
}

#[test]
fn json_aggregate_residual_is_documented() {
    let real = 33_u32;
    let estimate = estimate_tokens_for_family(JSON_SAMPLE, TokenFamily::O200k);
    let floor = (real as f64 * 0.76) as usize;
    assert!(
        estimate >= floor,
        "json_aggregate_residual_is_documented: estimate {estimate} must stay at or above 76% of          the measured {real} (floor {floor}). The crate README records the JSON residual at -23.8%;          a change that pushes it further must not pass silently."
    );
}

/// A realistic report body: nested objects, arrays, repeated keys, and
/// timestamps, so short alphanumeric segments sit between dense punctuation.
const SAMPLE_JSON: &str = include_str!("fixtures/sample.json");

/// The same document minified, where the punctuation density is highest.
const SAMPLE_MINIFIED_JSON: &str = include_str!("fixtures/sample_minified.json");

/// o200k counts for the two fixtures above, measured with tiktoken 0.14.0:
/// `tiktoken.get_encoding("o200k_base").encode(text)` over the file bytes
/// decoded as UTF-8.
const SAMPLE_JSON_O200K: u32 = 1412;
const SAMPLE_MINIFIED_JSON_O200K: u32 = 1094;

/// Structural JSON is held to a band chosen from the observed residual rather
/// than the prose anchors' 15%: the segment price only applies to runs at
/// least half punctuation, so the pretty-printed fixture stays at the prose
/// rate and lands 13.4% under its measured count. 14% is that residual rounded
/// up to the next even percentage point, and a change that widens it must fail
/// here rather than pass silently.
const JSON_FIXTURE_TOLERANCE: f64 = 0.14;

fn assert_within_tolerance(sample: &str, estimate: usize, measured: u32, tolerance: f64) {
    let low = measured as f64 * (1.0 - tolerance);
    let high = measured as f64 * (1.0 + tolerance);
    assert!(
        (estimate as f64) >= low && (estimate as f64) <= high,
        "{sample}: estimate {estimate} outside [{low:.2}, {high:.2}] for measured {measured}"
    );
}

#[test]
fn sample_json_within_tolerance() {
    let estimate = estimate_tokens_for_family(SAMPLE_JSON, TokenFamily::O200k);
    assert_within_tolerance("sample_json", estimate, SAMPLE_JSON_O200K, JSON_FIXTURE_TOLERANCE);
}

#[test]
fn sample_minified_json_within_tolerance() {
    let estimate = estimate_tokens_for_family(SAMPLE_MINIFIED_JSON, TokenFamily::O200k);
    assert_within_tolerance(
        "sample_minified_json",
        estimate,
        SAMPLE_MINIFIED_JSON_O200K,
        JSON_FIXTURE_TOLERANCE,
    );
}
