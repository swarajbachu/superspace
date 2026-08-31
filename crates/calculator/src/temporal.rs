use chrono::{
    DateTime, Datelike as _, Duration, LocalResult, NaiveDate, NaiveDateTime, TimeZone as _,
    Weekday,
};
use chrono_tz::Tz;
use thiserror::Error;

/// Calendar step used by date arithmetic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DateStep {
    /// Calendar days.
    Days(i64),
    /// Seven-day weeks.
    Weeks(i64),
    /// Weekdays excluding Saturday and Sunday.
    Workdays(i64),
}

/// Display unit for a parsed compound timespan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeSpanUnit {
    /// Seconds.
    Seconds,
    /// Minutes.
    Minutes,
    /// Hours.
    Hours,
    /// Days of exactly 24 hours.
    Days,
}

/// Pure date, workday, timespan, and IANA time-zone calculator.
#[derive(Clone, Copy, Debug, Default)]
pub struct TemporalCalculator;

impl TemporalCalculator {
    /// Add signed days, weeks, or workdays to an ISO date.
    ///
    /// # Errors
    ///
    /// Returns invalid-date or range-overflow failures.
    pub fn add(date: &str, step: DateStep) -> Result<NaiveDate, TemporalError> {
        let date = parse_date(date)?;
        match step {
            DateStep::Days(days) => date
                .checked_add_signed(Duration::days(days))
                .ok_or(TemporalError::OutOfRange),
            DateStep::Weeks(weeks) => date
                .checked_add_signed(Duration::weeks(weeks))
                .ok_or(TemporalError::OutOfRange),
            DateStep::Workdays(days) => add_workdays(date, days),
        }
    }

    /// Signed whole-day distance from the first ISO date to the second.
    ///
    /// # Errors
    ///
    /// Returns [`TemporalError::InvalidDate`] for malformed dates.
    pub fn days_between(first: &str, second: &str) -> Result<i64, TemporalError> {
        Ok((parse_date(second)? - parse_date(first)?).num_days())
    }

    /// Interpret a local date/time in one IANA zone and convert it to another.
    ///
    /// Ambiguous daylight-saving folds and nonexistent gap times fail closed instead of silently
    /// choosing an instant.
    ///
    /// # Errors
    ///
    /// Returns invalid date/time, zone, ambiguous-time, or nonexistent-time failures.
    pub fn convert_zone(local: &str, from: &str, to: &str) -> Result<DateTime<Tz>, TemporalError> {
        let local = NaiveDateTime::parse_from_str(local, "%Y-%m-%d %H:%M:%S")
            .or_else(|_| NaiveDateTime::parse_from_str(local, "%Y-%m-%d %H:%M"))
            .map_err(|_| TemporalError::InvalidDateTime)?;
        let from: Tz = from.parse().map_err(|_| TemporalError::InvalidZone)?;
        let to: Tz = to.parse().map_err(|_| TemporalError::InvalidZone)?;
        match from.from_local_datetime(&local) {
            LocalResult::Single(value) => Ok(value.with_timezone(&to)),
            LocalResult::Ambiguous(_, _) => Err(TemporalError::AmbiguousTime),
            LocalResult::None => Err(TemporalError::NonexistentTime),
        }
    }

    /// Parse a compound timespan such as `2d 3h 15m 10s` and return the requested unit.
    ///
    /// # Errors
    ///
    /// Returns malformed-span or numeric-overflow failures.
    pub fn timespan(input: &str, unit: TimeSpanUnit) -> Result<f64, TemporalError> {
        let mut total_seconds = 0_f64;
        let mut saw_value = false;
        for token in input.split_whitespace() {
            let split = token
                .find(|character: char| !character.is_ascii_digit() && character != '.')
                .ok_or(TemporalError::InvalidTimeSpan)?;
            let (number, suffix) = token.split_at(split);
            let value: f64 = number.parse().map_err(|_| TemporalError::InvalidTimeSpan)?;
            if !value.is_finite() || value < 0.0 {
                return Err(TemporalError::InvalidTimeSpan);
            }
            let multiplier = match suffix.to_ascii_lowercase().as_str() {
                "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
                "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
                "h" | "hr" | "hrs" | "hour" | "hours" => 3_600.0,
                "d" | "day" | "days" => 86_400.0,
                _ => return Err(TemporalError::InvalidTimeSpan),
            };
            total_seconds += value * multiplier;
            saw_value = true;
        }
        if !saw_value || !total_seconds.is_finite() {
            return Err(TemporalError::InvalidTimeSpan);
        }
        Ok(total_seconds
            / match unit {
                TimeSpanUnit::Seconds => 1.0,
                TimeSpanUnit::Minutes => 60.0,
                TimeSpanUnit::Hours => 3_600.0,
                TimeSpanUnit::Days => 86_400.0,
            })
    }
}

/// Temporal parsing, timezone, and range failures.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TemporalError {
    /// ISO calendar date is malformed or impossible.
    #[error("date is invalid")]
    InvalidDate,
    /// Local date/time is malformed.
    #[error("date and time are invalid")]
    InvalidDateTime,
    /// IANA timezone name is unknown.
    #[error("time zone is invalid")]
    InvalidZone,
    /// Local wall time occurs twice during a daylight-saving fold.
    #[error("local time is ambiguous because of daylight saving")]
    AmbiguousTime,
    /// Local wall time does not exist during a daylight-saving gap.
    #[error("local time does not exist because of daylight saving")]
    NonexistentTime,
    /// Compound timespan is malformed.
    #[error("timespan is invalid")]
    InvalidTimeSpan,
    /// Date arithmetic exceeded Chrono's supported range.
    #[error("date calculation is outside the supported range")]
    OutOfRange,
}

fn parse_date(value: &str) -> Result<NaiveDate, TemporalError> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").map_err(|_| TemporalError::InvalidDate)
}

fn add_workdays(mut date: NaiveDate, days: i64) -> Result<NaiveDate, TemporalError> {
    let direction = days.signum();
    let mut remaining = days.unsigned_abs();
    while remaining > 0 {
        date = date
            .checked_add_signed(Duration::days(direction))
            .ok_or(TemporalError::OutOfRange)?;
        if !matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
            remaining -= 1;
        }
    }
    Ok(date)
}

#[cfg(test)]
mod tests {
    use chrono::Timelike as _;

    use super::*;

    #[test]
    fn calendar_and_workday_arithmetic_cross_weekends() {
        assert_eq!(
            TemporalCalculator::add("2026-08-28", DateStep::Workdays(1))
                .expect("next workday")
                .to_string(),
            "2026-08-31"
        );
        assert_eq!(
            TemporalCalculator::add("2026-08-31", DateStep::Workdays(-1))
                .expect("previous workday")
                .to_string(),
            "2026-08-28"
        );
        assert_eq!(
            TemporalCalculator::days_between("2026-08-01", "2026-08-31").expect("difference"),
            30
        );
    }

    #[test]
    fn time_zone_conversion_observes_dst_and_rejects_gaps() {
        let converted = TemporalCalculator::convert_zone(
            "2026-07-01 09:00",
            "America/New_York",
            "Asia/Kolkata",
        )
        .expect("zone conversion");
        assert_eq!(converted.hour(), 18);
        assert_eq!(converted.minute(), 30);
        assert_eq!(
            TemporalCalculator::convert_zone(
                "2026-03-08 02:30",
                "America/New_York",
                "Asia/Kolkata"
            ),
            Err(TemporalError::NonexistentTime)
        );
    }

    #[test]
    fn compound_timespans_convert_to_requested_units() {
        let minutes =
            TemporalCalculator::timespan("2h 30m", TimeSpanUnit::Minutes).expect("minutes");
        assert!((minutes - 150.0).abs() < f64::EPSILON);
        let hours = TemporalCalculator::timespan("1d 12h", TimeSpanUnit::Hours).expect("hours");
        assert!((hours - 36.0).abs() < f64::EPSILON);
        assert!(TemporalCalculator::timespan("2 parsecs", TimeSpanUnit::Seconds).is_err());
    }
}
