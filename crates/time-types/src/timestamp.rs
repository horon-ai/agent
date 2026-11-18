//! The stored instant: whole seconds plus a nanosecond field.

use core::fmt;
use core::ops::{Add, Sub};
use core::str::FromStr;
use std::time::SystemTime;

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::{Date, Duration, Month, OffsetDateTime, UtcOffset};

use crate::naive;
use crate::rfc3339::{self, ParseError, Precision};

/// Earliest whole second a [`Timestamp`] holds: January 1 of `MIN_YEAR`, UTC.
///
/// The bounds are the `chrono` 0.4.42 `NaiveDate` limits (`MIN_YEAR = -262_143`,
/// `MAX_YEAR = 262_142`), because the value space of this type has to stay the
/// value space of the `DateTime<Utc>` it replaces. `time::Date` is wider
/// (±999_999 years with `large-dates`) and that extra range is not exposed: a
/// stored instant outside these bounds never came from the old type.
const MIN_SECONDS: i64 = match Date::from_calendar_date(-262_143, Month::January, 1) {
    Ok(date) => date.midnight().assume_utc().unix_timestamp(),
    // Unreachable: the date is inside `time`'s range. The arm keeps this const
    // total without a panic path.
    Err(_) => time::Timestamp::MIN.as_seconds(),
};

/// Latest whole second a [`Timestamp`] holds: 23:59:59 UTC on December 31, 262142.
const MAX_SECONDS: i64 = match Date::from_calendar_date(262_142, Month::December, 31) {
    Ok(date) => date.midnight().assume_utc().unix_timestamp() + 86_399,
    Err(_) => time::Timestamp::MAX.as_seconds(),
};

/// A UTC instant stored as whole seconds since the Unix epoch plus nanoseconds.
///
/// The representation is the one the stored sessions were written with: the
/// second field counts whole seconds, and a leap second is carried in `nanos`
/// as `1_000_000_000 + fraction` on a `:59` second, so a parsed `:60` second
/// survives a rewrite of the same text. [`Timestamp::timestamp_subsec_nanos`]
/// therefore reports up to `1_999_999_999`.
///
/// Ordering, equality and hashing follow the stored pair, so two values are
/// equal only when they were stored identically; the map from a stored pair to
/// an instant is injective inside the supported range.
///
/// The range is the old `chrono::DateTime<Utc>` range (years -262_143..=262_142),
/// so a value that the old type could hold is exactly a value this type holds.
/// Arithmetic that leaves the range panics, at the same points the old type
/// panicked; there is no saturating form, because the old contract had none.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Timestamp {
    seconds: i64,
    nanos: u32,
}

impl Timestamp {
    /// The Unix epoch, `1970-01-01T00:00:00Z`.
    pub const UNIX_EPOCH: Self = Self { seconds: 0, nanos: 0 };

    /// The current system time.
    pub fn now() -> Self {
        Self::from(OffsetDateTime::now_utc())
    }

    /// Build an instant from whole seconds and a nanosecond field.
    ///
    /// `nanos` below one billion is the fraction of `seconds`;
    /// `1_000_000_000..2_000_000_000` is a leap second carried on the `:59`
    /// second, which is how a stored `:60` second is represented. The
    /// acceptance matches `chrono::DateTime::from_timestamp`: a field of two
    /// billion or more is `None`, a leaping field is only accepted when
    /// `seconds` is a `:59` second (`secs % 60 == 59`), and `seconds` must be
    /// inside the stored range.
    pub const fn from_timestamp(seconds: i64, nanos: u32) -> Option<Self> {
        if nanos >= 2_000_000_000 {
            return None;
        }
        if seconds < MIN_SECONDS || seconds > MAX_SECONDS {
            return None;
        }
        // The old type allowed the leaping nanosecond only on second 59 of a
        // minute, because that is the second a `:60` spelling is carried on.
        if nanos >= 1_000_000_000 && seconds.rem_euclid(60) != 59 {
            return None;
        }
        Some(Self { seconds, nanos })
    }

    /// Build an instant from milliseconds since the epoch.
    ///
    /// A negative value floors toward the earlier second, so `-1_500` is
    /// `1969-12-31T23:59:58.500Z` and not a value rounded toward the epoch.
    pub const fn from_timestamp_millis(millis: i64) -> Option<Self> {
        let seconds = millis.div_euclid(1_000);
        let nanos = millis.rem_euclid(1_000) as u32 * 1_000_000;
        Self::from_timestamp(seconds, nanos)
    }

    /// Whole seconds since the epoch, as stored.
    pub const fn timestamp(self) -> i64 {
        self.seconds
    }

    /// Milliseconds since the epoch, as stored.
    ///
    /// A leap second adds its milliseconds to the `:59` second, the same
    /// arithmetic the previous type performed.
    pub const fn timestamp_millis(self) -> i64 {
        self.seconds * 1_000 + (self.nanos / 1_000_000) as i64
    }

    /// The stored nanosecond field, up to `1_999_999_999` for a leap second.
    pub const fn timestamp_subsec_nanos(self) -> u32 {
        self.nanos
    }

    /// The instant as `time::OffsetDateTime` in UTC.
    ///
    /// A leap second is folded into the following second, because
    /// `time::OffsetDateTime` cannot carry one; the stored value keeps the leap
    /// second for formatting and the storage contract. The fold stays inside
    /// `time`'s range, which is wider than the stored range, so the fallback
    /// below is unreachable for any value this type can hold.
    pub fn utc(self) -> OffsetDateTime {
        let (seconds, nanos) = self.folded();
        match OffsetDateTime::from_unix_timestamp(seconds).and_then(|at| at.replace_nanosecond(nanos)) {
            Ok(at) => at,
            // Unreachable: every storable second, and the second after a leap
            // one, is inside the range `from_unix_timestamp` accepts.
            Err(_) => Self::boundary(seconds),
        }
    }

    /// The same instant in the system's local offset.
    ///
    /// The offset is read for this instant, so a timestamp inside a
    /// daylight-saving period takes that period's offset. When the system zone
    /// cannot be read the instant stays in UTC, which is what `chrono::Local`
    /// did for the same call.
    ///
    /// `time::OffsetDateTime` has no field for a leap second, so a leaping value
    /// is shown at the folded instant (the following second) rather than at
    /// `:60`; [`Timestamp::calendar_utc`] and [`Timestamp::calendar_local`] are
    /// the readers that keep the hour, the date and the `:60` a leap second was
    /// written with.
    pub fn local(self) -> OffsetDateTime {
        let utc = self.utc();
        match UtcOffset::local_offset_at(utc) {
            Ok(offset) => utc.to_offset(offset),
            Err(_) => utc,
        }
    }

    /// The stored instant's UTC calendar fields, with a leap second still on
    /// its own date.
    ///
    /// A stored leap second sits on its `:59` second with a nanosecond field at
    /// or above one billion; the instant that fields-of-the-calendar denotes is
    /// the following second, but every calendar consumer from the old clock
    /// (bucket keys, log lines, prompt text, file names) printed the `:59`
    /// second's date, hour and minute with a `:60` second. This returns exactly
    /// that, so a day, weekday or hour bucket never moves across midnight
    /// because of a leap second.
    pub fn calendar_utc(self) -> Calendar {
        Calendar::of(self.stored_utc(), self.nanos)
    }

    /// The same fields in the system offset, with the offset that applied to
    /// them.
    ///
    /// The offset is read for the stored second, not for the folded instant, so
    /// a leap second at the end of a month or across a zone change still takes
    /// the offset of the second it belongs to.
    pub fn calendar_local(self) -> (Calendar, UtcOffset) {
        let stored = self.stored_utc();
        let offset = UtcOffset::local_offset_at(stored).unwrap_or(UtcOffset::UTC);
        let at = stored.checked_to_offset(offset).unwrap_or(stored);
        (Calendar::of(at, self.nanos), offset)
    }

    /// RFC 3339 in UTC, with `+00:00` and the shortest of 0, 3, 6 or 9
    /// fractional digits that keeps every stored nanosecond.
    pub fn to_rfc3339(&self) -> String {
        rfc3339::to_rfc3339(*self, Precision::Auto, false)
    }

    /// RFC 3339 with a fixed or automatic fraction, and `Z` when `use_z` asks
    /// for the UTC designator instead of `+00:00`.
    pub fn to_rfc3339_opts(&self, precision: Precision, use_z: bool) -> String {
        rfc3339::to_rfc3339(*self, precision, use_z)
    }

    /// Read the strict RFC 3339 grammar, which is what
    /// `chrono::DateTime::parse_from_rfc3339` accepted: a four-digit year,
    /// two-digit fields, no whitespace, and a colon in the numeric offset.
    ///
    /// [`FromStr`] and serde use the relaxed grammar instead, so the text of an
    /// instant outside `0..=9999` is read there and not here. Use this entry
    /// point where the old code called the strict function.
    pub fn parse_rfc3339(text: &str) -> Result<Self, ParseError> {
        rfc3339::parse_strict(text)
    }

    /// Read `chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")`: a numeric
    /// field trims the whitespace before it, needs no zero padding, and a
    /// signed year takes any number of digits. The result is that day's UTC
    /// midnight, and trailing characters (a trailing space included) are
    /// rejected.
    ///
    /// This is a separate grammar from [`FromStr`]: no offset, no `UTC`, and a
    /// space separator are not accepted.
    pub fn parse_naive_date(text: &str) -> Result<Self, ParseError> {
        naive::parse_date(text)
    }

    /// Read `chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.fZ")`
    /// as a UTC instant.
    ///
    /// The same field rules as [`Timestamp::parse_naive_date`] apply, the
    /// literal `T` and `Z` are case-sensitive, the fraction runs to nine kept
    /// digits with later digits dropped, and a `:60` second is stored as a leap
    /// second on its `:59` second. This grammar takes no offset: use
    /// [`Timestamp::parse_rfc3339`] for a value that carries one.
    pub fn parse_naive_utc(text: &str) -> Result<Self, ParseError> {
        naive::parse_datetime_utc(text)
    }

    /// The UTC instant with a leap second folded into the next second.
    fn folded(self) -> (i64, u32) {
        if self.nanos >= 1_000_000_000 {
            (self.seconds.saturating_add(1), self.nanos - 1_000_000_000)
        } else {
            (self.seconds, self.nanos)
        }
    }

    /// The calendar fields as stored, so a leap second still prints second 60.
    pub(crate) fn stored_utc(self) -> OffsetDateTime {
        match OffsetDateTime::from_unix_timestamp(self.seconds) {
            Ok(at) => at,
            // Unreachable for the same reason as in `utc`.
            Err(_) => Self::boundary(self.seconds),
        }
    }

    /// The closest representable instant on the requested side of the epoch.
    fn boundary(seconds: i64) -> OffsetDateTime {
        if seconds < 0 {
            Date::MIN.midnight().assume_utc()
        } else {
            Date::MAX.midnight().assume_utc()
        }
    }

    /// Move by `duration` with the old type's leap-second arithmetic.
    ///
    /// This is `chrono::NaiveTime::overflowing_add_signed` translated to the
    /// stored pair: a leap second keeps its `:60` spelling while the result
    /// stays inside the leaping second, and is normalized to the following
    /// second only when the movement leaves it.
    ///
    /// A result outside the stored range fails with a panic, at the same points
    /// the old `DateTime + TimeDelta` failed; the old contract had no saturating
    /// form. `saturating_add` below only keeps the `i64` intermediate defined,
    /// and a saturated value is always outside the stored range, so the panic
    /// fires exactly when the true sum does.
    fn offset_by(self, duration: Duration) -> Self {
        let mut seconds = self.seconds;
        let mut nanos = i64::from(self.nanos);
        let add_seconds = duration.whole_seconds();
        let add_nanos = i64::from(duration.subsec_nanoseconds());

        if nanos >= 1_000_000_000 {
            if add_seconds > 0 || (add_nanos > 0 && nanos >= 2_000_000_000 - add_nanos) {
                // The movement escapes the leap second: count it as an
                // ordinary second and carry on.
                nanos -= 1_000_000_000;
            } else if add_seconds < 0 {
                nanos -= 1_000_000_000;
                seconds = seconds.saturating_add(1);
            } else {
                // Only the fraction moves, and it stays inside the leap second.
                return Self {
                    seconds,
                    nanos: (nanos + add_nanos) as u32,
                };
            }
        }

        seconds = seconds.saturating_add(add_seconds);
        nanos += add_nanos;
        if nanos < 0 {
            nanos += 1_000_000_000;
            seconds = seconds.saturating_sub(1);
        } else if nanos >= 1_000_000_000 {
            nanos -= 1_000_000_000;
            seconds = seconds.saturating_add(1);
        }
        assert!(
            (MIN_SECONDS..=MAX_SECONDS).contains(&seconds),
            "Timestamp arithmetic leaves the stored range"
        );
        Self {
            seconds,
            nanos: nanos as u32,
        }
    }
}

/// Calendar fields of a stored instant, as the old clock printed them.
///
/// `second` is `60` for a stored leap second while the date, hour and minute
/// stay those of the `:59` second it is carried on, which is what
/// `chrono::Timelike::second` plus `%S` produced (`NaiveTime` keeps the 59 and
/// the formatter adds the leaping second). `nanosecond` is the fraction inside
/// that second, always below one billion.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Calendar {
    /// Proleptic Gregorian year, in the instant's offset.
    pub year: i32,
    /// Month, `1..=12`.
    pub month: u8,
    /// Day of the month, `1..=31`.
    pub day: u8,
    /// Hour, `0..=23`.
    pub hour: u8,
    /// Minute, `0..=59`.
    pub minute: u8,
    /// Second, `0..=60`; `60` only for a stored leap second.
    pub second: u8,
    /// Fraction inside `second`, `0..1_000_000_000`.
    pub nanosecond: u32,
}

impl Calendar {
    /// Read `at`'s fields, carrying the leap second from a stored nanosecond
    /// field at or above one billion.
    fn of(at: OffsetDateTime, stored_nanos: u32) -> Self {
        let leap = stored_nanos >= 1_000_000_000;
        Self {
            year: at.year(),
            month: at.month() as u8,
            day: at.day(),
            hour: at.hour(),
            minute: at.minute(),
            second: at.second() + u8::from(leap),
            nanosecond: stored_nanos % 1_000_000_000,
        }
    }

    /// The date as `YYYY-MM-DD`, with the year's mandatory sign outside
    /// `0..=9999` (the `%Y` text of the old formatter).
    pub fn date(&self) -> String {
        let year = self.year_text();
        format!("{year}-{:02}-{:02}", self.month, self.day)
    }

    /// The month as `YYYY-MM`.
    pub fn month_key(&self) -> String {
        let year = self.year_text();
        format!("{year}-{:02}", self.month)
    }

    /// The year as `%Y` printed it: four zero-padded digits inside
    /// `0..=9999`, and otherwise the sign plus four digits (`-0044`,
    /// `+12345`).
    fn year_text(&self) -> String {
        if (0..10_000).contains(&self.year) {
            format!("{:04}", self.year)
        } else {
            format!("{:+05}", self.year)
        }
    }

    /// Whether the second is the leaping one.
    pub fn is_leap_second(&self) -> bool {
        self.second == 60
    }
}

impl From<OffsetDateTime> for Timestamp {
    fn from(at: OffsetDateTime) -> Self {
        let seconds = at.unix_timestamp();
        // `time` is wider than the stored range; the old type panicked on a
        // value outside it, so this fails at the same point.
        assert!(
            (MIN_SECONDS..=MAX_SECONDS).contains(&seconds),
            "OffsetDateTime is outside the stored range"
        );
        Self {
            seconds,
            nanos: at.nanosecond(),
        }
    }
}

impl From<SystemTime> for Timestamp {
    /// A system time before the epoch floors to the earlier second; one outside
    /// the stored range fails with a panic, as the old conversion did.
    fn from(system: SystemTime) -> Self {
        let (seconds, nanos) = match system.duration_since(SystemTime::UNIX_EPOCH) {
            Ok(duration) => {
                let seconds = i64::try_from(duration.as_secs()).unwrap_or(i64::MAX);
                (seconds, duration.subsec_nanos())
            }
            Err(error) => {
                let duration = error.duration();
                let seconds = i64::try_from(duration.as_secs()).unwrap_or(i64::MAX);
                if duration.subsec_nanos() == 0 {
                    (seconds.saturating_neg(), 0)
                } else {
                    (
                        seconds.saturating_neg().saturating_sub(1),
                        1_000_000_000 - duration.subsec_nanos(),
                    )
                }
            }
        };
        assert!(
            (MIN_SECONDS..=MAX_SECONDS).contains(&seconds),
            "SystemTime is outside the stored range"
        );
        Self { seconds, nanos }
    }
}

impl Add<Duration> for Timestamp {
    type Output = Self;

    fn add(self, duration: Duration) -> Self {
        self.offset_by(duration)
    }
}

impl Sub<Duration> for Timestamp {
    type Output = Self;

    fn sub(self, duration: Duration) -> Self {
        self.offset_by(-duration)
    }
}

impl Sub for Timestamp {
    type Output = Duration;

    /// The gap with the old type's leap-second accounting
    /// (`chrono::NaiveTime::signed_duration_since`).
    ///
    /// A leaping second counts as a real second of elapsed time when the two
    /// values differ in their second of the day, which is the correction the
    /// old subtraction applied. The rule is kept as it was, including the fact
    /// that it does not see a leap second across a day boundary.
    fn sub(self, other: Self) -> Duration {
        let mut seconds = self.seconds - other.seconds;
        let frac = i64::from(self.nanos) - i64::from(other.nanos);
        let self_second_of_day = self.seconds.rem_euclid(86_400);
        let other_second_of_day = other.seconds.rem_euclid(86_400);
        if self_second_of_day > other_second_of_day && other.nanos >= 1_000_000_000 {
            seconds += 1;
        } else if self_second_of_day < other_second_of_day && self.nanos >= 1_000_000_000 {
            seconds -= 1;
        }
        let seconds_from_frac = frac.div_euclid(1_000_000_000);
        let nanos = frac.rem_euclid(1_000_000_000) as i32;
        Duration::new(seconds + seconds_from_frac, nanos)
    }
}

impl fmt::Display for Timestamp {
    /// The storage and protocol text: RFC 3339 in UTC with `Z` and the
    /// shortest fraction that keeps every nanosecond.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        rfc3339::write(formatter, *self, Precision::Auto, true)
    }
}

impl FromStr for Timestamp {
    type Err = ParseError;

    /// The relaxed RFC 3339 grammar that `chrono`'s `FromStr` accepted, so a
    /// string a stored record or a caller already produced keeps parsing.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        rfc3339::parse_relaxed(text)
    }
}

impl Serialize for Timestamp {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Timestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct TimestampVisitor;

        impl Visitor<'_> for TimestampVisitor {
            type Value = Timestamp;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("an RFC 3339 date and time string")
            }

            fn visit_str<E>(self, text: &str) -> Result<Timestamp, E>
            where
                E: de::Error,
            {
                text.parse::<Timestamp>().map_err(E::custom)
            }
        }

        deserializer.deserialize_str(TimestampVisitor)
    }
}
