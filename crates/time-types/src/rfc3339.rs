//! RFC 3339 text for [`Timestamp`]: one writer, one parser.
//!
//! The writer is the single definition of the stored form, so the hashed text,
//! the serialized field and the log line all agree. Both directions keep the
//! leap-second spelling (second 60) that the previous chrono value accepted and
//! produced.

use core::fmt::{self, Write};

use time::{Date, Month, PrimitiveDateTime, Time, UtcOffset};

use crate::Timestamp;

/// The longest RFC 3339 text this module writes: a signed six-digit year, the
/// date, the time with nine fractional digits and a six-byte `+00:00` offset.
const MAX_RFC3339_LEN: usize = 40;

/// How many fractional-second digits [`Timestamp::to_rfc3339_opts`] writes.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum Precision {
    /// No fractional digits.
    Seconds,
    /// Exactly three digits.
    Millis,
    /// Exactly nine digits.
    Nanos,
    /// No digits for a whole second, then the three, six or nine digits the
    /// stored value actually has.
    #[default]
    Auto,
}

/// Why a text value is not an RFC 3339 instant this crate stores.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ParseError {
    reason: &'static str,
}

impl ParseError {
    pub(crate) const fn new(reason: &'static str) -> Self {
        Self { reason }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid RFC 3339 timestamp: ")?;
        formatter.write_str(self.reason)
    }
}

impl std::error::Error for ParseError {}

/// The stored text: one allocation, no intermediate strings.
pub(crate) fn to_rfc3339(timestamp: Timestamp, precision: Precision, use_z: bool) -> String {
    let mut text = String::with_capacity(MAX_RFC3339_LEN);
    match write(&mut text, timestamp, precision, use_z) {
        Ok(()) => text,
        // Writing into a `String` cannot fail; the arm exists because
        // `fmt::Write` reports a `Result`.
        Err(fmt::Error) => text,
    }
}

/// Append the RFC 3339 text of `timestamp` to `sink`.
pub(crate) fn write(sink: &mut impl Write, timestamp: Timestamp, precision: Precision, use_z: bool) -> fmt::Result {
    let at = timestamp.stored_utc();
    let year = at.year();
    if (0..10_000).contains(&year) {
        write!(sink, "{year:04}")?;
    } else {
        // Outside the four-digit range the year carries an explicit sign.
        write!(sink, "{year:+05}")?;
    }

    let month = at.month() as u8;
    let day = at.day();
    let hour = at.hour();
    let minute = at.minute();
    let mut second = at.second();
    let mut nanos = timestamp.timestamp_subsec_nanos();
    if nanos >= 1_000_000_000 {
        second += 1;
        nanos -= 1_000_000_000;
    }
    write!(sink, "-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}")?;

    match precision {
        Precision::Seconds => {}
        Precision::Millis => {
            let millis = nanos / 1_000_000;
            write!(sink, ".{millis:03}")?;
        }
        Precision::Nanos => write!(sink, ".{nanos:09}")?,
        Precision::Auto => {
            if nanos != 0 {
                if nanos.is_multiple_of(1_000_000) {
                    let millis = nanos / 1_000_000;
                    write!(sink, ".{millis:03}")?;
                } else if nanos.is_multiple_of(1_000) {
                    let micros = nanos / 1_000;
                    write!(sink, ".{micros:06}")?;
                } else {
                    write!(sink, ".{nanos:09}")?;
                }
            }
        }
    }

    if use_z {
        sink.write_char('Z')
    } else {
        sink.write_str("+00:00")
    }
}

/// The calendar and offset fields both grammars produce.
struct Fields {
    year: i32,
    month: i32,
    day: i32,
    hour: i32,
    minute: i32,
    second: i32,
    fraction: u32,
    offset_seconds: i32,
}

/// Validate the parsed fields and build the instant, converted to UTC.
fn finish(fields: Fields) -> Result<Timestamp, ParseError> {
    let month = u8::try_from(fields.month).map_err(|_| ParseError::new("the month is out of range"))?;
    let month = Month::try_from(month).map_err(|_| ParseError::new("the month is out of range"))?;
    let day = u8::try_from(fields.day).map_err(|_| ParseError::new("the day is out of range"))?;
    let date =
        Date::from_calendar_date(fields.year, month, day).map_err(|_| ParseError::new("the date does not exist"))?;

    // A leap second is the 60th second; `time` stores it on the 59th second and
    // the stored fraction carries the leaping second.
    let (second, leap) = if fields.second == 60 {
        (59, 1_000_000_000_u32)
    } else {
        (fields.second, 0_u32)
    };
    let hour = u8::try_from(fields.hour).map_err(|_| ParseError::new("the hour is out of range"))?;
    let minute = u8::try_from(fields.minute).map_err(|_| ParseError::new("the minute is out of range"))?;
    let second = u8::try_from(second).map_err(|_| ParseError::new("the second is out of range"))?;
    let time = Time::from_hms_nano(hour, minute, second, fields.fraction)
        .map_err(|_| ParseError::new("the time of day does not exist"))?;
    let offset = UtcOffset::from_whole_seconds(fields.offset_seconds)
        .map_err(|_| ParseError::new("the UTC offset is out of range"))?;

    let at = PrimitiveDateTime::new(date, time).assume_offset(offset);
    Timestamp::from_timestamp(at.unix_timestamp(), fields.fraction + leap)
        .ok_or(ParseError::new("the instant is outside the supported range"))
}

/// The strict RFC 3339 grammar that `chrono::DateTime::parse_from_rfc3339`
/// accepted: a four-digit year, two-digit fields, no whitespace, and a colon in
/// the numeric offset.
///
/// This is the entry point for a caller that used the old strict function. The
/// text of an instant outside `0..=9999` is not read back here, which is what
/// the old strict parser did; `FromStr` and serde read that text.
pub(crate) fn parse_strict(text: &str) -> Result<Timestamp, ParseError> {
    let mut cursor = Cursor::new(text);
    let year = cursor.number(4)?;
    cursor.expect_byte(b'-', "expected '-' after the year")?;
    let month = cursor.number(2)?;
    cursor.expect_byte(b'-', "expected '-' after the month")?;
    let day = cursor.number(2)?;

    let separator = cursor
        .take_byte()
        .ok_or(ParseError::new("expected a date and time separator"))?;
    if !matches!(separator, b'T' | b't' | b' ') {
        return Err(ParseError::new("expected 'T' or a space between the date and time"));
    }

    let hour = cursor.number(2)?;
    cursor.expect_byte(b':', "expected ':' after the hour")?;
    let minute = cursor.number(2)?;
    cursor.expect_byte(b':', "expected ':' after the minute")?;
    let second = cursor.number(2)?;

    let fraction = parse_fraction(&mut cursor)?;
    let offset_seconds = parse_offset(&mut cursor)?;
    if cursor.remaining() != 0 {
        return Err(ParseError::new("unexpected characters after the UTC offset"));
    }
    finish(Fields {
        year,
        month,
        day,
        hour,
        minute,
        second,
        fraction,
        offset_seconds,
    })
}

/// The relaxed RFC 3339 grammar that `chrono`'s `FromStr` and serde accepted.
///
/// Every leniency is kept, because stored records were read through serde:
/// fields need no two-digit padding, a signed year of any length is allowed,
/// whitespace may precede any component, `UTC` names the zero offset, the colon
/// in the offset may be missing, and trailing whitespace is ignored.
pub(crate) fn parse_relaxed(text: &str) -> Result<Timestamp, ParseError> {
    let mut rest = text;
    let year = relaxed_year(&mut rest)?;
    skip_space(&mut rest);
    rest = rest
        .strip_prefix('-')
        .ok_or(ParseError::new("expected '-' after the year"))?;
    skip_space(&mut rest);
    let month = relaxed_number(&mut rest, 2)?;
    skip_space(&mut rest);
    rest = rest
        .strip_prefix('-')
        .ok_or(ParseError::new("expected '-' after the month"))?;
    skip_space(&mut rest);
    let day = relaxed_number(&mut rest, 2)?;

    let separator = rest
        .chars()
        .next()
        .ok_or(ParseError::new("expected a date and time separator"))?;
    if !matches!(separator, 'T' | 't' | ' ') {
        return Err(ParseError::new("expected 'T' or a space between the date and time"));
    }
    rest = &rest[separator.len_utf8()..];

    skip_space(&mut rest);
    let hour = relaxed_number(&mut rest, 2)?;
    skip_space(&mut rest);
    rest = rest
        .strip_prefix(':')
        .ok_or(ParseError::new("expected ':' after the hour"))?;
    skip_space(&mut rest);
    let minute = relaxed_number(&mut rest, 2)?;
    skip_space(&mut rest);
    rest = rest
        .strip_prefix(':')
        .ok_or(ParseError::new("expected ':' after the minute"))?;
    skip_space(&mut rest);
    let second = relaxed_number(&mut rest, 2)?;

    let fraction = relaxed_fraction(&mut rest)?;
    skip_space(&mut rest);
    let offset_seconds = relaxed_offset(&mut rest)?;
    if !rest.trim_start().is_empty() {
        return Err(ParseError::new("unexpected characters after the UTC offset"));
    }
    finish(Fields {
        year,
        month,
        day,
        hour,
        minute,
        second,
        fraction,
        offset_seconds,
    })
}

/// Skip leading whitespace, which the relaxed grammar allows before a component.
fn skip_space(rest: &mut &str) {
    *rest = rest.trim_start();
}

/// A year: one to four digits, or a sign and any number of digits.
fn relaxed_year(rest: &mut &str) -> Result<i32, ParseError> {
    skip_space(rest);
    if let Some(after) = rest.strip_prefix('-') {
        *rest = after;
        let magnitude = relaxed_long_number(rest)?;
        return magnitude
            .checked_neg()
            .ok_or(ParseError::new("the year is out of range"));
    }
    if let Some(after) = rest.strip_prefix('+') {
        *rest = after;
        return relaxed_long_number(rest);
    }
    relaxed_number(rest, 4)
}

/// One or more digits, for the signed year form.
fn relaxed_long_number(rest: &mut &str) -> Result<i32, ParseError> {
    let bytes = rest.as_bytes();
    let mut value = 0_i64;
    let mut taken = 0;
    while let Some(byte) = bytes.get(taken) {
        if !byte.is_ascii_digit() {
            break;
        }
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(i64::from(byte - b'0')))
            .ok_or(ParseError::new("the year is out of range"))?;
        taken += 1;
    }
    if taken == 0 {
        return Err(ParseError::new("expected a digit"));
    }
    *rest = &rest[taken..];
    i32::try_from(value).map_err(|_| ParseError::new("the year is out of range"))
}

/// One to `max_digits` decimal digits, as the relaxed grammar reads a field.
fn relaxed_number(rest: &mut &str, max_digits: usize) -> Result<i32, ParseError> {
    let bytes = rest.as_bytes();
    let mut value = 0_i32;
    let mut taken = 0;
    while taken < max_digits {
        let Some(byte) = bytes.get(taken) else { break };
        if !byte.is_ascii_digit() {
            break;
        }
        value = value * 10 + i32::from(byte - b'0');
        taken += 1;
    }
    if taken == 0 {
        return Err(ParseError::new("expected a digit"));
    }
    *rest = &rest[taken..];
    Ok(value)
}

/// An optional `.` and one to nine significant digits; later digits are dropped.
fn relaxed_fraction(rest: &mut &str) -> Result<u32, ParseError> {
    let Some(after) = rest.strip_prefix('.') else {
        return Ok(0);
    };
    let bytes = after.as_bytes();
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
    *rest = &after[taken..];
    Ok(value)
}

/// The offset as seconds east of Greenwich: `Z`, `UTC`, or `±HH[: ]MM`.
fn relaxed_offset(rest: &mut &str) -> Result<i32, ParseError> {
    if let Some(after) = rest.strip_prefix(|c: char| c == 'Z' || c == 'z') {
        *rest = after;
        return Ok(0);
    }
    if rest.get(..3).is_some_and(|head| head.eq_ignore_ascii_case("UTC")) {
        *rest = &rest[3..];
        return Ok(0);
    }

    let negative = if let Some(after) = rest.strip_prefix('+') {
        *rest = after;
        false
    } else if let Some(after) = rest.strip_prefix('-').or_else(|| rest.strip_prefix('−')) {
        *rest = after;
        true
    } else {
        return Err(ParseError::new("expected 'Z' or a numeric UTC offset"));
    };

    let hour = exact_number(rest, 2)?;
    // The old scanner accepted a colon, whitespace, or nothing here.
    *rest = rest.trim_start_matches(|c: char| c == ':' || c.is_whitespace());
    let minute = exact_number(rest, 2)?;
    if minute > 59 {
        return Err(ParseError::new("the UTC offset is out of range"));
    }
    let magnitude = hour * 3_600 + minute * 60;
    if magnitude > 86_340 {
        return Err(ParseError::new("the UTC offset is out of range"));
    }
    Ok(if negative { -magnitude } else { magnitude })
}

/// Exactly `count` ASCII digits.
fn exact_number(rest: &mut &str, count: usize) -> Result<i32, ParseError> {
    let bytes = rest.as_bytes();
    if bytes.len() < count {
        return Err(ParseError::new("expected a two-digit field"));
    }
    let mut value = 0_i32;
    for &byte in &bytes[..count] {
        if !byte.is_ascii_digit() {
            return Err(ParseError::new("expected a two-digit field"));
        }
        value = value * 10 + i32::from(byte - b'0');
    }
    *rest = &rest[count..];
    Ok(value)
}

/// The fraction of a second, on the digits present; digits past the ninth are
/// dropped, which is the truncation the previous parser applied.
fn parse_fraction(cursor: &mut Cursor<'_>) -> Result<u32, ParseError> {
    if cursor.peek() != Some(b'.') {
        return Ok(0);
    }
    cursor.advance();
    let mut value = 0_u32;
    let mut digits = 0_u32;
    while let Some(byte) = cursor.peek() {
        if !byte.is_ascii_digit() {
            break;
        }
        cursor.advance();
        if digits < 9 {
            value = value * 10 + u32::from(byte - b'0');
            digits += 1;
        }
    }
    if digits == 0 {
        return Err(ParseError::new("expected digits after the decimal point"));
    }
    for _ in digits..9 {
        value *= 10;
    }
    Ok(value)
}

/// The UTC offset in seconds east of Greenwich.
fn parse_offset(cursor: &mut Cursor<'_>) -> Result<i32, ParseError> {
    if matches!(cursor.peek(), Some(b'Z' | b'z')) {
        cursor.advance();
        return Ok(0);
    }
    let negative = cursor
        .take_sign()
        .ok_or(ParseError::new("expected 'Z' or a numeric UTC offset"))?;
    let hour = cursor.number(2)?;
    cursor.expect_byte(b':', "expected ':' in the UTC offset")?;
    let minute = cursor.number(2)?;
    if hour > 23 || minute > 59 {
        return Err(ParseError::new("the UTC offset is out of range"));
    }
    let magnitude = hour * 3_600 + minute * 60;
    Ok(if negative { -magnitude } else { magnitude })
}

/// A byte cursor over the text being parsed, so no slice is re-indexed by hand.
struct Cursor<'a> {
    bytes: &'a [u8],
    index: usize,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            bytes: text.as_bytes(),
            index: 0,
        }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.index
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.index).copied()
    }

    /// Skip one byte that `peek` already inspected.
    fn advance(&mut self) {
        self.index += 1;
    }

    fn take_byte(&mut self) -> Option<u8> {
        let byte = self.bytes.get(self.index).copied()?;
        self.index += 1;
        Some(byte)
    }

    fn expect_byte(&mut self, expected: u8, reason: &'static str) -> Result<(), ParseError> {
        match self.take_byte() {
            Some(byte) if byte == expected => Ok(()),
            _ => Err(ParseError::new(reason)),
        }
    }

    /// Consume the offset's sign: `+` is not negative, `-` and the Unicode
    /// minus sign are. The old scanner took both minus characters.
    fn take_sign(&mut self) -> Option<bool> {
        match self.peek() {
            Some(b'+') => {
                self.advance();
                Some(false)
            }
            Some(b'-') => {
                self.advance();
                Some(true)
            }
            _ => {
                let rest = self.bytes.get(self.index..)?;
                if rest.starts_with("−".as_bytes()) {
                    self.index += "−".len();
                    Some(true)
                } else {
                    None
                }
            }
        }
    }

    /// Exactly `count` decimal digits, as a value that fits `i32`.
    fn number(&mut self, count: usize) -> Result<i32, ParseError> {
        let mut value = 0_i32;
        for _ in 0..count {
            let byte = self.take_byte().ok_or(ParseError::new("expected a digit"))?;
            if !byte.is_ascii_digit() {
                return Err(ParseError::new("expected a digit"));
            }
            value = value * 10 + i32::from(byte - b'0');
        }
        Ok(value)
    }
}
