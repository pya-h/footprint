pub mod csv;
pub mod html;
pub mod jsonl;
pub mod redact;

pub use csv::render_csv;
pub use html::render_html;
pub use jsonl::render_jsonl;
pub use redact::{hash_secret, redact_json, redact_text, redact_url};

use std::os::unix::fs::PermissionsExt;

/// Directional overrides and isolates reorder what follows them: `evil\u{202e}fdp.exe`
/// displays as `evilexe.pdf`.
fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Evidence is untrusted: strip control and ANSI escape characters
/// before printing to a terminal. Printable text, newline and tab survive.
pub fn sanitize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // skip an ANSI escape: CSI … final-byte, or a two-char sequence
            let mut rest = chars.clone();
            match rest.next() {
                Some('[') => {
                    for c2 in rest.by_ref() {
                        if (0x40..=0x7e).contains(&(c2 as u32)) {
                            break;
                        }
                    }
                    chars = rest;
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            }
            continue;
        }
        match c {
            '\n' => out.push('\n'),
            '\t' => out.push('\t'),
            c if c.is_control() || is_bidi_control(c) => continue,
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod sanitize_tests {
    use super::sanitize;

    #[test]
    fn strips_ansi_and_control_chars() {
        assert_eq!(sanitize("\x1b[31mred\x1b[0m plain"), "red plain");
        assert_eq!(sanitize("beep\x07bell"), "beepbell");
        assert_eq!(sanitize("line\nbreak\ttab"), "line\nbreak\ttab");
        assert_eq!(sanitize("zero\x00width"), "zerowidth");
        assert_eq!("plain", sanitize("plain"));
    }
}

use anyhow::{Context, Result};
use fp_correlate::{IocHit, check_iocs, match_event_iocs};
use fp_store::{EventFilter, RunRow, Store};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::BufWriter;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportEvent {
    pub id: u64,
    pub run_id: u64,
    pub ts_utc: i64,
    pub ts_local: String,
    pub source_kind: String,
    pub app: Option<String>,
    pub profile: Option<String>,
    pub kind: String,
    pub host: Option<String>,
    pub subject: Option<String>,
    pub summary: Option<String>,
    pub attrs: Option<serde_json::Value>,
    pub locator: Option<String>,
    pub prov_file: Option<String>,
    pub prov_sha256: Option<String>,
    pub confidence: String,
    pub ioc_hits: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportReport {
    pub case_path: String,
    /// the zone `ts_local` is rendered in (`local`, `UTC`, `+03:30`)
    pub zone: String,
    pub runs: Vec<RunRow>,
    pub ioc_hits: Vec<IocHit>,
    pub events: Vec<ExportEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Jsonl,
    Csv,
    Html,
}

impl std::str::FromStr for ExportFormat {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "jsonl" => Ok(ExportFormat::Jsonl),
            "csv" => Ok(ExportFormat::Csv),
            "html" => Ok(ExportFormat::Html),
            other => anyhow::bail!("unknown export format: {other} (expected jsonl, csv, or html)"),
        }
    }
}

/// How an export renders times: the zone's name for headers, and the instant in that zone.
pub struct ExportTime<'a> {
    pub zone: &'a str,
    pub format: &'a dyn Fn(i64, &str) -> String,
}

pub fn export_case(
    store: &Store,
    filter: &EventFilter,
    format: ExportFormat,
    out_path: &Path,
    redact: bool,
    time: &ExportTime,
) -> Result<usize> {
    let rows = store.list_events_detailed(filter)?;

    let iocs = store.list_iocs()?;

    let mut events = Vec::with_capacity(rows.len());
    for r in rows {
        let ts_local = (time.format)(r.ts_utc, &r.ts_precision);
        let ioc_hits = match_event_iocs(
            r.subject_host.as_deref(),
            r.subject.as_deref(),
            r.summary.as_deref(),
            &iocs,
        );

        let subject = if redact {
            r.subject.map(|s| redact_text(&s))
        } else {
            r.subject
        };

        let summary = if redact {
            r.summary.map(|s| redact_text(&s))
        } else {
            r.summary
        };

        let attrs = r
            .attrs_json
            .as_deref()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
            .map(|val| if redact { redact_json(&val) } else { val });

        events.push(ExportEvent {
            id: r.id,
            run_id: r.run_id,
            ts_utc: r.ts_utc,
            ts_local,
            source_kind: r.source_kind,
            app: r.app,
            profile: r.profile,
            kind: r.kind,
            host: r.subject_host,
            subject,
            summary,
            attrs,
            locator: r.locator,
            prov_file: r.prov_file,
            prov_sha256: r.prov_sha256,
            confidence: r.confidence,
            ioc_hits,
        });
    }

    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(out_path)
        .with_context(|| format!("failed to create export file {}", out_path.display()))?;
    // `mode` applies at create only; overwriting a permissive file must not keep its mode
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to chmod export file {}", out_path.display()))?;

    let mut writer = BufWriter::new(file);

    match format {
        ExportFormat::Jsonl => {
            render_jsonl(&mut writer, &events)?;
        }
        ExportFormat::Csv => {
            render_csv(&mut writer, &events)?;
        }
        ExportFormat::Html => {
            let mut ioc_hits = check_iocs(store)?;
            if redact {
                for hit in &mut ioc_hits {
                    hit.target_desc = redact_text(&hit.target_desc);
                    hit.matched_value = redact_text(&hit.matched_value);
                }
            }
            let runs = store.list_runs()?;
            let report = ExportReport {
                case_path: store.path().display().to_string(),
                zone: time.zone.to_string(),
                runs,
                ioc_hits,
                events: events.clone(),
            };
            render_html(&mut writer, &report)?;
        }
    }

    Ok(events.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_escapes_and_bidi_overrides() {
        assert_eq!(sanitize("a\u{1b}[31mred\u{1b}[0m\u{7}b"), "aredb");
        assert_eq!(sanitize("evil\u{202e}fdp.exe"), "evilfdp.exe");
        assert_eq!(sanitize("line\nnext\ttab"), "line\nnext\ttab");
    }

    #[test]
    fn csv_cells_never_start_a_formula() {
        assert_eq!(
            csv::escape_csv("=HYPERLINK(\"x\")"),
            "\"'=HYPERLINK(\"\"x\"\")\""
        );
        assert_eq!(csv::escape_csv("-cmd"), "'-cmd");
        assert_eq!(csv::escape_csv("plain"), "plain");
    }
    use chrono::DateTime;
    use fp_core::{
        Actor, Confidence, Event, EventKind, Provenance, SourceInstance, TimePrecision,
        time::TimeFormat,
    };
    use fp_store::{Batch, NewRun};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    #[test]
    fn test_export_case_all_formats_and_redact() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("case.db");
        let mut store = Store::open_or_create(&db_path).unwrap();

        let run_id = store
            .insert_run(&NewRun {
                host: "host1",
                user: "user1",
                uid: 1000,
                euid: 1000,
                version: "0.1.0",
                tz: "UTC",
                args: "",
            })
            .unwrap();

        let inst = SourceInstance::new(
            "chromium.history",
            "brave",
            Some("Default".into()),
            PathBuf::from("/home/u/.config/Brave"),
        );
        let inst_id = store.insert_source(run_id, &inst).unwrap();

        let ev = Event {
            run_id,
            source_kind: "chromium.history".into(),
            ts_utc: 1000,
            ts_precision: TimePrecision::Second,
            ts_raw: "1000".into(),
            ts_raw_format: TimeFormat::UnixSeconds,
            ts_tz_assumed: None,
            source_inst: inst_id,
            kind: EventKind::WebVisit,
            actor: Some(Actor::app("Brave")),
            subject: "https://zoom.us/download?token=supersecret123".into(),
            subject_host: Some("zoom.us".into()),
            summary: "visit zoom.us with token".into(),
            attrs: serde_json::json!({"url": "https://zoom.us/download?token=supersecret123"}),
            provenance: Provenance {
                file: PathBuf::from("/tmp/History"),
                file_sha256: None,
                locator: "1".into(),
            },
            confidence: Confidence::Direct,
        };

        store
            .ingest(
                run_id,
                &Batch {
                    source_identity: "chromium.history|brave|Default|/home/u/.config/Brave",
                    events: &[ev],
                    states: &[],
                    artifacts: &[],
                },
            )
            .unwrap();

        store
            .insert_ioc("domain", "zoom.us", Some("video app"))
            .unwrap();

        let filter = EventFilter::default();
        let utc = |ts: i64, _: &str| {
            DateTime::from_timestamp_micros(ts)
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default()
        };
        let time = ExportTime {
            zone: "UTC",
            format: &utc,
        };

        // 1. JSONL with redact, overwriting a permissive file: the mode must tighten
        let jsonl_out = dir.path().join("export.jsonl");
        fs::write(&jsonl_out, "old").unwrap();
        fs::set_permissions(&jsonl_out, fs::Permissions::from_mode(0o644)).unwrap();
        let count = export_case(
            &store,
            &filter,
            ExportFormat::Jsonl,
            &jsonl_out,
            true,
            &time,
        )
        .unwrap();
        assert_eq!(count, 1);
        let mode = fs::metadata(&jsonl_out).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let content = fs::read_to_string(&jsonl_out).unwrap();
        assert!(!content.contains("supersecret123"));
        assert!(content.contains("token=<sha256:"));
        assert!(content.contains("\"ioc_hits\":[\"zoom.us\"]"));
        assert!(content.contains("\"source_kind\":\"chromium.history\""));
        assert!(content.contains("\"kind\":\"web.visit\""));

        // 2. CSV without redact
        let csv_out = dir.path().join("export.csv");
        let count =
            export_case(&store, &filter, ExportFormat::Csv, &csv_out, false, &time).unwrap();
        assert_eq!(count, 1);
        let csv_content = fs::read_to_string(&csv_out).unwrap();
        assert!(csv_content.starts_with("id,time_utc"));
        // the time column follows the renderer it was given, not the host zone
        assert!(csv_content.contains("1970-01-01 00:00:00"), "{csv_content}");
        assert!(csv_content.contains("supersecret123"));
        assert!(csv_content.contains("zoom.us"));

        // 3. HTML
        let html_out = dir.path().join("export.html");
        let count =
            export_case(&store, &filter, ExportFormat::Html, &html_out, true, &time).unwrap();
        assert_eq!(count, 1);
        let html_content = fs::read_to_string(&html_out).unwrap();
        assert!(html_content.contains("<!DOCTYPE html>"));
        assert!(html_content.contains("IOC: zoom.us"));
        assert!(!html_content.contains("supersecret123"));
    }
}
