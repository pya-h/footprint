use crate::TimePrecision;
use chrono::{DateTime, FixedOffset, Local, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

const WEBKIT_EPOCH_US: i64 = 11_644_473_600_000_000;
const DOTNET_EPOCH_US: i64 = 62_135_596_800_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeFormat {
    WebKitMicros,
    PRTime,
    UnixSeconds,
    UnixMillis,
    UnixNanos,
    JournaldRealtime,
    DotNetBinary,
    LocalNaive(String),
    Rfc3339,
    ZshExtended,
    BashHashEpoch,
    FileTime,
    None,
}

impl fmt::Display for TimeFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimeFormat::WebKitMicros => write!(f, "webkit_micros"),
            TimeFormat::PRTime => write!(f, "prtime"),
            TimeFormat::UnixSeconds => write!(f, "unix_seconds"),
            TimeFormat::UnixMillis => write!(f, "unix_millis"),
            TimeFormat::UnixNanos => write!(f, "unix_nanos"),
            TimeFormat::JournaldRealtime => write!(f, "journald_realtime"),
            TimeFormat::DotNetBinary => write!(f, "dotnet_binary"),
            TimeFormat::LocalNaive(fmt) => write!(f, "local_naive:{fmt}"),
            TimeFormat::Rfc3339 => write!(f, "rfc3339"),
            TimeFormat::ZshExtended => write!(f, "zsh_extended"),
            TimeFormat::BashHashEpoch => write!(f, "bash_hash_epoch"),
            TimeFormat::FileTime => write!(f, "file_time"),
            TimeFormat::None => write!(f, "none"),
        }
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum TimeError {
    #[error("invalid integer: {0}")]
    ParseInt(String),
    #[error("invalid RFC3339 timestamp: {0}")]
    ParseRfc3339(String),
    #[error("invalid local naive timestamp: {0}")]
    ParseLocal(String),
    #[error("unsupported time format")]
    Unsupported,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedTime {
    pub ts_utc: i64,
    pub precision: TimePrecision,
    pub tz_assumed: Option<String>,
}

pub fn normalize(
    raw: &str,
    format: &TimeFormat,
    assume_tz: Option<&str>,
) -> Result<NormalizedTime, TimeError> {
    match format {
        TimeFormat::WebKitMicros => {
            let us = parse_i64(raw)?;
            Ok(NormalizedTime {
                ts_utc: us - WEBKIT_EPOCH_US,
                precision: TimePrecision::Exact,
                tz_assumed: None,
            })
        }
        TimeFormat::PRTime | TimeFormat::JournaldRealtime => {
            let us = parse_i64(raw)?;
            Ok(NormalizedTime {
                ts_utc: us,
                precision: TimePrecision::Exact,
                tz_assumed: None,
            })
        }
        TimeFormat::UnixSeconds => {
            let s = parse_i64(raw)?;
            Ok(NormalizedTime {
                ts_utc: s * 1_000_000,
                precision: TimePrecision::Second,
                tz_assumed: None,
            })
        }
        TimeFormat::UnixMillis => {
            let ms = parse_i64(raw)?;
            Ok(NormalizedTime {
                ts_utc: ms * 1000,
                precision: TimePrecision::Exact,
                tz_assumed: None,
            })
        }
        TimeFormat::UnixNanos => {
            let ns = parse_i64(raw)?;
            Ok(NormalizedTime {
                ts_utc: ns / 1000,
                precision: TimePrecision::Exact,
                tz_assumed: None,
            })
        }
        TimeFormat::DotNetBinary => {
            let raw = parse_i64(raw)?;
            let ticks = raw & 0x3FFF_FFFF_FFFF_FFFF;
            let us = ticks / 10 - DOTNET_EPOCH_US;
            Ok(NormalizedTime {
                ts_utc: us,
                precision: TimePrecision::Exact,
                tz_assumed: None,
            })
        }
        TimeFormat::LocalNaive(fmt) => {
            let naive = NaiveDateTime::parse_from_str(raw, fmt)
                .map_err(|_| TimeError::ParseLocal(raw.to_string()))?;
            let tz = timezone_or_local(assume_tz)?;
            let dt = tz
                .from_local_datetime(&naive)
                .single()
                .ok_or_else(|| TimeError::ParseLocal(raw.to_string()))?;
            Ok(NormalizedTime {
                ts_utc: dt.timestamp_micros(),
                precision: TimePrecision::Second,
                tz_assumed: assume_tz.map(String::from),
            })
        }
        TimeFormat::Rfc3339 => {
            let dt = raw
                .parse::<DateTime<FixedOffset>>()
                .map_err(|_| TimeError::ParseRfc3339(raw.to_string()))?;
            Ok(NormalizedTime {
                ts_utc: dt.with_timezone(&Utc).timestamp_micros(),
                precision: TimePrecision::Exact,
                tz_assumed: None,
            })
        }
        TimeFormat::ZshExtended => parse_zsh_extended(raw),
        TimeFormat::BashHashEpoch => {
            let s = parse_i64(raw)?;
            Ok(NormalizedTime {
                ts_utc: s * 1_000_000,
                precision: TimePrecision::Second,
                tz_assumed: None,
            })
        }
        TimeFormat::FileTime => {
            let ns = parse_i64(raw)?;
            Ok(NormalizedTime {
                ts_utc: ns / 1000,
                precision: TimePrecision::Exact,
                tz_assumed: None,
            })
        }
        TimeFormat::None => Ok(NormalizedTime {
            ts_utc: 0,
            precision: TimePrecision::OrderOnly,
            tz_assumed: None,
        }),
    }
}

fn parse_i64(s: &str) -> Result<i64, TimeError> {
    s.trim()
        .parse::<i64>()
        .map_err(|_| TimeError::ParseInt(s.to_string()))
}

fn timezone_or_local(assume_tz: Option<&str>) -> Result<FixedOffset, TimeError> {
    match assume_tz {
        Some(tz) => parse_offset(tz),
        None => Ok(*Local::now().offset()),
    }
}

fn parse_offset(tz: &str) -> Result<FixedOffset, TimeError> {
    tz.parse::<FixedOffset>()
        .map_err(|_| TimeError::ParseLocal(format!("bad tz offset: {tz}")))
}

fn parse_zsh_extended(raw: &str) -> Result<NormalizedTime, TimeError> {
    let start = raw
        .find(':')
        .map(|i| i + 1)
        .ok_or_else(|| TimeError::ParseLocal(raw.to_string()))?;
    let rest = raw[start..].trim_start();
    let (ts_part, _) = rest.split_once(':').unwrap_or((rest, ""));
    let s = parse_i64(ts_part)?;
    Ok(NormalizedTime {
        ts_utc: s * 1_000_000,
        precision: TimePrecision::Second,
        tz_assumed: None,
    })
}

pub fn format_utc(ts_utc: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(ts_utc).unwrap_or_else(|| Utc::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_utc(raw: &str, format: TimeFormat, expected: &str) {
        let got = normalize(raw, &format, None).unwrap();
        let dt = format_utc(got.ts_utc);
        assert_eq!(dt.to_rfc3339(), expected);
    }

    #[test]
    fn webkit_micros_golden() {
        assert_utc(
            "13436646517268392",
            TimeFormat::WebKitMicros,
            "2026-10-16T17:48:37.268392+00:00",
        );
    }

    #[test]
    fn dotnet_binary_golden() {
        assert_utc(
            "-8584094347052926203",
            TimeFormat::DotNetBinary,
            "2026-10-16T17:36:20.184960+00:00",
        );
    }

    #[test]
    fn unix_seconds_golden() {
        assert_utc(
            "1792172917",
            TimeFormat::UnixSeconds,
            "2026-10-16T17:48:37+00:00",
        );
    }

    #[test]
    fn local_naive_assumes_host_tz() {
        let got = normalize("2026-10-16 21:08:00", &TimeFormat::LocalNaive("%Y-%m-%d %H:%M:%S".into()), Some("+03:30")).unwrap();
        let dt = format_utc(got.ts_utc);
        assert_eq!(dt.to_rfc3339(), "2026-10-16T17:38:00+00:00");
        assert_eq!(got.precision, TimePrecision::Second);
    }

    #[test]
    fn zsh_extended() {
        let got = normalize(": 1792172917:0;ls", &TimeFormat::ZshExtended, None).unwrap();
        let dt = format_utc(got.ts_utc);
        assert_eq!(dt.to_rfc3339(), "2026-10-16T17:48:37+00:00");
    }

    #[test]
    fn rfc3339() {
        assert_utc(
            "2026-10-16T17:48:37.268392Z",
            TimeFormat::Rfc3339,
            "2026-10-16T17:48:37.268392+00:00",
        );
    }

    #[test]
    fn file_time_ns() {
        assert_utc(
            "1792172917000000000",
            TimeFormat::FileTime,
            "2026-10-16T17:48:37+00:00",
        );
    }

    #[test]
    fn none_is_order_only() {
        let got = normalize("anything", &TimeFormat::None, None).unwrap();
        assert_eq!(got.precision, TimePrecision::OrderOnly);
        assert_eq!(got.ts_utc, 0);
    }
}
