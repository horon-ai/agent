//! The two `chrono::Naive*::parse_from_str` grammars the usage and search
//! filters read, kept apart from the RFC 3339 readers.
//!
//! `base`'s usage display parses a reset time as `%Y-%m-%d`, and the session
//! search filter parses `after`/`before` as `%Y-%m-%dT%H:%M:%S%.fZ`. Both went
//! through `chrono`'s `StrftimeItems` parser, whose rules differ from RFC 3339:
//! a numeric item trims the whitespace in front of itself and needs no zero
//! padding, a signed year takes any number of digits, and the format's literal
//! characters are matched exactly. Reproducing those rules here is what keeps a
//! stored or user-supplied value parsing as it did.

use time::{Date, Month, PrimitiveDateTime, Time};

use crate::Timestamp;
use crate::rfc3339::ParseError;

/// `chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")`, as UTC midnight.
pub(crate) fn parse_date(text: &str) -> Result<Timestamp, ParseError> {
    let mut scan = Scan::new(text);
    let date = scan.date()?;
    if !scan.rest.is_empty() {
        return Err(ParseError::new("unexpected characters after the date"));
    }
    let seconds = date.midnight().assume_utc().unix_timestamp();
    Timestamp::from_timestamp(seconds, 0).ok_or(ParseError::new("the date is outside the supported range"))
}

/// `chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.fZ")`.
pub(crate) fn parse_datetime_utc(text: &str) -> Result<Timestamp, ParseError> {
    let mut scan = Scan::new(text);
    let date = scan.date()?;
    // The literal `T` and `Z` are case-sensitive here; the relaxed RFC 3339
    // reader is the one that accepts lower case and other separators.
    scan.expect_literal('T', "expected 'T' between the date and the time")?;
    let hour = scan.number(1, 2)?;
    scan.expect_literal(':', "expected ':' after the hour")?;
    let minute = scan.number(1, 2)?;
    scan.expect_literal(':', "expected ':' after the minute")?;
    let second = scan.number(1, 2)?;
    let fraction = scan.fraction()?;
    scan.expect_literal('Z', "expected 'Z' after the time")?;
    if !scan.rest.is_empty() {
        return Err(ParseError::new("unexpected characters after the time"));
    }

    let hour = u8::try_from(hour).map_err(|_| ParseError::new("the hour is out of range"))?;
    let minute = u8::try_from(minute).map_err(|_| ParseError::new("the minute is out of range"))?;
    // The 60th second is a leap second: `time` carries it on the 59th second
    // with the leaping second in the nanosecond field, which is also how a
    // stored leap second is represented.
    let (second, leap) = if second == 60 {
        (59_u8, 1_000_000_000_u32)
    } else {
        let second = u8::try_from(second).map_err(|_| ParseError::new("the second is out of range"))?;
        (second, 0_u32)
    };
    let time = Time::from_hms_nano(hour, minute, second, fraction)
        .map_err(|_| ParseError::new("the time of day does not exist"))?;
    let seconds = PrimitiveDateTime::new(date, time).assume_utc().unix_timestamp();
    Timestamp::from_timestamp(seconds, fraction + leap)
        .ok_or(ParseError::new("the instant is outside the supported range"))
}

/// A cursor following `chrono`'s `StrftimeItems` scanning rules.
struct Scan<'a> {
    rest: &'a str,
}

impl<'a> Scan<'a> {
    fn new(text: &'a str) -> Self {
        Self { rest: text }
    }

    /// A numeric item trims the whitespace in front of itself
    /// (`parse_internal`: `s = s.trim_start()` runs only for a numeric spec).
    fn skip_space(&mut self) {
        self.rest = self.rest.trim_start();
    }

    /// A literal from the format string, matched exactly as written.
    ///
    /// Chrono's literal arm does not trim: only its numeric and `Space` arms
    /// do, so whitespace in front of `-`, `T` or `Z` is an error here.
    fn expect_literal(&mut self, expected: char, reason: &'static str) -> Result<(), ParseError> {
        match self.rest.chars().next() {
            Some(found) if found == expected => {
                self.rest = &self.rest[expected.len_utf8()..];
                Ok(())
            }
            _ => Err(ParseError::new(reason)),
        }
    }

    /// One to `max_digits` ASCII digits: a numeric item's width is an upper
    /// bound, not a required length.
    fn number(&mut self, min_digits: usize, max_digits: usize) -> Result<i64, ParseError> {
        self.skip_space();
        let bytes = self.rest.as_bytes();
        let mut value = 0_i64;
        let mut taken = 0;
        while taken < max_digits {
            let Some(byte) = bytes.get(taken) else { break };
            if !byte.is_ascii_digit() {
                break;
            }
            value = value * 10 + i64::from(byte - b'0');
            taken += 1;
        }
        if taken < min_digits {
            return Err(ParseError::new("expected a digit"));
        }
        self.rest = &self.rest[taken..];
        Ok(value)
    }

    /// `%Y`: a sign and any number of digits, or one to four digits without a
    /// sign (`Item::Numeric(Year)` is the only signed width-4 spec).
    fn year(&mut self) -> Result<i32, ParseError> {
        self.skip_space();
        let negative = match self.rest.as_bytes().first() {
            Some(b'+') => {
                self.rest = &self.rest[1..];
                false
            }
            Some(b'-') => {
                self.rest = &self.rest[1..];
                true
            }
            _ => {
                let value = self.number(1, 4)?;
                return i32::try_from(value).map_err(|_| ParseError::new("the year is out of range"));
            }
        };
        let magnitude = self.unsigned_digits()?;
        if negative {
            let magnitude = i64::try_from(magnitude).map_err(|_| ParseError::new("the year is out of range"))?;
            return i32::try_from(-magnitude).map_err(|_| ParseError::new("the year is out of range"));
        }
        i32::try_from(magnitude).map_err(|_| ParseError::new("the year is out of range"))
    }

    /// Every digit available, as a value that fits `u64`.
    fn unsigned_digits(&mut self) -> Result<u64, ParseError> {
        let bytes = self.rest.as_bytes();
        let mut value = 0_u64;
        let mut taken = 0;
        while let Some(byte) = bytes.get(taken) {
            if !byte.is_ascii_digit() {
                break;
            }
            value = match value
                .checked_mul(10)
                .and_then(|value| value.checked_add(u64::from(byte - b'0')))
            {
                Some(value) => value,
                None => return Err(ParseError::new("the year is out of range")),
            };
            taken += 1;
        }
        if taken == 0 {
            return Err(ParseError::new("expected a digit"));
        }
        self.rest = &self.rest[taken..];
        Ok(value)
    }

    /// The `%Y-%m-%d` field group, in the order the format names it.
    fn date(&mut self) -> Result<Date, ParseError> {
        let year = self.year()?;
        self.expect_literal('-', "expected '-' after the year")?;
        let month = self.number(1, 2)?;
        self.expect_literal('-', "expected '-' after the month")?;
        let day = self.number(1, 2)?;

        let month = u8::try_from(month).map_err(|_| ParseError::new("the month is out of range"))?;
        let month = Month::try_from(month).map_err(|_| ParseError::new("the month is out of range"))?;
        let day = u8::try_from(day).map_err(|_| ParseError::new("the day is out of range"))?;
        Date::from_calendar_date(year, month, day).map_err(|_| ParseError::new("the date does not exist"))
    }

    /// `%.f`: a fraction only when a `.` is present, one to nine digits kept,
    /// and later digits consumed and dropped (`scan::nanosecond`).
    fn fraction(&mut self) -> Result<u32, ParseError> {
        if self.rest.as_bytes().first() != Some(&b'.') {
            return Ok(0);
        }
        self.rest = &self.rest[1..];
        let bytes = self.rest.as_bytes();
        let mut value = 0_u32;
        let mut taken = 0;
        while let Some(byte) = bytes.get(taken) {
            if !byte.is_ascii_digit() {
                break;
            }
            if taken < 9 {
                value = value * 10 + u32::from(byte - b'0');
            }
            taken += 1;
        }
        if taken == 0 {
            return Err(ParseError::new("expected digits after the decimal point"));
        }
        for _ in taken.min(9)..9 {
            value *= 10;
        }
        self.rest = &self.rest[taken..];
        Ok(value)
    }
}
