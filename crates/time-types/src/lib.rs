//! Wall-clock instants for the stored session, protocol and logging contracts.
//!
//! [`Timestamp`] is the one persisted instant type. It round-trips the same
//! RFC 3339 text the storage and protocol layers already carry, keeps the
//! nanosecond field exact, and keeps the old `chrono::DateTime<Utc>` value range
//! and leap-second arithmetic; an operation that leaves the range fails at the
//! same points the old one failed.
//!
//! The rest of `time` is re-exported so a crate that handles an instant never
//! needs its own datetime dependency for the common surface: [`Duration`],
//! [`OffsetDateTime`], [`Date`], [`Time`], [`PrimitiveDateTime`], [`Month`],
//! [`Weekday`], [`UtcOffset`], `macros` and `format_description`. Local-time
//! formatting uses `time`'s compile-time `format_description!`; note that a
//! consumer of that macro still names the `time` crate directly, because the
//! expansion resolves `::time` paths.
//!
//! Reading has three entry points, matching what the old code called:
//! `FromStr` and serde read the relaxed RFC 3339 form the old `FromStr`
//! accepted (unpadded fields, whitespace between components, `UTC`, a
//! colon-less offset, signed years, trailing whitespace);
//! [`Timestamp::parse_rfc3339`] reads the strict form the old
//! `parse_from_rfc3339` accepted; and [`Timestamp::parse_naive_date`] plus
//! [`Timestamp::parse_naive_utc`] read the `%Y-%m-%d` and
//! `%Y-%m-%dT%H:%M:%S%.fZ` forms the old `NaiveDate`/`NaiveDateTime`
//! `parse_from_str` calls accepted.

mod naive;
mod rfc3339;
mod timestamp;

pub use crate::rfc3339::{ParseError, Precision};
pub use crate::timestamp::{Calendar, Timestamp};

pub use time::{
    Date, Duration, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset, Weekday, format_description, macros,
};

#[cfg(test)]
mod tests;
