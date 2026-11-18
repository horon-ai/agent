//! Regressions for the stored instant: text, range, arithmetic and serde.

use std::time::{Duration as StdDuration, SystemTime};

use crate::macros::format_description;
use crate::{Date, Duration, Month, OffsetDateTime, Precision, PrimitiveDateTime, Time, Timestamp, UtcOffset, Weekday};

/// The stored range: chrono 0.4.42 `NaiveDate::MIN`/`MAX` as whole seconds.
///
/// `-262_143-01-01T00:00:00Z` through `262_142-12-31T23:59:59Z`.
fn stored_bounds() -> (i64, i64) {
    let first = Date::from_calendar_date(-262_143, Month::January, 1)
        .expect("the first stored date")
        .midnight()
        .assume_utc()
        .unix_timestamp();
    let last = Date::from_calendar_date(262_142, Month::December, 31)
        .expect("the last stored date")
        .midnight()
        .assume_utc()
        .unix_timestamp()
        + 86_399;
    (first, last)
}

#[test]
fn the_epoch_is_the_default_and_zero() {
    assert_eq!(Timestamp::default(), Timestamp::UNIX_EPOCH);
    assert_eq!(Timestamp::UNIX_EPOCH.timestamp(), 0);
    assert_eq!(Timestamp::UNIX_EPOCH.timestamp_subsec_nanos(), 0);
    assert_eq!(Timestamp::UNIX_EPOCH.to_rfc3339(), "1970-01-01T00:00:00+00:00");
    assert_eq!(Timestamp::UNIX_EPOCH.to_string(), "1970-01-01T00:00:00Z");
}

#[test]
fn auto_precision_writes_zero_three_six_or_nine_digits() {
    let cases = [
        (0_u32, "1970-01-01T00:00:00Z"),
        (4_000_000, "1970-01-01T00:00:00.004Z"),
        (123_456_000, "1970-01-01T00:00:00.123456Z"),
        (123_456_789, "1970-01-01T00:00:00.123456789Z"),
    ];
    for (nanos, expected) in cases {
        let at = Timestamp::from_timestamp(0, nanos).expect("a representable instant");
        assert_eq!(at.to_rfc3339_opts(Precision::Auto, true), expected);
        assert_eq!(at.to_string(), expected);
    }
}

#[test]
fn fixed_precision_truncates_and_pads() {
    let at = Timestamp::from_timestamp(1, 123_456_789).expect("a representable instant");
    assert_eq!(at.to_rfc3339_opts(Precision::Seconds, true), "1970-01-01T00:00:01Z");
    assert_eq!(at.to_rfc3339_opts(Precision::Millis, true), "1970-01-01T00:00:01.123Z");
    assert_eq!(
        at.to_rfc3339_opts(Precision::Nanos, true),
        "1970-01-01T00:00:01.123456789Z"
    );

    let whole = Timestamp::from_timestamp(1, 0).expect("a representable instant");
    assert_eq!(
        whole.to_rfc3339_opts(Precision::Millis, true),
        "1970-01-01T00:00:01.000Z"
    );
    assert_eq!(
        whole.to_rfc3339_opts(Precision::Nanos, true),
        "1970-01-01T00:00:01.000000000Z"
    );
}

#[test]
fn the_utc_spelling_follows_the_caller() {
    let at = Timestamp::from_timestamp(0, 0).expect("a representable instant");
    assert_eq!(at.to_rfc3339(), "1970-01-01T00:00:00+00:00");
    assert_eq!(
        at.to_rfc3339_opts(Precision::Seconds, false),
        "1970-01-01T00:00:00+00:00"
    );
    assert_eq!(at.to_rfc3339_opts(Precision::Seconds, true), "1970-01-01T00:00:00Z");
}

#[test]
fn negative_milliseconds_floor_toward_the_earlier_second() {
    let at = Timestamp::from_timestamp_millis(-1_500).expect("a representable instant");
    assert_eq!(at.timestamp(), -2);
    assert_eq!(at.timestamp_subsec_nanos(), 500_000_000);
    assert_eq!(at.timestamp_millis(), -1_500);
    assert_eq!(at.to_rfc3339_opts(Precision::Millis, true), "1969-12-31T23:59:58.500Z");

    let just_before = Timestamp::from_timestamp(-1, 500_000_000).expect("a representable instant");
    assert_eq!(
        just_before.to_rfc3339_opts(Precision::Millis, true),
        "1969-12-31T23:59:59.500Z"
    );
    assert_eq!(
        Timestamp::from_timestamp_millis(1_500),
        Timestamp::from_timestamp(1, 500_000_000)
    );
}

#[test]
fn a_leap_second_keeps_second_sixty_through_text() {
    let at = "2016-12-31T23:59:60.5Z"
        .parse::<Timestamp>()
        .expect("a leap second parses");
    assert_eq!(at.timestamp(), 1_483_228_799);
    assert_eq!(at.timestamp_subsec_nanos(), 1_500_000_000);
    assert_eq!(at.timestamp_millis(), 1_483_228_800_500);
    assert_eq!(at.to_rfc3339(), "2016-12-31T23:59:60.500+00:00");
    assert_eq!(at.to_rfc3339_opts(Precision::Seconds, true), "2016-12-31T23:59:60Z");
    assert_eq!(
        at.to_rfc3339_opts(Precision::Nanos, true),
        "2016-12-31T23:59:60.500000000Z"
    );

    // `time::OffsetDateTime` cannot hold the 60th second, so the instant it
    // reports is the following second.
    assert_eq!(at.utc().unix_timestamp(), 1_483_228_800);
    assert_eq!(at.utc().nanosecond(), 500_000_000);
}

#[test]
fn a_whole_leap_second_has_no_fraction() {
    let at = "2016-12-31T23:59:60Z"
        .parse::<Timestamp>()
        .expect("a leap second parses");
    assert_eq!(at.timestamp_subsec_nanos(), 1_000_000_000);
    assert_eq!(at.to_rfc3339_opts(Precision::Auto, true), "2016-12-31T23:59:60Z");
}

#[test]
fn numeric_offsets_convert_to_utc() {
    let utc = "2024-03-05T03:00:00Z".parse::<Timestamp>().expect("a valid timestamp");
    let eastern = "2024-03-05T12:00:00+09:00"
        .parse::<Timestamp>()
        .expect("a valid timestamp");
    assert_eq!(eastern, utc);
    let western = "2024-03-05T12:00:00-05:30"
        .parse::<Timestamp>()
        .expect("a valid timestamp");
    assert_eq!(
        western.to_rfc3339_opts(Precision::Seconds, true),
        "2024-03-05T17:30:00Z"
    );
    let zero = "2024-03-05T03:00:00+00:00"
        .parse::<Timestamp>()
        .expect("a valid timestamp");
    assert_eq!(zero, utc);
}

#[test]
fn lowercase_designators_and_a_space_separator_parse() {
    let spaced = "2024-03-05 03:00:00z".parse::<Timestamp>().expect("a valid timestamp");
    assert_eq!(spaced.to_rfc3339_opts(Precision::Seconds, true), "2024-03-05T03:00:00Z");
    let lowercase = "2024-03-05t03:00:00Z".parse::<Timestamp>().expect("a valid timestamp");
    assert_eq!(lowercase, spaced);
}

#[test]
fn digits_past_the_ninth_are_dropped() {
    let at = "1970-01-01T00:00:00.1234567891234Z"
        .parse::<Timestamp>()
        .expect("a valid timestamp");
    assert_eq!(at.timestamp_subsec_nanos(), 123_456_789);
    let short = "1970-01-01T00:00:00.5Z"
        .parse::<Timestamp>()
        .expect("a valid timestamp");
    assert_eq!(short.timestamp_subsec_nanos(), 500_000_000);
    assert_eq!(short.to_string(), "1970-01-01T00:00:00.500Z");
}

#[test]
fn text_outside_both_grammars_is_rejected() {
    let rejected = [
        "",
        "1970-01-01T00:00:00",
        "2019-02-29T00:00:00Z",
        "2024-13-01T00:00:00Z",
        "2024-01-32T00:00:00Z",
        "2024-01-01T24:00:00Z",
        "2024-01-01T00:60:00Z",
        "2024-01-01T00:00:61Z",
        "2024-01-01T00:00:00+24:00Z",
        "2024-01-01T00:00:00+09:60",
        "2024-01-01T00:00:00.",
        "2024-01-01T00:00:00Z trailing",
        "+1234567-06-07T08:09:10Z",
        "2024-01-01X00:00:00Z",
    ];
    for text in rejected {
        assert!(text.parse::<Timestamp>().is_err(), "{text} must be rejected");
        assert!(
            Timestamp::parse_rfc3339(text).is_err(),
            "{text} must be rejected strictly"
        );
    }
}

#[test]
fn the_relaxed_reader_keeps_the_old_leniency() {
    let canonical = "2024-03-05T03:00:00Z".parse::<Timestamp>().expect("a valid instant");
    let forms = [
        "2024-3-5T3:0:0Z",
        "2024-03-05 03:00:00Z",
        "2024-03-05T03:00:00+0000",
        "2024-03-05T03:00:00+00:00",
        "2024-03-05T03:00:00 UTC",
        "2024-03-05T03:00:00utc",
        " 2024-03-05T03:00:00Z",
        "2024-03-05T03:00:00Z ",
        "2024- 3 - 5T3 : 0 : 0Z",
        "2024-03-05T03:00:00+00 00",
        "2024-03-05t03:00:00z",
    ];
    for text in forms {
        let parsed = text.parse::<Timestamp>().expect("the relaxed grammar reads it");
        assert_eq!(parsed, canonical, "{text}");
    }
}

#[test]
fn the_strict_reader_rejects_what_only_the_relaxed_grammar_allows() {
    let relaxed_only = [
        "2024-3-5T3:0:0Z",
        "2024-03-05T03:00:00+0900",
        "2024-03-05T03:00:00 UTC",
        "+12345-06-07T08:09:10Z",
        " 2024-03-05T03:00:00Z",
        "2024-03-05T03:00:00Z ",
        "2024-03-05T03:00:00+09 00",
    ];
    for text in relaxed_only {
        assert!(text.parse::<Timestamp>().is_ok(), "{text} must stay readable");
        assert!(
            Timestamp::parse_rfc3339(text).is_err(),
            "{text} must stay strict-rejected"
        );
    }
}

#[test]
fn the_strict_reader_reads_the_canonical_form() {
    let canonical = "2024-03-05T03:00:00Z".parse::<Timestamp>().expect("a valid instant");
    for text in [
        "2024-03-05T03:00:00Z",
        "2024-03-05T03:00:00+00:00",
        "2024-03-05T03:00:00−00:00",
        "2024-03-05 03:00:00z",
        "2024-03-05t03:00:00Z",
    ] {
        let parsed = Timestamp::parse_rfc3339(text).expect("the strict grammar reads it");
        assert_eq!(parsed, canonical, "{text}");
    }
    let leap = Timestamp::parse_rfc3339("2016-12-31T23:59:60.5Z").expect("a leap second parses");
    assert_eq!(leap.timestamp_subsec_nanos(), 1_500_000_000);
}

#[test]
fn the_stored_range_is_the_old_datetime_range() {
    let (first, last) = stored_bounds();

    assert!(Timestamp::from_timestamp(first, 0).is_some());
    assert!(Timestamp::from_timestamp(first - 1, 0).is_none());
    assert!(Timestamp::from_timestamp(last, 0).is_some());
    assert!(Timestamp::from_timestamp(last + 1, 0).is_none());
    assert!(Timestamp::from_timestamp(0, 2_000_000_000).is_none());
    assert!(Timestamp::from_timestamp(0, 1_999_999_999).is_none());
    assert!(Timestamp::from_timestamp(59, 1_999_999_999).is_some());
    assert!(Timestamp::from_timestamp_millis(i64::MAX).is_none());

    // The old type accepted a leaping field only on a `:59` second, and the
    // stored range starts at midnight and ends on 23:59:59.
    assert!(Timestamp::from_timestamp(last, 1_500_000_000).is_some());
    assert!(Timestamp::from_timestamp(first, 1_500_000_000).is_none());
    assert!(Timestamp::from_timestamp(first + 59, 1_500_000_000).is_some());
}

#[test]
fn negative_seconds_keep_the_old_pair_and_floor_millis() {
    let at = Timestamp::from_timestamp(-1, 500_000_000).expect("a representable instant");
    assert_eq!(at.timestamp(), -1);
    assert_eq!(at.timestamp_subsec_nanos(), 500_000_000);
    assert_eq!(at.timestamp_millis(), -500);
    assert_eq!(at.to_rfc3339_opts(Precision::Millis, true), "1969-12-31T23:59:59.500Z");
    assert_eq!(Timestamp::from_timestamp_millis(-500), Some(at));
    assert_eq!(
        Timestamp::from_timestamp_millis(-1_001),
        Timestamp::from_timestamp(-2, 999_000_000)
    );
}

#[test]
fn a_leap_second_keeps_its_spelling_while_the_movement_stays_inside_it() {
    let leap = "2016-12-31T23:59:60.5Z"
        .parse::<Timestamp>()
        .expect("a leap second parses");
    let millis = |at: Timestamp| at.to_rfc3339_opts(Precision::Millis, true);

    assert_eq!(millis(leap + Duration::milliseconds(250)), "2016-12-31T23:59:60.750Z");
    assert_eq!(millis(leap - Duration::milliseconds(250)), "2016-12-31T23:59:60.250Z");
    assert_eq!(millis(leap + Duration::milliseconds(0)), "2016-12-31T23:59:60.500Z");

    // Leaving the leaping second normalizes to the following second.
    assert_eq!(millis(leap + Duration::milliseconds(750)), "2017-01-01T00:00:00.250Z");
    assert_eq!(millis(leap + Duration::milliseconds(500)), "2017-01-01T00:00:00.000Z");
    assert_eq!(millis(leap - Duration::milliseconds(1_500)), "2016-12-31T23:59:59.000Z");
    assert_eq!(millis(leap + Duration::seconds(1)), "2017-01-01T00:00:00.500Z");
}

#[test]
fn a_leap_second_counts_in_a_difference_the_way_the_old_type_counted_it() {
    // The old subtraction added the leap second when the two values differ in
    // their second of the day. 40_019 is a `:59` second, the only second a
    // leaping field may sit on.
    let same_second = Timestamp::from_timestamp(40_019, 500_000_000).expect("a representable instant");
    let leap = Timestamp::from_timestamp(40_019, 1_500_000_000).expect("a representable instant");
    let next = Timestamp::from_timestamp(40_020, 0).expect("a representable instant");
    assert_eq!(leap - same_second, Duration::seconds(1));
    assert_eq!(next - leap, Duration::milliseconds(500));
    assert_eq!(leap - next, Duration::milliseconds(-500));

    // Across a day boundary the old rule did not see the leap second; the same
    // answer is kept rather than "fixed", so a stored difference does not move.
    let midnight_leap = "2016-12-31T23:59:60.5Z"
        .parse::<Timestamp>()
        .expect("a leap second parses");
    let midnight_after = "2017-01-01T00:00:00Z".parse::<Timestamp>().expect("a valid instant");
    assert_eq!(midnight_after - midnight_leap, Duration::milliseconds(-500));
}

#[test]
fn durations_move_the_instant_and_normalize_the_fraction() {
    let base = Timestamp::from_timestamp(1_000, 500_000_000).expect("a representable instant");
    let plus_one = Timestamp::from_timestamp(1_001, 500_000_000).expect("a representable instant");
    let plus_fraction = Timestamp::from_timestamp(1_001, 250_000_000).expect("a representable instant");
    let minus_fraction = Timestamp::from_timestamp(999, 750_000_000).expect("a representable instant");
    let minus_one = Timestamp::from_timestamp(999, 500_000_000).expect("a representable instant");

    assert_eq!(base + Duration::seconds(1), plus_one);
    assert_eq!(base + Duration::milliseconds(750), plus_fraction);
    assert_eq!(base - Duration::milliseconds(750), minus_fraction);
    assert_eq!(base - Duration::seconds(1), minus_one);
    assert_eq!(base + Duration::seconds(0), base);
}

#[test]
fn arithmetic_at_the_stored_boundaries_matches_the_old_result() {
    let (first, last) = stored_bounds();
    let first_at = Timestamp::from_timestamp(first, 0).expect("the first second");
    let last_at = Timestamp::from_timestamp(last, 0).expect("the last second");

    // A step in and back out stays on the boundary, as it did on the old type.
    assert_eq!(last_at - Duration::seconds(1) + Duration::seconds(1), last_at);
    assert_eq!(first_at + Duration::seconds(1) - Duration::seconds(1), first_at);
    assert_eq!(
        last_at - Duration::milliseconds(1),
        Timestamp::from_timestamp(last - 1, 999_000_000).expect("inside the range")
    );
    assert_eq!(
        first_at + Duration::milliseconds(1),
        Timestamp::from_timestamp(first, 1_000_000).expect("inside the range")
    );
}

#[test]
#[should_panic(expected = "stored range")]
fn adding_past_the_last_second_fails_like_the_old_type() {
    let (_, last) = stored_bounds();
    let last_at = Timestamp::from_timestamp(last, 0).expect("the last second");
    let _ = last_at + Duration::seconds(1);
}

#[test]
#[should_panic(expected = "stored range")]
fn subtracting_before_the_first_second_fails_like_the_old_type() {
    let (first, _) = stored_bounds();
    let first_at = Timestamp::from_timestamp(first, 0).expect("the first second");
    let _ = first_at - Duration::seconds(1);
}

#[test]
#[should_panic(expected = "stored range")]
fn a_system_time_outside_the_stored_range_fails_like_the_old_type() {
    // One second past the last storable instant (year 262143).
    let beyond = SystemTime::UNIX_EPOCH + StdDuration::from_secs(9_000_000_000_000);
    let _ = Timestamp::from(beyond);
}

#[test]
fn the_difference_is_a_signed_duration() {
    let earlier = Timestamp::from_timestamp(1_000, 500_000_000).expect("a representable instant");
    let later = Timestamp::from_timestamp(1_002, 0).expect("a representable instant");

    let forward = later - earlier;
    assert_eq!(forward.whole_seconds(), 1);
    assert_eq!(forward.subsec_nanoseconds(), 500_000_000);
    assert_eq!(forward.whole_days(), 0);

    let backward = earlier - later;
    assert_eq!(backward.whole_seconds(), -1);
    assert_eq!(backward.subsec_nanoseconds(), -500_000_000);
}

#[test]
fn system_time_round_trips_including_before_the_epoch() {
    let after = SystemTime::UNIX_EPOCH + StdDuration::new(1, 500_000_000);
    assert_eq!(
        Timestamp::from(after),
        Timestamp::from_timestamp(1, 500_000_000).expect("a representable instant")
    );

    let before = SystemTime::UNIX_EPOCH - StdDuration::new(1, 500_000_000);
    let at = Timestamp::from(before);
    assert_eq!(at.timestamp(), -2);
    assert_eq!(at.timestamp_subsec_nanos(), 500_000_000);
    assert_eq!(at.to_rfc3339_opts(Precision::Millis, true), "1969-12-31T23:59:58.500Z");
}

#[test]
fn now_agrees_with_the_system_clock() {
    let now = Timestamp::now();
    let system = Timestamp::from(SystemTime::now());
    let difference = if now > system { now - system } else { system - now };
    assert!(difference.whole_seconds() < 5);
}

#[test]
fn serde_writes_the_stored_text_and_reads_offsets() {
    let at = Timestamp::from_timestamp(1, 200_000_000).expect("a representable instant");
    assert_eq!(
        serde_json::to_string(&at).expect("serializes"),
        "\"1970-01-01T00:00:01.200Z\""
    );

    let parsed: Timestamp = serde_json::from_str("\"2024-03-05T12:00:00+09:00\"").expect("accepts an offset");
    assert_eq!(parsed.to_rfc3339_opts(Precision::Seconds, true), "2024-03-05T03:00:00Z");
    let relaxed: Timestamp = serde_json::from_str("\"2024-3-5 12:00:00+0900\"").expect("the stored grammar accepts it");
    assert_eq!(relaxed, parsed);
    assert!(serde_json::from_str::<Timestamp>("\"2024-03-05T12:00:00\"").is_err());
    assert!(serde_json::from_str::<Timestamp>("1709596800").is_err());
}

#[test]
fn serde_round_trips_the_leap_second() {
    let at = "2016-12-31T23:59:60.5Z"
        .parse::<Timestamp>()
        .expect("a valid timestamp");
    let text = serde_json::to_string(&at).expect("serializes");
    assert_eq!(text, "\"2016-12-31T23:59:60.500Z\"");
    let back: Timestamp = serde_json::from_str(&text).expect("deserializes");
    assert_eq!(back, at);
}

#[test]
fn years_outside_four_digits_keep_their_mandatory_sign() {
    let extended = PrimitiveDateTime::new(
        Date::from_calendar_date(12345, Month::June, 7).expect("a valid date"),
        Time::from_hms(8, 9, 10).expect("a valid time"),
    )
    .assume_utc();
    let at = Timestamp::from(extended);
    assert_eq!(at.to_rfc3339(), "+12345-06-07T08:09:10+00:00");
    assert_eq!(
        at.to_rfc3339_opts(Precision::Millis, true),
        "+12345-06-07T08:09:10.000Z"
    );
    assert_eq!(
        serde_json::to_string(&at).expect("serializes"),
        "\"+12345-06-07T08:09:10Z\""
    );

    let negative = PrimitiveDateTime::new(
        Date::from_calendar_date(-44, Month::March, 15).expect("a valid date"),
        Time::MIDNIGHT,
    )
    .assume_utc();
    let negative = Timestamp::from(negative);
    assert_eq!(negative.to_rfc3339(), "-0044-03-15T00:00:00+00:00");

    // What the writer emits, the parser reads back: the stored hash covers text
    // this boundary produced, so a signed year cannot be write-only.
    for value in [at, negative] {
        for precision in [Precision::Auto, Precision::Millis, Precision::Nanos] {
            let text = value.to_rfc3339_opts(precision, true);
            assert_eq!(text.parse::<Timestamp>().expect("the writer's text parses"), value);
            let text = value.to_rfc3339_opts(precision, false);
            assert_eq!(text.parse::<Timestamp>().expect("the writer's text parses"), value);
        }
    }
}

#[test]
fn an_offset_date_time_converts_with_its_fraction() {
    let at = OffsetDateTime::from_unix_timestamp(1_700_000_000)
        .expect("a representable instant")
        .replace_nanosecond(999_999_999)
        .expect("a valid fraction");
    let timestamp = Timestamp::from(at);
    assert_eq!(timestamp.timestamp(), 1_700_000_000);
    assert_eq!(timestamp.timestamp_subsec_nanos(), 999_999_999);
    assert_eq!(timestamp.utc(), at);
}

#[test]
fn local_keeps_the_instant_and_uses_the_system_offset() {
    let at = Timestamp::from_timestamp(1_700_000_000, 500_000_000).expect("a representable instant");
    let utc = at.utc();
    let local = at.local();
    assert_eq!(local, utc);
    match UtcOffset::local_offset_at(utc) {
        Ok(offset) => assert_eq!(local.offset(), offset),
        Err(_) => assert_eq!(local.offset(), UtcOffset::UTC),
    }
}

#[test]
fn the_reexported_format_description_macro_formats_a_local_time() {
    let at = Timestamp::from_timestamp(0, 0)
        .expect("a representable instant")
        .local();
    let format = format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
    let text = at.format(&format).expect("the formatter accepts the value");
    assert_eq!(text.len(), 19);
    assert_eq!(&text[4..5], "-");
    assert_eq!(&text[10..11], " ");

    let date = Date::from_calendar_date(2024, Month::June, 7).expect("a valid date");
    assert_eq!(date.weekday(), Weekday::Friday);
    assert_eq!(UtcOffset::UTC.whole_seconds(), 0);
}

#[test]
fn the_date_reader_matches_the_old_strftime_grammar() {
    let canonical = Timestamp::parse_naive_date("2024-03-05").expect("the canonical form reads");
    assert_eq!(
        canonical.to_rfc3339_opts(Precision::Seconds, true),
        "2024-03-05T00:00:00Z"
    );

    // Unpadded fields, whitespace in front of a numeric item, and a signed year
    // are what the old `%Y-%m-%d` parser accepted.
    for text in ["2024-3-5", "  2024-03-05", "2024- 3- 5", "2024-3-05", "+2024-03-05"] {
        let parsed = Timestamp::parse_naive_date(text).expect("the old grammar reads it");
        assert_eq!(parsed, canonical, "{text}");
    }

    let negative = Timestamp::parse_naive_date("-0044-03-15").expect("a signed year reads");
    assert_eq!(negative.calendar_utc().date(), "-0044-03-15");
    let extended = Timestamp::parse_naive_date("+12345-06-07").expect("an extended year reads");
    assert_eq!(extended.calendar_utc().date(), "+12345-06-07");
}

#[test]
fn the_date_reader_rejects_what_the_old_grammar_rejected() {
    for text in [
        "",
        "2024-03-05 ",
        "2024-03-05T00:00:00Z",
        "2024-03",
        "2024-03-05-",
        "2024-00-05",
        "2024-13-05",
        "2024-02-30",
        "2024-03-0",
        "20240305",
        // An unsigned year is at most four digits; a fifth needs a sign.
        "02024-03-05",
        "+262143-01-01",
        "-262144-12-31",
    ] {
        assert!(
            Timestamp::parse_naive_date(text).is_err(),
            "{text} must be rejected by %Y-%m-%d"
        );
    }
}

#[test]
fn the_utc_datetime_reader_matches_the_old_strftime_grammar() {
    let canonical = Timestamp::parse_naive_utc("2024-03-05T03:04:05Z").expect("the canonical form reads");
    assert_eq!(
        canonical.to_rfc3339_opts(Precision::Seconds, true),
        "2024-03-05T03:04:05Z"
    );

    for text in [
        "2024-3-5T3:4:5Z",
        " 2024-03-05T03:04:05Z",
        "2024- 3- 5T03:04:05Z",
        "2024-03-05T 3: 4: 5Z",
        "+2024-03-05T03:04:05Z",
    ] {
        let parsed = Timestamp::parse_naive_utc(text).expect("the old grammar reads it");
        assert_eq!(parsed, canonical, "{text}");
    }

    // The fraction keeps at most nine digits and drops the rest.
    let millis = Timestamp::parse_naive_utc("2024-03-05T03:04:05.123Z").expect("a fraction reads");
    assert_eq!(millis.timestamp_subsec_nanos(), 123_000_000);
    let long = Timestamp::parse_naive_utc("2024-03-05T03:04:05.1234567891234Z").expect("more than nine digits read");
    assert_eq!(long.timestamp_subsec_nanos(), 123_456_789);
}

#[test]
fn the_utc_datetime_reader_keeps_the_sixtieth_second() {
    let leap = Timestamp::parse_naive_utc("2016-12-31T23:59:60.5Z").expect("a leap second reads");
    assert_eq!(leap.timestamp(), 1_483_228_799);
    assert_eq!(leap.timestamp_subsec_nanos(), 1_500_000_000);
    assert_eq!(
        leap.to_rfc3339_opts(Precision::Nanos, true),
        "2016-12-31T23:59:60.500000000Z"
    );
    let whole = Timestamp::parse_naive_utc("2016-12-31T23:59:60Z").expect("a whole leap second reads");
    assert_eq!(whole.timestamp_subsec_nanos(), 1_000_000_000);
}

#[test]
fn the_utc_datetime_reader_rejects_what_the_old_grammar_rejected() {
    for text in [
        "",
        "2024-03-05T03:04:05",
        "2024-03-05T03:04:05Z ",
        "2024-03-05t03:04:05Z",
        "2024-03-05T03:04:05z",
        "2024-03-05 03:04:05Z",
        "2024-03-05T03:04:05+00:00",
        "2024-03-05T03:04:05 UTC",
        "2024-03-05T03:04:05.Z",
        "2024-03-05T24:00:00Z",
        "2024-03-05T03:60:00Z",
        "2024-03-05T03:04:61Z",
        // The strict RFC 3339 readers accept this one; the `%.fZ` grammar does
        // not, because its literal `Z` must follow the fraction directly.
        "2024-03-05T03:04:05.5 Z",
    ] {
        assert!(
            Timestamp::parse_naive_utc(text).is_err(),
            "{text} must be rejected by %Y-%m-%dT%H:%M:%S%.fZ"
        );
    }
}

#[test]
fn the_naive_readers_stay_apart_from_the_rfc3339_readers() {
    // RFC 3339 only: an offset. The naive grammar must not read it.
    assert!(Timestamp::parse_rfc3339("2024-03-05T03:04:05+09:00").is_ok());
    assert!(Timestamp::parse_naive_utc("2024-03-05T03:04:05+09:00").is_err());

    // Naive only: unpadded fields. The strict RFC 3339 grammar must not read it.
    assert!(Timestamp::parse_naive_utc("2024-3-5T3:4:5Z").is_ok());
    assert!(Timestamp::parse_rfc3339("2024-3-5T3:4:5Z").is_err());

    // A space instead of `T` belongs to the relaxed reader alone.
    assert!("2024-03-05 03:04:05Z".parse::<Timestamp>().is_ok());
    assert!(Timestamp::parse_naive_utc("2024-03-05 03:04:05Z").is_err());
}

#[test]
fn calendar_utc_keeps_a_leap_second_on_its_own_date() {
    let leap = "2016-12-31T23:59:60.5Z"
        .parse::<Timestamp>()
        .expect("a leap second parses");
    let calendar = leap.calendar_utc();
    assert_eq!(
        (calendar.year, calendar.month, calendar.day),
        (2016, 12, 31),
        "the date stays the `:59` one"
    );
    assert_eq!((calendar.hour, calendar.minute, calendar.second), (23, 59, 60));
    assert_eq!(calendar.nanosecond, 500_000_000);
    assert_eq!(calendar.date(), "2016-12-31");
    assert!(calendar.is_leap_second());

    // The folded instant these fields must not come from.
    assert_eq!(leap.utc().date().to_string(), "2017-01-01");
}

#[test]
fn calendar_utc_matches_the_folded_fields_for_a_normal_second() {
    let at = Timestamp::from_timestamp(1_700_000_000, 4_000_000).expect("a representable instant");
    let calendar = at.calendar_utc();
    let utc = at.utc();
    assert_eq!(calendar.year, utc.year());
    assert_eq!(calendar.month, utc.month() as u8);
    assert_eq!(calendar.day, utc.day());
    assert_eq!(calendar.hour, utc.hour());
    assert_eq!(calendar.minute, utc.minute());
    assert_eq!(calendar.second, utc.second());
    assert_eq!(calendar.nanosecond, 4_000_000);
    assert_eq!(calendar.date(), "2023-11-14");
    assert_eq!(calendar.month_key(), "2023-11");
    assert!(!calendar.is_leap_second());
}

#[test]
fn calendar_utc_signs_the_year_outside_four_digits() {
    let negative = PrimitiveDateTime::new(
        Date::from_calendar_date(-44, Month::March, 15).expect("a valid date"),
        Time::MIDNIGHT,
    )
    .assume_utc();
    let calendar = Timestamp::from(negative).calendar_utc();
    assert_eq!((calendar.year, calendar.month, calendar.day), (-44, 3, 15));
    assert_eq!(calendar.date(), "-0044-03-15");
    assert_eq!(calendar.month_key(), "-0044-03");

    let extended = PrimitiveDateTime::new(
        Date::from_calendar_date(12345, Month::June, 7).expect("a valid date"),
        Time::from_hms(8, 9, 10).expect("a valid time"),
    )
    .assume_utc();
    let calendar = Timestamp::from(extended).calendar_utc();
    assert_eq!(calendar.date(), "+12345-06-07");
    assert_eq!(calendar.month_key(), "+12345-06");
}

#[test]
fn calendar_local_reads_the_offset_of_the_stored_second() {
    let at = Timestamp::from_timestamp(1_700_000_000, 500_000_000).expect("a representable instant");
    let (calendar, offset) = at.calendar_local();
    let local = at.local();
    assert_eq!(offset, local.offset());
    assert_eq!(calendar.year, local.year());
    assert_eq!(calendar.hour, local.hour());
    assert_eq!(calendar.second, local.second());
    assert_eq!(calendar.nanosecond, 500_000_000);
}

#[test]
fn calendar_local_keeps_the_leap_second_second() {
    let leap = "2016-12-31T23:59:60.5Z"
        .parse::<Timestamp>()
        .expect("a leap second parses");
    let (calendar, offset) = leap.calendar_local();
    let expected_offset = UtcOffset::local_offset_at(leap.stored_utc()).unwrap_or(UtcOffset::UTC);
    assert_eq!(offset, expected_offset);
    assert_eq!(calendar.second, 60, "a local reader keeps the `:60` second");
    assert_eq!(calendar.nanosecond, 500_000_000);
    // Same wall clock as UTC here; the assertion is that only the offset moved.
    assert_eq!(calendar.minute, 59);
}

#[test]
fn ordering_puts_a_leap_second_before_the_next_second() {
    let just_before = "2016-12-31T23:59:59.500Z"
        .parse::<Timestamp>()
        .expect("a valid timestamp");
    let leap = "2016-12-31T23:59:60.500Z"
        .parse::<Timestamp>()
        .expect("a valid timestamp");
    let next = "2017-01-01T00:00:00Z".parse::<Timestamp>().expect("a valid timestamp");
    assert!(just_before < leap);
    assert!(leap < next);
    assert_eq!(leap.timestamp(), just_before.timestamp());
}

#[test]
fn stored_text_round_trips_through_parse_and_display() {
    // Each entry is already the automatic-precision display form, so the text
    // must come back byte-identical.
    let texts = [
        "1970-01-01T00:00:00Z",
        "0001-01-01T00:00:00Z",
        "2024-02-29T23:59:59.999999999Z",
        "1969-12-31T23:59:59.500Z",
        "2016-12-31T23:59:60.250Z",
        "2023-11-14T22:13:20.004Z",
    ];
    for text in texts {
        let at = text.parse::<Timestamp>().expect("a valid timestamp");
        assert_eq!(at.to_string(), text);
    }
}

#[test]
fn the_canonical_hash_form_is_utc_nanos_with_z() {
    let at = Timestamp::from_timestamp(1_700_000_000, 4_000_000).expect("a representable instant");
    assert_eq!(
        at.to_rfc3339_opts(Precision::Nanos, true),
        "2023-11-14T22:13:20.004000000Z"
    );
    assert_eq!(
        Timestamp::UNIX_EPOCH.to_rfc3339_opts(Precision::Nanos, true),
        "1970-01-01T00:00:00.000000000Z"
    );
}

/// The system zone database is reachable through `TZ` only where POSIX
/// timezones exist, which Windows does not implement, so the whole probe is
/// Unix-only. This module carries the crate's single OS-specific `cfg` line.
#[cfg(unix)]
mod local_zone_tests {
    use super::*;

    /// Set in the child process so the same test asserts instead of spawning.
    const CHILD_ZONE: &str = "HORON_AGENT_TIME_ZONE_CHILD";

    /// The exact libtest name of the test below, in this nested module.
    const TEST_NAME: &str = "tests::local_zone_tests::system_zone_samples_match_the_zone_database";

    /// The zones the parent runs, each in its own child process.
    const ZONES: [&str; 3] = ["America/New_York", "Asia/Seoul", "Europe/Moscow"];

    /// The sample set each zone must report, as `(instant text, seconds east)`.
    ///
    /// Seoul ran summer time in 1988, Moscow held +04:00 until 2014 and +03:00
    /// after, and New York changes offset every year, so each pair also proves
    /// the offset is read at the instant rather than from the current clock.
    fn samples(zone: &str) -> Option<&'static [(&'static str, i32)]> {
        match zone {
            "America/New_York" => Some(&[("2024-01-15T12:00:00Z", -18_000), ("2024-07-15T12:00:00Z", -14_400)]),
            "Asia/Seoul" => Some(&[("1988-01-01T00:00:00Z", 32_400), ("1988-07-01T00:00:00Z", 36_000)]),
            "Europe/Moscow" => Some(&[("2012-01-01T00:00:00Z", 14_400), ("2015-01-01T00:00:00Z", 10_800)]),
            _ => None,
        }
    }

    #[test]
    fn system_zone_samples_match_the_zone_database() {
        if let Some(zone) = std::env::var_os(CHILD_ZONE) {
            // Child: assert the samples for the zone this process was given.
            let zone = zone.to_string_lossy().into_owned();
            let samples = samples(&zone).expect("the child gets a known zone");
            for (text, seconds) in samples {
                let at = text.parse::<Timestamp>().expect("a valid instant");
                assert_eq!(at.local().offset().whole_seconds(), *seconds, "{zone} at {text}");
            }
            return;
        }

        // Parent: one child per zone, so the zone can differ between samples
        // without racing on one process environment.
        let exe = std::env::current_exe().expect("the test binary path");
        for zone in ZONES {
            let output = std::process::Command::new(&exe)
                .args([TEST_NAME, "--exact", "--test-threads=1"])
                .env("TZ", zone)
                .env(CHILD_ZONE, zone)
                .output()
                .expect("the child runs");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "the {zone} child failed: {stdout}{}",
                String::from_utf8_lossy(&output.stderr)
            );
            // A name typo would run no test and still exit 0.
            assert!(stdout.contains("1 passed"), "the {zone} child ran no sample: {stdout}");
        }
    }
}
