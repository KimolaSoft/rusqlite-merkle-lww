//! RFC 3339 timestamps at fixed microsecond precision, and the monotonic
//! issuance guard.
//!
//! **Timestamps are microseconds, always.** The canonical form is RFC 3339
//! with exactly six fractional digits, in UTC:
//!
//! ```text
//! 2026-11-12T10:30:00.123456Z
//! ```
//!
//! Two properties of that form are load-bearing, and everything else follows
//! from them:
//!
//! 1. **The lexicographic order of two formatted timestamps is their
//!    chronological order.** The Merkle buckets are ordered by timestamp, and a
//!    format whose sort order is not chronological would silently misplace
//!    rows. Fixed width, fixed offset, UTC — and the property holds.
//! 2. **A timestamp is a fixed number of bytes.** It is what goes in the
//!    digest, so it cannot vary in length.
//!
//! The canonical range is years `0000` to `9999`. Outside it the year grows
//! past four digits and the lexicographic property no longer holds, so a
//! deployment that needs instants beyond that should shift its `epoch_us`
//! rather than its timestamps.

use crate::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};

/// Microseconds in a second.
pub const MICROS_PER_SEC: i64 = 1_000_000;

/// Microseconds in a day.
pub const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SEC;

/// The lowest year the canonical form can represent.
pub const MIN_YEAR: i64 = 0;

/// The highest year the canonical form can represent.
pub const MAX_YEAR: i64 = 9999;

/// Format a Unix microsecond instant as RFC 3339, in UTC, with exactly six
/// fractional digits.
///
/// The result is always the same width for a given year, and sorts
/// lexicographically in chronological order.
pub fn format_rfc3339_micros(us: i64) -> String {
    let seconds = us.div_euclid(MICROS_PER_SEC);
    let micros = us.rem_euclid(MICROS_PER_SEC);

    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);

    let (year, month, day) = civil_from_days(days);
    let hour = second_of_day / 3600;
    let minute = (second_of_day % 3600) / 60;
    let second = second_of_day % 60;

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{micros:06}Z")
}

/// Parse an RFC 3339 timestamp into Unix microseconds.
///
/// Accepted:
///
/// - `YYYY-MM-DDTHH:MM:SS`, with `T`, `t` or a space as the separator;
/// - an optional fractional part of one to nine digits, truncated to
///   microseconds;
/// - a zone of `Z`, `z`, or `±HH:MM`.
///
/// Rejected: anything else, including a second of `60` — a leap second has no
/// place in a fixed-width, monotonic timestamp, and the crate never emits one.
pub fn parse_rfc3339(input: &str) -> Result<i64, Error> {
    let bad = |reason: &str| Error::Timestamp(format!("{input:?}: {reason}"));

    let bytes = input.as_bytes();
    if bytes.len() < 20 {
        return Err(bad("too short to be an RFC 3339 timestamp"));
    }

    let year = digits(&input[0..4], 4).ok_or_else(|| bad("year is not four digits"))?;
    if bytes[4] != b'-' {
        return Err(bad("expected '-' after the year"));
    }
    let month = digits_u32(&input[5..7], 2).ok_or_else(|| bad("month is not two digits"))?;
    if bytes[7] != b'-' {
        return Err(bad("expected '-' after the month"));
    }
    let day = digits_u32(&input[8..10], 2).ok_or_else(|| bad("day is not two digits"))?;
    if !matches!(bytes[10], b'T' | b't' | b' ') {
        return Err(bad("expected 'T' between the date and the time"));
    }
    let hour = digits_u32(&input[11..13], 2).ok_or_else(|| bad("hour is not two digits"))?;
    if bytes[13] != b':' {
        return Err(bad("expected ':' after the hour"));
    }
    let minute = digits_u32(&input[14..16], 2).ok_or_else(|| bad("minute is not two digits"))?;
    if bytes[16] != b':' {
        return Err(bad("expected ':' after the minute"));
    }
    let second = digits_u32(&input[17..19], 2).ok_or_else(|| bad("second is not two digits"))?;

    let mut cursor = 19;
    let mut fraction_micros: i64 = 0;
    if cursor < bytes.len() && bytes[cursor] == b'.' {
        cursor += 1;
        let start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        let digits = &input[start..cursor];
        if digits.is_empty() {
            return Err(bad("the fractional part has no digits"));
        }
        if digits.len() > 9 {
            return Err(bad("the fractional part has more than nine digits"));
        }
        // Truncate to microseconds. Anything finer is not representable and is
        // not significant at this resolution.
        let mut scaled = String::with_capacity(6);
        scaled.push_str(&digits[..digits.len().min(6)]);
        while scaled.len() < 6 {
            scaled.push('0');
        }
        fraction_micros = scaled
            .parse::<i64>()
            .map_err(|_| bad("the fractional part is not a number"))?;
    }

    let offset_seconds: i64 = match bytes.get(cursor) {
        None => return Err(bad("no time zone")),
        Some(b'Z') | Some(b'z') => {
            cursor += 1;
            0
        }
        Some(sign @ (b'+' | b'-')) => {
            let sign = if *sign == b'-' { -1 } else { 1 };
            if cursor + 6 > bytes.len() {
                return Err(bad("the time zone offset is truncated"));
            }
            let off_hour = digits_u32(&input[cursor + 1..cursor + 3], 2)
                .ok_or_else(|| bad("the offset hour is not two digits"))?;
            if bytes[cursor + 3] != b':' {
                return Err(bad("expected ':' in the time zone offset"));
            }
            let off_minute = digits_u32(&input[cursor + 4..cursor + 6], 2)
                .ok_or_else(|| bad("the offset minute is not two digits"))?;
            if off_hour > 23 || off_minute > 59 {
                return Err(bad("the time zone offset is out of range"));
            }
            cursor += 6;
            sign * (off_hour as i64 * 3600 + off_minute as i64 * 60)
        }
        Some(_) => return Err(bad("the time zone is not 'Z' or an offset")),
    };

    if cursor != bytes.len() {
        return Err(bad("there is trailing text after the timestamp"));
    }

    if !(MIN_YEAR..=MAX_YEAR).contains(&year) {
        return Err(bad("the year is outside the canonical range"));
    }
    if !(1..=12).contains(&month) {
        return Err(bad("the month is out of range"));
    }
    if day == 0 || day > days_in_month(year, month) {
        return Err(bad("the day is out of range for that month"));
    }
    if hour > 23 {
        return Err(bad("the hour is out of range"));
    }
    if minute > 59 {
        return Err(bad("the minute is out of range"));
    }
    if second > 59 {
        return Err(bad("the second is out of range"));
    }

    let days = days_from_civil(year, month, day);
    let seconds =
        days * 86_400 + hour as i64 * 3600 + minute as i64 * 60 + second as i64 - offset_seconds;

    Ok(seconds * MICROS_PER_SEC + fraction_micros)
}

/// Whether a year is a leap year in the proleptic Gregorian calendar.
pub const fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// The number of days in a month.
pub const fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// The number of days since 1970-01-01 for a civil date.
///
/// This is the standard proleptic Gregorian conversion, with floor division
/// throughout so that dates before the epoch land on the right day.
const fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let year_of_era = y - era * 400; // [0, 399]
    let mp = ((month + 9) % 12) as i64; // March = 0
    let day_of_year = (153 * mp + 2) / 5 + day as i64 - 1; // [0, 365]
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year; // [0, 146096]
    era * 146_097 + day_of_era - 719_468
}

/// The civil date for a number of days since 1970-01-01.
const fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// Read exactly `n` ASCII digits as a number.
fn digits(text: &str, n: usize) -> Option<i64> {
    if text.len() != n || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<i64>().ok()
}

/// Read exactly `n` ASCII digits as a number.
fn digits_u32(text: &str, n: usize) -> Option<u32> {
    if text.len() != n || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u32>().ok()
}

/// A guard that hands out timestamps which never repeat and never go backwards.
///
/// Two writes in the same microsecond still have to order deterministically, so
/// the guard issues a timestamp **strictly greater** than the last one it
/// issued. It is the reason the crate can promise that `last_update` is
/// strictly increasing within a node.
///
/// The guard is deliberately **not** a clock: it never moves time forward on
/// its own. It only ever refuses to issue a timestamp that would tie or
/// regress.
#[derive(Debug)]
pub struct MonotonicGuard {
    last: AtomicI64,
}

impl MonotonicGuard {
    /// A fresh guard, with nothing issued yet.
    pub const fn new() -> Self {
        MonotonicGuard {
            last: AtomicI64::new(i64::MIN),
        }
    }

    /// The process-wide guard.
    ///
    /// This is the one the crate uses, so that a timestamp is unique across the
    /// whole process and not merely within one connection or one table.
    pub fn global() -> &'static MonotonicGuard {
        static GLOBAL: MonotonicGuard = MonotonicGuard::new();
        &GLOBAL
    }

    /// Issue a timestamp that is strictly greater than every timestamp this
    /// guard has issued, and no earlier than `now_us`.
    ///
    /// Returns `now_us` when the clock has moved on. When it has not, it
    /// returns the last issued timestamp plus one microsecond.
    ///
    /// The result saturates at `i64::MAX`: a process that issues more than
    /// `i64::MAX` distinct timestamps has other problems, and returning the
    /// maximum is better than wrapping into the past.
    pub fn issue(&self, now_us: i64) -> i64 {
        let mut last = self.last.load(Ordering::SeqCst);
        loop {
            let next = now_us.max(last.saturating_add(1));
            match self
                .last
                .compare_exchange(last, next, Ordering::SeqCst, Ordering::SeqCst)
            {
                Ok(_) => return next,
                Err(observed) => last = observed,
            }
        }
    }

    /// The last timestamp this guard issued, if any.
    pub fn last_issued(&self) -> Option<i64> {
        let last = self.last.load(Ordering::SeqCst);
        (last != i64::MIN).then_some(last)
    }
}

impl Default for MonotonicGuard {
    fn default() -> Self {
        MonotonicGuard::new()
    }
}

/// Issue a timestamp from the process-wide guard.
///
/// See [`MonotonicGuard::issue`].
pub fn issue_monotonic(now_us: i64) -> i64 {
    MonotonicGuard::global().issue(now_us)
}

/// The current wall clock, in Unix microseconds.
///
/// The crate has no clock dependency, so this reads the system clock directly.
/// A test that needs to control time uses its own clock instead.
pub fn now_us() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_micros() as i64,
        // Before the epoch. The sign is right; the magnitude is the system's.
        Err(e) => -(e.duration().as_micros() as i64),
    }
}

/// A timestamp, rendered for a human.
///
/// The canonical form is the wire form, so this is only for logs and error
/// messages.
pub struct TimestampDisplay(pub i64);

impl fmt::Display for TimestampDisplay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format_rfc3339_micros(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_canonical_form_is_fixed_width() {
        assert_eq!(
            format_rfc3339_micros(1_735_689_600_000_000),
            "2025-01-01T00:00:00.000000Z"
        );
        assert_eq!(
            format_rfc3339_micros(1_735_689_600_123_456),
            "2025-01-01T00:00:00.123456Z"
        );
    }

    #[test]
    fn a_timestamp_round_trips() {
        for us in [
            0,
            1,
            1_000_000,
            1_735_689_600_000_000,
            1_735_689_600_123_456,
            // A leap day.
            1_709_164_800_000_000,
            // Just before the epoch.
            -1,
            -1_000_000,
        ] {
            let text = format_rfc3339_micros(us);
            assert_eq!(parse_rfc3339(&text).unwrap(), us, "{text}");
        }
    }

    #[test]
    fn lexicographic_order_is_chronological_order() {
        let mut instants = [
            0_i64,
            1,
            -1,
            1_000_000,
            1_735_689_600_000_000,
            1_735_689_600_123_456,
            1_709_164_800_000_000,
            -86_400_000_000,
        ];
        instants.sort_unstable();

        let formatted: Vec<String> = instants.iter().map(|u| format_rfc3339_micros(*u)).collect();
        let mut sorted = formatted.clone();
        sorted.sort();

        assert_eq!(formatted, sorted);
    }

    #[test]
    fn an_offset_is_converted_to_utc() {
        // 2025-01-01T01:00:00+01:00 is 2025-01-01T00:00:00Z.
        assert_eq!(
            parse_rfc3339("2025-01-01T01:00:00+01:00").unwrap(),
            parse_rfc3339("2025-01-01T00:00:00Z").unwrap()
        );
        // And a negative offset moves the other way.
        assert_eq!(
            parse_rfc3339("2024-12-31T23:00:00-01:00").unwrap(),
            parse_rfc3339("2025-01-01T00:00:00Z").unwrap()
        );
    }

    #[test]
    fn a_fraction_is_truncated_to_microseconds() {
        assert_eq!(
            parse_rfc3339("2025-01-01T00:00:00.123456789Z").unwrap(),
            parse_rfc3339("2025-01-01T00:00:00.123456Z").unwrap()
        );
        assert_eq!(
            parse_rfc3339("2025-01-01T00:00:00.5Z").unwrap(),
            parse_rfc3339("2025-01-01T00:00:00.500000Z").unwrap()
        );
    }

    #[test]
    fn a_malformed_timestamp_is_rejected() {
        for bad in [
            "",
            "not a timestamp",
            "2025-01-01",
            "2025-13-01T00:00:00Z",
            "2025-02-30T00:00:00Z",
            "2025-01-01T24:00:00Z",
            "2025-01-01T00:00:60Z",
            "2025-01-01T00:00:00",
            "2025-01-01T00:00:00Z extra",
            "2025-01-01T00:00:00.Z",
            "2025-01-01T00:00:00+1:00",
        ] {
            assert!(parse_rfc3339(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn a_leap_day_is_accepted_and_a_common_day_is_not() {
        assert!(parse_rfc3339("2024-02-29T00:00:00Z").is_ok());
        assert!(parse_rfc3339("2025-02-29T00:00:00Z").is_err());
        assert!(parse_rfc3339("2000-02-29T00:00:00Z").is_ok());
        assert!(parse_rfc3339("1900-02-29T00:00:00Z").is_err());
    }

    #[test]
    fn the_guard_never_repeats_or_regresses() {
        let guard = MonotonicGuard::new();
        let mut previous = i64::MIN;
        for _ in 0..1000 {
            let issued = guard.issue(1_000);
            assert!(issued > previous);
            previous = issued;
        }
        // The clock stood still, so the guard walked the timestamps forward one
        // microsecond at a time from the first one it issued.
        assert_eq!(previous, 1_000 + 999);
    }

    #[test]
    fn the_guard_follows_a_clock_that_moves_forward() {
        let guard = MonotonicGuard::new();
        assert_eq!(guard.issue(100), 100);
        assert_eq!(guard.issue(200), 200);
        // A clock that goes backwards does not take the timestamp with it.
        assert_eq!(guard.issue(150), 201);
    }

    #[test]
    fn the_process_wide_guard_is_shared() {
        let first = issue_monotonic(1_000);
        let second = issue_monotonic(1_000);
        assert!(second > first);
    }
}
