pub use horon_agent_core::util::*;
pub use horon_agent_provider_core::http_error_body;

use horon_agent_time::{Calendar, Timestamp};

/// The local calendar reading and its offset in seconds, from one lookup.
fn local_calendar(now: Timestamp) -> (Calendar, i32) {
    let (calendar, offset) = now.calendar_local();
    (calendar, offset.whole_seconds())
}

/// The `%Y` year text for the compact `YYYYMMDD` stamps, which [`Calendar::date`]
/// cannot serve because it carries separators. The rule is the one `date()`
/// applies: four zero-padded digits inside `0..=9999`, otherwise the ISO 8601
/// sign and five characters (`-0005`, `+12000`).
fn compact_year(calendar: &Calendar) -> String {
    if (0..10_000).contains(&calendar.year) {
        format!("{:04}", calendar.year)
    } else {
        format!("{:+05}", calendar.year)
    }
}

/// `YYYY-MM-DDTHH:MM:SS.f{3}` plus `±HHMM`, e.g. `2026-01-02T03:04:05.678+0900`.
fn stamp_with_offset(calendar: &Calendar, offset: i32) -> String {
    let (sign, offset) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
    format!(
        "{}T{:02}:{:02}:{:02}.{:03}{sign}{:02}{:02}",
        calendar.date(),
        calendar.hour,
        calendar.minute,
        calendar.second,
        calendar.nanosecond / 1_000_000,
        offset / 3_600,
        (offset % 3_600) / 60,
    )
}

/// `YYYY-MM-DDTHH:MM:SS.f{3}` plus the numeric offset, e.g.
/// `2026-01-02T03:04:05.678+0900`.
pub fn offset_stamp_millis(now: Timestamp) -> String {
    let (calendar, offset) = local_calendar(now);
    stamp_with_offset(&calendar, offset)
}

/// Compact UTC stamp with milliseconds, e.g. `20260102T030405.678Z`.
pub fn utc_stamp_millis(now: Timestamp) -> String {
    let calendar = now.calendar_utc();
    format!(
        "{}{:02}{:02}T{:02}{:02}{:02}.{:03}Z",
        compact_year(&calendar),
        calendar.month,
        calendar.day,
        calendar.hour,
        calendar.minute,
        calendar.second,
        calendar.nanosecond / 1_000_000,
    )
}

/// Compact UTC stamp with whole seconds, e.g. `20260102T030405Z`.
pub fn utc_stamp_seconds(now: Timestamp) -> String {
    let calendar = now.calendar_utc();
    format!(
        "{}{:02}{:02}T{:02}{:02}{:02}Z",
        compact_year(&calendar),
        calendar.month,
        calendar.day,
        calendar.hour,
        calendar.minute,
        calendar.second,
    )
}

/// UTC calendar date, e.g. `2026-01-02`.
pub fn utc_date(now: Timestamp) -> String {
    now.calendar_utc().date()
}

/// UTC calendar month, e.g. `2026-01`.
pub fn utc_month(now: Timestamp) -> String {
    now.calendar_utc().month_key()
}

/// UTC date and minute, e.g. `2026-01-02 03:04`.
pub fn utc_minutes(now: Timestamp) -> String {
    let calendar = now.calendar_utc();
    format!("{} {:02}:{:02}", calendar.date(), calendar.hour, calendar.minute)
}

/// UTC file-name stamp, e.g. `2026-01-02-030405`.
pub fn utc_stamp_filename(now: Timestamp) -> String {
    let calendar = now.calendar_utc();
    format!(
        "{}-{:02}{:02}{:02}",
        calendar.date(),
        calendar.hour,
        calendar.minute,
        calendar.second,
    )
}

/// `YYYY-MM-DD` in the system's local offset, for log files and retention.
pub fn local_date(now: Timestamp) -> String {
    now.calendar_local().0.date()
}

/// The local date and the local wall-clock stamp of one instant, read from a
/// single local-offset lookup so a log line never straddles a zone change.
pub fn local_date_and_stamp_millis(now: Timestamp) -> (String, String) {
    let (calendar, offset) = local_calendar(now);
    (calendar.date(), stamp_with_offset(&calendar, offset))
}

/// Format an anyhow error including its full cause chain.
///
/// This preserves actionable upstream details such as HTTP status/body instead of
/// only showing the outermost context message.
pub fn format_error_chain(err: &anyhow::Error) -> String {
    let mut parts = Vec::new();
    for cause in err.chain() {
        let text = cause.to_string();
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        if parts.last().is_some_and(|prev: &String| prev == trimmed) {
            continue;
        }
        parts.push(trimmed.to_string());
    }

    match parts.len() {
        0 => "unknown error".to_string(),
        1 => parts.remove(0),
        _ => parts.join(": "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(millis: i64) -> Timestamp {
        Timestamp::from_timestamp_millis(millis).expect("in range")
    }

    #[test]
    fn utc_stamps_keep_the_previous_text() {
        // 2026-01-02T03:04:05.678Z
        let now = at(1_767_323_045_678);
        assert_eq!(utc_stamp_millis(now), "20260102T030405.678Z");
        assert_eq!(utc_stamp_seconds(now), "20260102T030405Z");
        assert_eq!(utc_date(now), "2026-01-02");
        assert_eq!(utc_month(now), "2026-01");
        assert_eq!(utc_minutes(now), "2026-01-02 03:04");
        assert_eq!(utc_stamp_filename(now), "2026-01-02-030405");
    }

    #[test]
    fn utc_stamps_zero_pad_the_year_field() {
        // 1970-01-01T00:00:00Z is the epoch, and 1969-12-31T23:59:59Z is one
        // second before it; both stay four digits like `%Y`.
        assert_eq!(utc_date(Timestamp::UNIX_EPOCH), "1970-01-01");
        assert_eq!(utc_stamp_seconds(Timestamp::UNIX_EPOCH), "19700101T000000Z");
        let before = at(-1_000);
        assert_eq!(utc_date(before), "1969-12-31");
        assert_eq!(utc_stamp_millis(before), "19691231T235959.000Z");
    }

    #[test]
    fn extended_years_keep_the_iso_sign_in_every_stamp() {
        // `%Y` wrote a signed, five-character year outside `0..=9999`; the
        // compact stamps carry the same text, so a reader can round-trip it.
        let early = at(-62_324_899_200_000); // -0005-01-02T00:00:00Z
        assert_eq!(utc_date(early), "-0005-01-02");
        assert_eq!(utc_month(early), "-0005-01");
        assert_eq!(utc_stamp_seconds(early), "-00050102T000000Z");
        assert_eq!(utc_stamp_millis(early), "-00050102T000000.000Z");
        assert_eq!(utc_stamp_filename(early), "-0005-01-02-000000");

        let late = at(316_516_204_800_000); // +12000-01-01T00:00:00Z
        assert_eq!(utc_date(late), "+12000-01-01");
        assert_eq!(utc_stamp_seconds(late), "+120000101T000000Z");
    }

    #[test]
    fn leap_second_prints_second_sixty_on_its_own_date() {
        // 2016-12-31T23:59:60.5Z is stored on the `:59` second with a leaping
        // nanosecond field; the date must not roll into the next minute.
        let leap = Timestamp::from_timestamp(1_483_228_799, 1_500_000_000).expect("leap second");
        assert_eq!(utc_stamp_millis(leap), "20161231T235960.500Z");
        assert_eq!(utc_stamp_seconds(leap), "20161231T235960Z");
        assert_eq!(utc_date(leap), "2016-12-31");
        assert_eq!(utc_stamp_filename(leap), "2016-12-31-235960");
    }

    #[test]
    fn offset_stamp_is_local_wall_clock_plus_numeric_offset() {
        let now = at(1_767_323_045_678);
        let stamp = offset_stamp_millis(now);
        // `YYYY-MM-DDTHH:MM:SS.mmm±HHMM`, whatever the host zone is.
        assert_eq!(stamp.len(), "2026-01-02T03:04:05.678+0900".len(), "{stamp}");
        assert_eq!(stamp.as_bytes()[4], b'-', "{stamp}");
        assert_eq!(stamp.as_bytes()[7], b'-', "{stamp}");
        assert_eq!(stamp.as_bytes()[10], b'T', "{stamp}");
        assert_eq!(stamp.as_bytes()[13], b':', "{stamp}");
        assert_eq!(stamp.as_bytes()[16], b':', "{stamp}");
        assert_eq!(stamp.as_bytes()[19], b'.', "{stamp}");
        assert!(matches!(stamp.as_bytes()[23], b'+' | b'-'), "{stamp}");
        // The date part matches the local date of the same instant, and the
        // local reading of that instant is the one the stamp printed.
        let (calendar, offset) = now.calendar_local();
        assert_eq!(&stamp[..10], calendar.date().as_str(), "{stamp}");
        assert_eq!(
            &stamp[11..19],
            format!("{:02}:{:02}:{:02}", calendar.hour, calendar.minute, calendar.second),
            "{stamp}"
        );
        let (date, same_stamp) = local_date_and_stamp_millis(now);
        assert_eq!(stamp, same_stamp);
        assert_eq!(date, stamp[..10]);
        // The offset in the text is the one `calendar_local` used.
        let offset = offset.whole_seconds();
        let (sign, offset) = if offset < 0 { ('-', -offset) } else { ('+', offset) };
        assert_eq!(
            &stamp[23..],
            format!("{sign}{:02}{:02}", offset / 3_600, (offset % 3_600) / 60),
            "{stamp}"
        );
    }

    #[test]
    fn offset_stamp_pads_the_numeric_offset_to_four_digits() {
        let stamp = offset_stamp_millis(at(0));
        // Hours and minutes of the offset are each two digits (`+0900`, `-0530`).
        let offset = &stamp[23..];
        assert_eq!(offset.len(), 5, "{stamp}");
        assert!(offset[1..].chars().all(|c| c.is_ascii_digit()), "{stamp}");
    }

    #[test]
    fn test_format_error_chain_includes_nested_causes() {
        let err = anyhow::anyhow!("HTTP 400: invalid argument").context("Gemini generateContent failed");
        assert_eq!(
            format_error_chain(&err),
            "Gemini generateContent failed: HTTP 400: invalid argument"
        );
    }

    #[test]
    fn test_format_error_chain_deduplicates_repeated_messages() {
        let err = anyhow::anyhow!("same").context("same");
        assert_eq!(format_error_chain(&err), "same");
    }
}
