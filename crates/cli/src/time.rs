use anyhow::{anyhow, Result};
use chrono::{Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};

pub fn parse(input: &str) -> Result<i64> {
    if let Some(ts) = parse_at(input) {
        return Ok(ts * 1_000_000);
    }
    if let Some(ts) = parse_relative(input) {
        return Ok(ts);
    }
    if input == "yesterday" {
        let start = Local::now()
            .date_naive()
            .pred_opt()
            .unwrap()
            .and_time(NaiveTime::MIN);
        return Ok(local_to_utc_us(&start));
    }
    let dt = parse_datetime(input)?;
    Ok(local_to_utc_us(&dt))
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
        "s" | "sec" | "secs" => Duration::seconds(n),
        "m" | "min" | "mins" => Duration::minutes(n),
        "h" | "hr" | "hrs" => Duration::hours(n),
        "d" | "day" | "days" => Duration::days(n),
        "w" | "week" | "weeks" => Duration::weeks(n),
        _ => return None,
    };
    let target = if sign == '-' {
        Local::now() - dur
    } else {
        Local::now() + dur
    };
    Some(target.timestamp_micros())
}

fn parse_datetime(input: &str) -> Result<NaiveDateTime> {
    if let Ok(dt) = NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M") {
        return Ok(dt);
    }
    if let Ok(dt) = NaiveDateTime::parse_from_str(input, "%Y-%m-%dT%H:%M:%S") {
        return Ok(dt);
    }
    if let Ok(t) = NaiveTime::parse_from_str(input, "%H:%M") {
        let today = Local::now().date_naive();
        return Ok(today.and_time(t));
    }
    if let Ok(t) = NaiveTime::parse_from_str(input, "%H:%M:%S") {
        let today = Local::now().date_naive();
        return Ok(today.and_time(t));
    }
    if let Ok(d) = NaiveDate::parse_from_str(input, "%Y-%m-%d") {
        return Ok(d.and_time(NaiveTime::MIN));
    }
    Err(anyhow!("unrecognized time: {input}"))
}

fn local_to_utc_us(naive: &NaiveDateTime) -> i64 {
    Local
        .from_local_datetime(naive)
        .single()
        .map(|dt| dt.timestamp_micros())
        .unwrap_or_else(|| Utc::now().timestamp_micros())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_epoch() {
        let got = parse("@1792172917").unwrap();
        assert_eq!(got, 1792172917_000_000);
    }

    #[test]
    fn iso_minutes() {
        let got = parse("2026-10-16T20:40").unwrap();
        let expected = Utc.with_ymd_and_hms(2026, 10, 16, 17, 10, 0).unwrap().timestamp_micros();
        assert_eq!(got, expected);
    }

    #[test]
    fn relative_hours() {
        let before = parse("-1h").unwrap();
        let now = Utc::now().timestamp_micros();
        assert!(before < now);
        assert!(now - before < 4_000_000_000);
    }
}
