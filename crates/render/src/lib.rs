pub mod csv;
pub mod html;
pub mod jsonl;
pub mod redact;

pub use csv::render_csv;
pub use html::render_html;
pub use jsonl::render_jsonl;
pub use redact::{hash_secret, redact_json, redact_text, redact_url};

use anyhow::{Context, Result};
use chrono::{DateTime, Local};
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

fn format_local_time(ts_utc: i64, precision: &str) -> String {
    if precision == "order_only" {
        return "(untimed)".to_string();
    }
    DateTime::from_timestamp_micros(ts_utc)
        .map(|t| {
            t.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| format!("@{ts_utc}µs"))
}

pub fn export_case(
    store: &Store,
    filter: &EventFilter,
    source_filters: &[String],
    format: ExportFormat,
    out_path: &Path,
    redact: bool,
) -> Result<usize> {
    let mut rows = store.list_events_detailed(filter)?;

    if !source_filters.is_empty() {
        rows.retain(|r| {
            let app = r.app.as_deref().unwrap_or("");
            let source_str = match (&r.app, &r.profile) {
                (Some(a), Some(p)) => format!("{a}/{p}"),
                (Some(a), None) => a.clone(),
                _ => String::new(),
            };
            source_filters.iter().any(|s| {
                s == &r.kind || s == app || s == &source_str || r.kind.starts_with(s.as_str())
            })
        });
    }

    let iocs = store.list_iocs().unwrap_or_default();

    let mut events = Vec::with_capacity(rows.len());
    for r in rows {
        let ts_local = format_local_time(r.ts_utc, &r.ts_precision);
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

    let mut writer = BufWriter::new(file);

    match format {
        ExportFormat::Jsonl => {
            render_jsonl(&mut writer, &events)?;
        }
        ExportFormat::Csv => {
            render_csv(&mut writer, &events)?;
        }
        ExportFormat::Html => {
            let mut ioc_hits = check_iocs(store).unwrap_or_default();
            if redact {
                for hit in &mut ioc_hits {
                    hit.target_desc = redact_text(&hit.target_desc);
                    hit.matched_value = redact_text(&hit.matched_value);
                }
            }
            let runs = store.list_runs().unwrap_or_default();
            let report = ExportReport {
                case_path: store.path().display().to_string(),
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

        // 1. JSONL with redact
        let jsonl_out = dir.path().join("export.jsonl");
        let count =
            export_case(&store, &filter, &[], ExportFormat::Jsonl, &jsonl_out, true).unwrap();
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
        let count = export_case(&store, &filter, &[], ExportFormat::Csv, &csv_out, false).unwrap();
        assert_eq!(count, 1);
        let csv_content = fs::read_to_string(&csv_out).unwrap();
        assert!(csv_content.starts_with("id,time_utc"));
        assert!(csv_content.contains("supersecret123"));
        assert!(csv_content.contains("zoom.us"));

        // 3. HTML
        let html_out = dir.path().join("export.html");
        let count = export_case(&store, &filter, &[], ExportFormat::Html, &html_out, true).unwrap();
        assert_eq!(count, 1);
        let html_content = fs::read_to_string(&html_out).unwrap();
        assert!(html_content.contains("<!DOCTYPE html>"));
        assert!(html_content.contains("IOC: zoom.us"));
        assert!(!html_content.contains("supersecret123"));
    }
}
