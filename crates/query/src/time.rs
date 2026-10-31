use anyhow::{Context, Result, anyhow};
use chrono::{Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};

use crate::Tz;

pub fn parse(input: &str) -> Result<i64> {
    parse_inner(input, None)
}

/// Like [`parse`], but absolute naive datetimes resolve in `assume_tz`
/// (from `--assume-tz`) instead of the system zone; relative forms and
/// `@epoch` are anchored to the real now and stay unchanged.
pub fn parse_in(input: &str, assume_tz: Option<Tz>) -> Result<i64> {
    parse_inner(input, assume_tz)
}

fn parse_inner(input: &str, assume_tz: Option<Tz>) -> Result<i64> {
    if let Some(ts) = parse_at(input) {
        return ts
            .checked_mul(1_000_000)
            .ok_or_else(|| anyhow!("time out of range: {input}"));
    }
    if let Some(ts) = parse_relative(input) {
        return Ok(ts);
    }
    let zone = match assume_tz {
        Some(Tz::Fixed(offset)) => Some(offset),
        Some(Tz::Utc) => chrono::FixedOffset::east_opt(0),
        Some(Tz::Local) | None => None,
    };
    let today = match zone {
        Some(offset) => chrono::Utc::now().with_timezone(&offset).date_naive(),
        None => Local::now().date_naive(),
    };
    if input == "yesterday" {
        let start = today
            .pred_opt()
            .context("no day before today")?
            .and_time(NaiveTime::MIN);
        return naive_to_utc_us(&start, zone.as_ref());
    }
    naive_to_utc_us(&parse_datetime(input, today)?, zone.as_ref())
}

fn parse_at(input: &str) -> Option<i64> {
    let num = input.strip_prefix('@')?;
    num.parse::<i64>().ok()
}

fn parse_relative(input: &str) -> Option<i64> {
    let sign = input.chars().next()?;
    if sign != '-' && sign != '+' {
        return None;
    }
    let rest = &input[1..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let unit = &rest[digits.len()..];
    let n: i64 = digits.parse().ok()?;
    let dur = match unit {
        "s" | "sec" | "secs" => Duration::try_seconds(n)?,
        "m" | "min" | "mins" => Duration::try_minutes(n)?,
        "h" | "hr" | "hrs" => Duration::try_hours(n)?,
        "d" | "day" | "days" => Duration::try_days(n)?,
        "w" | "week" | "weeks" => Duration::try_weeks(n)?,
        _ => return None,
    };
    let target = if sign == '-' {
        Local::now().checked_sub_signed(dur)
    } else {
        Local::now().checked_add_signed(dur)
    };
    Some(target?.timestamp_micros())
}

fn parse_datetime(input: &str, today: NaiveDate) -> Result<NaiveDateTime> {
    if let Ok(dt) = NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M") {
        return Ok(dt);
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S") {
        return Ok(dt);
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(input, "%Y-%m-%d %H:%M") {
        return Ok(dt);
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(input, "%Y-%m-%d %H:%M:%S") {
        return Ok(dt);
    }
    if let Ok(t) = NaiveTime::parse_from_str(input, "%H:%M") {
        return Ok(today.and_time(t));
    }
    if let Ok(t) = NaiveTime::parse_from_str(input, "%H:%M:%S") {
        return Ok(today.and_time(t));
    }
    if let Ok(d) = NaiveDate::parse_from_str(input, "%Y-%m-%d") {
        return Ok(d.and_time(NaiveTime::MIN));
    }
    Err(anyhow!("unrecognized time: {input}"))
}

fn naive_to_utc_us(naive: &NaiveDateTime, zone: Option<&chrono::FixedOffset>) -> Result<i64> {
    match zone {
        Some(offset) => offset
            .from_local_datetime(naive)
            .earliest()
            .map(|dt| dt.timestamp_micros())
            .ok_or_else(|| anyhow!("{naive} does not exist in {offset}")),
        None => Local
            .from_local_datetime(naive)
            .earliest()
            .map(|dt| dt.timestamp_micros())
            .ok_or_else(|| anyhow!("{naive} does not exist in the local time zone")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_epoch() {
        let got = parse("@1792172917").unwrap();
        assert_eq!(got, 1_792_172_917_000_000);
    }

    #[test]
    fn iso_minutes_is_local_time() {
        let got = parse("2026-10-16T20:40").unwrap();
        let expected = Local
            .with_ymd_and_hms(2026, 10, 16, 20, 40, 0)
            .earliest()
            .unwrap()
            .timestamp_micros();
        assert_eq!(got, expected);
    }

    #[test]
    fn naive_time_uses_assumed_zone() {
        let tz = Tz::Fixed(chrono::FixedOffset::east_opt(3 * 3600 + 1800).unwrap());
        let got = parse_in("2026-10-16T20:00", Some(tz)).unwrap();
        // 20:00 at +03:30 is 16:30 UTC
        let expected = chrono::DateTime::parse_from_rfc3339("2026-10-16T16:30:00Z")
            .unwrap()
            .timestamp_micros();
        assert_eq!(got, expected);
    }

    #[test]
    fn relative_time_ignores_assumed_zone() {
        let tz = Tz::Fixed(chrono::FixedOffset::east_opt(3 * 3600 + 1800).unwrap());
        let a = parse_in("-1h", Some(tz)).unwrap();
        let b = parse("-1h").unwrap();
        assert!((a - b).abs() < 5_000_000);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("soon").is_err());
        assert!(parse("-5x").is_err());
    }

    #[test]
    fn relative_hours() {
        let before = parse("-1h").unwrap();
        let now = chrono::Utc::now().timestamp_micros();
        assert!(before < now);
        assert!(now - before < 4_000_000_000);
    }
}
