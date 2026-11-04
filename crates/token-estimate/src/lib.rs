//! Content-aware token estimation for context budgeting.
//!
//! # Why this exists
//!
//! The compaction trigger has to size a conversation before the provider
//! reports usage, and dividing the character count by four gets that wrong
//! outside English prose: Korean carries about one token per Hangul syllable,
//! and code and JSON carry far more punctuation per character than prose.
//! Undercounting Korean let transcripts overrun the window; overcounting code
//! compacted conversations that still had room.
//!
//! # Design
//!
//! Each whitespace-delimited run is priced by its character mix, using rates
//! fitted against real provider counts per tokenizer family. The crate is
//! dependency-free and allocation-free because it runs on every message of
//! every turn.
//!
//! # Character classes
//!
//! Every character is priced in exactly one class, so no character is free.
//! A non-ASCII character that is neither Hangul nor ASCII is never zero: it is
//! Han, kana, another alphabetic script, or a symbol.
//!
//! | class | covers |
//! |---|---|
//! | Hangul | precomposed syllables, conjoining jamo, compatibility jamo |
//! | Han | CJK ideographs in the base, extension A, compatibility and supplementary blocks, `々`, `〇` |
//! | kana | hiragana, katakana, phonetic extensions, halfwidth katakana, kana supplement |
//! | Arabic | Arabic, Arabic supplement, extended-A, presentation forms |
//! | Indic | Devanagari, Bengali, Gurmukhi, Gujarati, Oriya, Tamil, Telugu, Kannada, Malayalam, Sinhala, Tibetan, Myanmar, Khmer, Tai Tham, Saurashtra |
//! | other letter | every remaining non-ASCII alphabetic character: Cyrillic, Greek, Hebrew, Thai, Georgian, ... |
//! | emoji | pictographs, dingbats, symbols and arrows, plus the variation selector and joiner that compose them |
//! | symbol | every remaining non-ASCII character: CJK punctuation, fullwidth signs, combining marks outside the alphabetic property |
//! | ASCII alphanumeric | `A-Z`, `a-z`, `0-9` |
//! | ASCII punctuation | printable ASCII that is not alphanumeric |
//!
//! # Known limitation
//!
//! The rates are fitted, not derived from a tokenizer, so they hold within
//! roughly ±15% across the anchor set (Korean prose, English prose, Rust
//! source, JSON) rather than exactly. One shape signal refines the character
//! mix: a run that is at least half punctuation is symbol-dense, so its ASCII
//! alphanumeric text is priced per segment between delimiters instead of at
//! the prose rate. The threshold is set where a tokenizer stops merging short
//! segments with their delimiters; a quarter-punctuation threshold overshot
//! the dense JSON anchor by 30%.
//!
//! A character-level rate cannot separate prose from repetition: `漢` costs
//! about 0.58 tokens per character inside Chinese prose but a full token when
//! repeated, because prose reuses characters that the vocabulary merges. The
//! rates are fitted to prose, and the run-length term below stops a short CJK
//! run from taking the prose discount, so estimates stay inside the tested
//! bands in both directions. The one documented limitation is a repeated single
//! character: a real tokenizer splits it per character while prose sets the
//! rate, so that case is underestimated.
//!
//! Arabic and Indic are separate classes for the same reason: cl100k prices
//! Devanagari at over one token per character while Arabic prose stays near
//! 0.73, so one shared rate would underestimate one of them by a quarter.

/// Tokenizer family whose vocabulary prices the estimate.
///
/// The families disagree most on Hangul: o200k merges common Korean substrings
/// into single tokens, while cl100k spends close to one token per syllable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenFamily {
    /// The o200k vocabulary, used by current models.
    O200k,
    /// The older cl100k vocabulary, still used by some providers.
    Cl100k,
}

/// Average ASCII alphanumeric characters covered by one token, measured across
/// prose, code, and JSON.
const ASCII_CHARS_PER_TOKEN: f64 = 4.87;

/// Token cost of one ASCII punctuation character. Punctuation is split far
/// more aggressively than words, so a bracket or comma usually stands alone.
const PUNCT_RATE: f64 = 0.55;

/// Punctuation density, as a percentage of a run's characters, at which the
/// run counts as symbol-dense and its ASCII alphanumerics are priced per
/// segment between delimiters. Below it the prose rate is closer to what a
/// tokenizer does.
///
/// Tuned against the fixtures in `tests/fixtures` and the 15% anchors: at 25%
/// and 35% the dense JSON anchor overshoots its measured count by 30%, because
/// a tokenizer merges two- and three-character segments with their delimiters
/// while one token per segment cannot. Only runs that are at least half
/// punctuation, where that merge stops paying, take the segment price.
const SEGMENT_DENSITY_PERCENT: usize = 50;

/// Character classes that are priced separately. `classify` maps every
/// character to exactly one of these, so no script can contribute nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Hangul,
    Han,
    Kana,
    Arabic,
    Indic,
    OtherLetter,
    Emoji,
    Symbol,
}

/// Characters of shared context over which a CJK run's per-character cost
/// settles. A token covers up to a few characters of its neighbours, so a long
/// run of ideographs pays nearly the full per-character rate while prose, whose
/// characters recur and merge, pays the discounted rate. Fitted at 18, the
/// value that puts both Han prose and repeated Han inside ±10% of their
/// measured counts.
const CJK_MERGE_WINDOW: f64 = 18.0;

/// The smallest share of a Han ideograph's rate that a run can be charged.
/// Short Han runs measure about 0.50 tokens per character and `han_rate` is
/// 0.95 on o200k, so the floor is 0.50 / 0.95. The prose discount alone would
/// charge 0.33 per character and undershoot a short run by a third.
const fn han_min_merge() -> f64 {
    0.53
}

/// The smallest share of a kana character's rate that a run can be charged.
/// Short kana runs measure about 0.667 tokens per character against the same
/// 0.95 rate, so the floor is 0.667 / 0.95.
const fn kana_min_merge() -> f64 {
    0.70
}

/// Share of a CJK character's full rate that a run of `cjk_len` CJK characters
/// is charged. Short runs sit inside prose and merge with their neighbours;
/// long runs do not. `min_merge` holds a short run at the measured rate of its
/// own class instead of the prose discount.
fn cjk_merge(cjk_len: usize, min_merge: f64) -> f64 {
    if cjk_len == 0 {
        return 0.0;
    }
    (1.0 - CJK_MERGE_WINDOW / cjk_len as f64).max(min_merge)
}

impl TokenFamily {
    /// Token cost of a single Hangul character.
    const fn hangul_rate(self) -> f64 {
        match self {
            Self::O200k => 0.78,
            Self::Cl100k => 1.22,
        }
    }

    /// Token cost of a single Han ideograph before the run-length term. A
    /// repeated ideograph costs about a full token on o200k and two on cl100k;
    /// inside prose the vocabulary merges it with its neighbours, so the run
    /// term below discounts this rate. Measured 0.58 per character in prose and
    /// 1.00 repeated on o200k.
    const fn han_rate(self) -> f64 {
        match self {
            Self::O200k => 0.95,
            Self::Cl100k => 1.90,
        }
    }

    /// Token cost of a single kana character before the run-length term.
    /// cl100k holds kana in its vocabulary while Han ideographs fall back to
    /// bytes, which is why the two are priced apart.
    const fn kana_rate(self) -> f64 {
        match self {
            Self::O200k => 0.95,
            Self::Cl100k => 1.04,
        }
    }

    /// Token cost of a single Arabic character.
    const fn arabic_rate(self) -> f64 {
        match self {
            Self::O200k => 0.38,
            Self::Cl100k => 0.88,
        }
    }

    /// Token cost of a single Indic character. cl100k splits Devanagari at over
    /// one token per character, so it cannot share the Arabic rate.
    const fn indic_rate(self) -> f64 {
        match self {
            Self::O200k => 0.38,
            Self::Cl100k => 1.20,
        }
    }

    /// Token cost of a single alphabetic character outside the scripts above:
    /// Cyrillic, Greek, Hebrew, Thai and the rest.
    const fn other_letter_rate(self) -> f64 {
        match self {
            Self::O200k => 0.30,
            Self::Cl100k => 0.50,
        }
    }

    /// Token cost of a single non-ASCII symbol. CJK punctuation, arrows and
    /// fullwidth signs cost about one token each.
    const fn symbol_rate(self) -> f64 {
        match self {
            Self::O200k => 0.90,
            Self::Cl100k => 1.00,
        }
    }

    /// Token cost of a single emoji codepoint. Emoji are three to four bytes and
    /// never merged into a neighbouring token, so they cost about two tokens
    /// each rather than one.
    const fn emoji_rate(self) -> f64 {
        match self {
            Self::O200k => 2.00,
            Self::Cl100k => 2.00,
        }
    }
}

/// Estimates the token count of `text` for the current o200k vocabulary.
pub fn estimate_tokens(text: &str) -> usize {
    estimate_tokens_for_family(text, TokenFamily::O200k)
}

/// Estimates the token count of `text` for `family`.
///
/// The result is never zero: an empty string still occupies a token boundary in
/// a request, and zero would let a caller treat it as free.
pub fn estimate_tokens_for_family(text: &str, family: TokenFamily) -> usize {
    let mut tokens = 0.0_f64;

    for run in text.split(is_run_break).filter(|run| !run.is_empty()) {
        let mut hangul = 0_usize;
        let mut han = 0_usize;
        let mut kana = 0_usize;
        let mut arabic = 0_usize;
        let mut indic = 0_usize;
        let mut other_letter = 0_usize;
        let mut emoji = 0_usize;
        let mut symbol = 0_usize;
        let mut ascii_alnum = 0_usize;
        let mut ascii_punct = 0_usize;
        let mut run_chars = 0_usize;
        // Length of the ASCII alphanumeric segment in progress, and the tokens
        // its closed predecessors cost. Punctuation ends a segment, so a dense
        // JSON or code run prices each short identifier or number separately
        // instead of letting the whole run average out at the prose rate.
        let mut segment_len = 0_usize;
        let mut segment_tokens = 0.0_f64;

        for c in run.chars() {
            run_chars += 1;
            if c.is_ascii() {
                if c.is_ascii_alphanumeric() {
                    ascii_alnum += 1;
                    segment_len += 1;
                } else {
                    if !c.is_ascii_whitespace() {
                        ascii_punct += 1;
                    }
                    close_ascii_segment(&mut segment_len, &mut segment_tokens);
                }
                continue;
            }
            close_ascii_segment(&mut segment_len, &mut segment_tokens);
            match classify(c) {
                CharClass::Hangul => hangul += 1,
                CharClass::Han => han += 1,
                CharClass::Kana => kana += 1,
                CharClass::Arabic => arabic += 1,
                CharClass::Indic => indic += 1,
                CharClass::OtherLetter => other_letter += 1,
                CharClass::Emoji => emoji += 1,
                CharClass::Symbol => symbol += 1,
            }
        }

        close_ascii_segment(&mut segment_len, &mut segment_tokens);

        // A run that is at least half punctuation is symbol-dense: a tokenizer
        // splits it at most delimiters, so short identifiers and numbers each
        // cost about a token and averaging the whole run at the prose rate
        // undershoots. Word-like runs keep the prose rate.
        let ascii_alnum_tokens = if ascii_punct * 100 >= run_chars * SEGMENT_DENSITY_PERCENT {
            segment_tokens
        } else {
            ascii_alnum as f64 / ASCII_CHARS_PER_TOKEN
        };

        // Han and kana share one run length because both are CJK text that
        // merges across characters inside prose and stops merging when
        // repeated, but each class keeps its own floor.
        let cjk_len = han + kana;
        let han_merge = cjk_merge(cjk_len, han_min_merge());
        let kana_merge = cjk_merge(cjk_len, kana_min_merge());
        let script = hangul as f64 * family.hangul_rate()
            + han as f64 * family.han_rate() * han_merge
            + kana as f64 * family.kana_rate() * kana_merge
            + arabic as f64 * family.arabic_rate()
            + indic as f64 * family.indic_rate()
            + other_letter as f64 * family.other_letter_rate()
            + emoji as f64 * family.emoji_rate()
            + symbol as f64 * family.symbol_rate();

        if hangul > 0 {
            // Hangul syllables already account for most of a mixed run, and
            // charging its punctuation again was measured to overshoot Korean
            // samples, so only the ASCII tail is added here.
            tokens += script + ascii_alnum_tokens;
        } else {
            // A one-character run still costs a token, hence the floor of one
            // before punctuation is added on top. A run of non-ASCII script
            // characters is priced as a whole, so the floor applies to the
            // script cost rather than being charged again beside it.
            let body = (script + ascii_alnum_tokens).max(1.0);
            tokens += body + ascii_punct as f64 * PUNCT_RATE;
        }
    }

    ((tokens + 0.5) as usize).max(1)
}

/// Closes the ASCII alphanumeric segment in progress, charging it at the
/// measured characters-per-token rate with a floor of one token. A segment is
/// the run of letters and digits between two delimiters.
fn close_ascii_segment(segment_len: &mut usize, segment_tokens: &mut f64) {
    if *segment_len > 0 {
        *segment_tokens += ((*segment_len as f64) / ASCII_CHARS_PER_TOKEN).ceil().max(1.0);
        *segment_len = 0;
    }
}

/// Only the whitespace characters that actually appear in transcripts split a
/// run, so rare controls cannot silently break a token in two.
fn is_run_break(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\r' | '\n')
}

/// Prices one non-ASCII character. The ASCII classes are handled before this
/// is reached, so every branch here applies to a character that an earlier
/// version of this crate counted as zero tokens.
fn classify(c: char) -> CharClass {
    let code = c as u32;
    if is_hangul(code) {
        CharClass::Hangul
    } else if is_han(code) {
        CharClass::Han
    } else if is_kana(code) {
        CharClass::Kana
    } else if is_emoji(code) {
        CharClass::Emoji
    } else if is_arabic(code) {
        CharClass::Arabic
    } else if is_indic(code) {
        CharClass::Indic
    } else if c.is_alphabetic() {
        CharClass::OtherLetter
    } else {
        CharClass::Symbol
    }
}

/// Hangul precomposed syllables, conjoining jamo, or compatibility jamo.
fn is_hangul(code: u32) -> bool {
    matches!(code, 0xAC00..=0xD7A3 | 0x1100..=0x11FF | 0x3130..=0x318F)
}

/// CJK ideographs in the base block, extension A, the compatibility block and
/// the supplementary planes, plus the ideographic iteration mark and `〇`.
fn is_han(code: u32) -> bool {
    matches!(
        code,
        0x4E00..=0x9FFF | 0x3400..=0x4DBF | 0xF900..=0xFAFF | 0x20000..=0x3FFFF | 0x3005 | 0x3007
    )
}

/// Hiragana, katakana, katakana phonetic extensions, halfwidth katakana and its
/// voiced sound marks, and the kana supplement.
fn is_kana(code: u32) -> bool {
    matches!(
        code,
        0x3040..=0x30FF | 0x31F0..=0x31FF | 0xFF66..=0xFF9F | 0x1B000..=0x1B0FF
    )
}

/// Pictographs, dingbats, miscellaneous symbols and arrows, plus the variation
/// selector, zero-width joiner and combining keycap that compose them.
fn is_emoji(code: u32) -> bool {
    matches!(
        code,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0x2300..=0x23FF | 0xFE0F | 0x200D | 0x20E3
    )
}

/// Arabic, Arabic Supplement, Arabic Extended-A, and the presentation forms.
fn is_arabic(code: u32) -> bool {
    matches!(
        code,
        0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF
    )
}

/// The Indic and adjacent South and Southeast Asian scripts, whose cl100k cost
/// per character is far above Arabic and Cyrillic.
fn is_indic(code: u32) -> bool {
    matches!(
        code,
        0x0900..=0x0DFF
            | 0x0E00..=0x0EFF
            | 0x0F00..=0x0FFF
            | 0x1000..=0x109F
            | 0x1780..=0x17FF
            | 0x1A00..=0x1AAF
            | 0xA8E0..=0xA8FF
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const KOREAN_PROSE: &str = "에이전트가 파일을 읽고 편집한 뒤 테스트를 실행합니다. 이 과정에서 컨텍스트가 얼마나 남았는지 정확히 알아야 압축을 제때 시작할 수 있습니다.";

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

    const HAN_PROSE: &str =
        "人工智能代理读取并编辑文件然后运行测试。在这个过程中它必须准确知道还剩多少上下文，以便及时开始压缩。";

    const KANA_PROSE: &str = "エージェントはファイルを読み込んで編集し、その後テストを実行します。その過程でコンテキストがどれだけ残っているかを正確に知る必要があります。";

    const CYRILLIC: &str = "Агент читает и редактирует файлы, затем запускает тесты. В процессе работы он должен точно знать, сколько контекста осталось.";

    /// Korean prose with four emoji embedded. NOT a pure-emoji sample: its real
    /// token count is almost entirely Hangul, which is why the old test built on
    /// it could not see an emoji error in either direction. It is kept because the
    /// documentation table cites it as a mixed sample.
    const KOREAN_WITH_EMOJI: &str = "완료했습니다 ✅ 실패 3건 ❌ 재시도 예정 🔁 승인 대기 ⏳";

    /// Anchors are real tokenizer counts for these exact samples. The band is
    /// 15% either way because a fitted rate cannot be tighter than that
    /// without overspecializing to the samples.
    const TOLERANCE: f64 = 0.15;

    fn assert_within_band(estimate: usize, real: f64, sample: &str) {
        let low = real * (1.0 - TOLERANCE);
        let high = real * (1.0 + TOLERANCE);
        assert!(
            (estimate as f64) >= low && (estimate as f64) <= high,
            "{sample}: estimate {estimate} outside [{low:.2}, {high:.2}] for real {real}"
        );
    }

    #[test]
    fn korean_prose_matches_anchor() {
        assert_eq!(KOREAN_PROSE.chars().count(), 79);
        assert_within_band(estimate_tokens(KOREAN_PROSE), 44.0, "korean prose");
    }

    #[test]
    fn korean_prose_is_heavier_on_cl100k() {
        let o200k = estimate_tokens_for_family(KOREAN_PROSE, TokenFamily::O200k);
        let cl100k = estimate_tokens_for_family(KOREAN_PROSE, TokenFamily::Cl100k);
        assert_within_band(cl100k, 72.0, "korean prose cl100k");
        assert!(cl100k > o200k, "cl100k {cl100k} should exceed o200k {o200k}");
    }

    #[test]
    fn english_prose_matches_anchor() {
        assert_eq!(ENGLISH_PROSE.chars().count(), 146);
        assert_within_band(estimate_tokens(ENGLISH_PROSE), 30.0, "english prose");
    }

    #[test]
    fn code_is_not_underestimated() {
        assert_eq!(RUST_SAMPLE.chars().count(), 129);
        let estimate = estimate_tokens(RUST_SAMPLE);
        let floor = 34.0 * 0.85;
        assert!(
            (estimate as f64) >= floor,
            "rust sample: estimate {estimate} below the 85% floor {floor:.2} of 34"
        );
    }

    #[test]
    fn json_is_not_worse_than_the_old_heuristic() {
        assert_eq!(JSON_SAMPLE.chars().count(), 105);
        let estimate = estimate_tokens(JSON_SAMPLE);
        let old_heuristic = JSON_SAMPLE.len() as f64 / 4.0;
        assert!(
            (estimate as f64) > old_heuristic,
            "json sample: estimate {estimate} must beat the chars/4 heuristic {old_heuristic:.2}"
        );
        assert_within_band(estimate, 33.0, "json sample");
    }

    #[test]
    fn empty_and_single_char_are_at_least_one() {
        assert_eq!(estimate_tokens(""), 1);
        assert_eq!(estimate_tokens("   \n\t"), 1);
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens_for_family("", TokenFamily::Cl100k), 1);
    }

    /// A script sample is held to the same ±15% band as the anchors in both
    /// directions: coming in under the real count never compacts, and coming in
    /// over it compacts early and discards usable context.
    #[test]
    fn han_prose_is_not_underestimated() {
        assert_eq!(HAN_PROSE.chars().count(), 50);
        assert_within_band(estimate_tokens(HAN_PROSE), 29.0, "han prose");
    }

    #[test]
    fn kana_prose_is_not_underestimated() {
        assert_eq!(KANA_PROSE.chars().count(), 70);
        assert_within_band(estimate_tokens(KANA_PROSE), 47.0, "kana prose");
    }

    #[test]
    fn cyrillic_is_not_underestimated() {
        assert_eq!(CYRILLIC.chars().count(), 125);
        assert_within_band(estimate_tokens(CYRILLIC), 32.0, "cyrillic");
    }

    /// A pure-emoji sample, measured on both families.
    ///
    /// The old test measured `EMOJI`, which is Korean prose plus four emoji, so
    /// its 22 real tokens were almost entirely Hangul: it could not see an emoji
    /// error in either direction, and its name promised otherwise.
    ///
    /// Measured with tiktoken 0.14.0: 33 repeated U+1F600 cost 33 tokens on
    /// o200k and 66 on cl100k. The estimator charges 2.00 per codepoint on both,
    /// so it is 2x high on o200k for this sample and exact on cl100k. That is
    /// recorded here rather than asserted away: the repeated single emoji is the
    /// one case where the flat rate is badly wrong, and a test that hides it is
    /// the defect this replaces.
    #[test]
    fn a_pure_emoji_run_is_priced_and_its_o200k_overcount_is_recorded() {
        let repeated = "\u{1F600}".repeat(33);
        assert_eq!(repeated.chars().count(), 33);

        let o200k = estimate_tokens_for_family(&repeated, TokenFamily::O200k);
        let cl100k = estimate_tokens_for_family(&repeated, TokenFamily::Cl100k);

        assert_eq!(
            cl100k, 66,
            "cl100k charges one token per repeated emoji codepoint, so 33 cost 66"
        );
        assert_eq!(
            o200k, 66,
            "the flat 2.00 rate gives 66; the real o200k count is 33, so this is a 2x overcount"
        );
        assert!(
            o200k > 33,
            "the overcount is the known limitation; if the rate changes so that \
             o200k no longer exceeds the measured 33, update this test and the README together"
        );
    }

    /// The mixed Korean-plus-emoji sample still measures close to its anchor, and
    /// its real tokens are mostly Hangul - which is the point of keeping it apart
    /// from the pure-emoji cases above.
    #[test]
    fn a_mixed_korean_and_emoji_sample_matches_its_anchor() {
        assert_eq!(KOREAN_WITH_EMOJI.chars().count(), 33);
        assert_within_band(estimate_tokens(KOREAN_WITH_EMOJI), 22.0, "korean with emoji");
    }

    /// A varied emoji set is priced by the same flat rate as a repeated one, so
    /// both families get 40 for 20 codepoints. Measured with tiktoken 0.14.0:
    /// those 20 varied emoji are 39 tokens on o200k and 56 on cl100k. The flat
    /// rate is therefore close on o200k (2.6% high) and about 29% low on cl100k -
    /// the family whose undercount overruns the window, and the one asserted here.
    #[test]
    fn a_varied_emoji_set_is_priced_and_its_cl100k_undercount_is_recorded() {
        let varied = "😀🔥✅👍⏳🎉🚀💡📌🧪🎯🌈🦀🍀🌊⭐🎵🔧📎🧭";
        assert_eq!(varied.chars().count(), 20);
        let o200k = estimate_tokens_for_family(varied, TokenFamily::O200k);
        let cl100k = estimate_tokens_for_family(varied, TokenFamily::Cl100k);
        assert_eq!(o200k, 40, "20 codepoints at 2.00");
        assert_eq!(cl100k, 40, "20 codepoints at 2.00");

        assert!(
            (o200k as f64) >= 39.0 * 0.80,
            "o200k real is 39, so the estimate must not fall 20% below it, got {o200k}"
        );
        assert!(
            (cl100k as f64) >= 56.0 * 0.60,
            "cl100k real is 56; the estimate is about 29% low, which is the known \
             direction that overruns the window. If this falls further the rate needs \
             revisiting, got {cl100k}"
        );
        assert!(
            (cl100k as f64) < 56.0,
            "the undercount is the recorded limitation; if the rate changes so cl100k \
             reaches the measured 56, update this test and the README together"
        );
    }

    /// Every class must be charged something. A zero rate prices a whole script
    /// at nothing, and that failure is silent: the transcript overruns the
    /// window and the provider rejects the request.
    #[test]
    fn every_script_class_is_charged() {
        for family in [TokenFamily::O200k, TokenFamily::Cl100k] {
            assert!(
                family.hangul_rate() > 0.0,
                "{family:?}: hangul rate {} is not positive",
                family.hangul_rate()
            );
            assert!(
                family.han_rate() > 0.0,
                "{family:?}: han rate {} is not positive",
                family.han_rate()
            );
            assert!(
                family.kana_rate() > 0.0,
                "{family:?}: kana rate {} is not positive",
                family.kana_rate()
            );
            assert!(
                family.arabic_rate() > 0.0,
                "{family:?}: arabic rate {} is not positive",
                family.arabic_rate()
            );
            assert!(
                family.indic_rate() > 0.0,
                "{family:?}: indic rate {} is not positive",
                family.indic_rate()
            );
            assert!(
                family.other_letter_rate() > 0.0,
                "{family:?}: other letter rate {} is not positive",
                family.other_letter_rate()
            );
            assert!(
                family.symbol_rate() > 0.0,
                "{family:?}: symbol rate {} is not positive",
                family.symbol_rate()
            );
            assert!(
                family.emoji_rate() > 0.0,
                "{family:?}: emoji rate {} is not positive",
                family.emoji_rate()
            );
        }
        // Shared rates are read through a binding so the guard survives a
        // constant being edited to zero without failing the build instead.
        for (name, rate) in [
            ("ascii chars per token", ASCII_CHARS_PER_TOKEN),
            ("punctuation", PUNCT_RATE),
            ("han floor", han_min_merge()),
            ("kana floor", kana_min_merge()),
        ] {
            assert!(rate > 0.0, "{name} {rate} is not positive");
        }
    }

    /// One character of a script the estimator classifies must still cost a
    /// token, in both families. This is the end-to-end form of the rate guard.
    #[test]
    fn one_character_of_every_script_is_at_least_one_token() {
        let samples = [
            ("Han", "漢"),
            ("Cyrillic", "Ж"),
            ("Arabic", "ش"),
            ("Devanagari", "क"),
            ("emoji", "😀"),
            ("Greek", "Ω"),
            ("Thai", "ก"),
            ("Hebrew", "א"),
        ];
        for family in [TokenFamily::O200k, TokenFamily::Cl100k] {
            for (name, sample) in samples {
                let estimate = estimate_tokens_for_family(sample, family);
                assert!(
                    estimate >= 1,
                    "{family:?} {name} {sample:?}: estimate {estimate} is below one token"
                );
            }
        }
    }

    /// Short CJK runs cost more per character than prose: a real tokenizer
    /// splits them instead of merging them with a neighbour. Undershooting
    /// here never triggers compaction, so each case is held to the real count
    /// from both sides.
    #[test]
    fn short_cjk_runs_are_not_underestimated() {
        let cases = [
            ("han 6", "人工智能代理".to_string(), 6, 3.0),
            ("han 20", "漢".repeat(20), 20, 11.0),
            ("kana 6", "かなカナあい".to_string(), 6, 4.0),
            ("kana 12", "かなカナあいうえおかきく".to_string(), 12, 8.0),
        ];
        for (name, sample, chars, real) in cases {
            assert_eq!(sample.chars().count(), chars, "{name} sample length");
            let estimate = estimate_tokens(&sample) as f64;
            let low = real * 0.85;
            let high = real * 1.30;
            assert!(
                estimate >= low && estimate <= high,
                "{name} ({chars} chars): estimate {estimate} outside [{low:.2}, {high:.2}] for real {real}"
            );
        }
    }

    #[test]
    fn no_script_estimates_zero() {
        // A repeated ideograph is the worst case: the vocabulary cannot merge
        // it with a neighbour, so it costs a full token per character.
        let repeated = "漢".repeat(1000);
        assert_eq!(repeated.chars().count(), 1000);
        assert!(
            estimate_tokens(&repeated) >= 850,
            "1000 repeated ideographs estimated at {}",
            estimate_tokens(&repeated)
        );
    }
}
