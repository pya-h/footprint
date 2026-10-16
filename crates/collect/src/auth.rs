use crate::collector::{Collector, CollectCtx, Env, Sink};
use crate::utils::in_window;
use anyhow::Result;
use fp_core::{Actor, Confidence, Event, EventKind, Provenance, SourceInstance, SourceStatus, time::TimeFormat};
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub struct AuthCollector;

impl AuthCollector {
    pub fn new() -> Self {
        Self
    }
}

impl Collector for AuthCollector {
    fn kind(&self) -> &'static str {
        "auth.journald"
    }

    fn discover(&self, _env: &Env) -> Vec<SourceInstance> {
        vec![SourceInstance {
            id: 1,
            kind: self.kind().to_string(),
            app: "journald".to_string(),
            profile: None,
            path: PathBuf::from("/run/log/journal"),
            status: SourceStatus::Ok,
            note: None,
        }]
    }

    fn collect(
        &self,
        inst: &SourceInstance,
        ctx: &CollectCtx,
        sink: &Sink,
    ) -> Result<fp_core::CollectStats> {
        let since_arg = ctx.since.map(|since| {
            chrono::DateTime::from_timestamp_micros(since)
                .map(|d| d.to_rfc3339())
                .unwrap_or_default()
        });
        let until_arg = ctx.until.map(|until| {
            chrono::DateTime::from_timestamp_micros(until)
                .map(|d| d.to_rfc3339())
                .unwrap_or_default()
        });
        let filters: &[&[&str]] = &[
            &["_COMM=sudo", "_COMM=su", "_COMM=pkexec"],
            &["_SYSTEMD_UNIT=ssh.service"],
        ];

        let mut lines = Vec::new();
        for filter in filters {
            let mut cmd = Command::new("/usr/bin/journalctl");
            cmd.arg("-o").arg("json");
            if let Some(ref dt) = since_arg {
                cmd.arg("--since").arg(dt);
            }
            if let Some(ref dt) = until_arg {
                cmd.arg("--until").arg(dt);
            }
            for f in *filter {
                cmd.arg(f);
            }
            cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
            let output = cmd.output()?;
            if !output.status.success() {
                sink.error(format!(
                    "journalctl exited {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr)
                ));
                continue;
            }
            lines.extend(String::from_utf8_lossy(&output.stdout).lines().map(|s| s.to_string()));
        }
        lines.sort();
        lines.dedup();

        for line in lines {
            let Ok(entry): Result<JournalEntry, _> = serde_json::from_str(&line) else { continue };
            let Some(ts_us) = entry.realtime_us() else { continue };
            if !in_window(ctx, ts_us) {
                continue;
            }

            let message = entry.message.as_deref().unwrap_or("");
            let (kind, summary) = match entry.comm.as_deref() {
                Some("sudo") => {
                    let cmd = extract_field(message, "COMMAND=");
                    let user = extract_field(message, "USER=");
                    let tty = extract_field(message, "TTY=");
                    let summary = format!(
                        "{}: {}{}",
                        user.as_deref().unwrap_or("?"),
                        cmd.as_deref().unwrap_or("?"),
                        tty.map(|t| format!(" ({t})")).unwrap_or_default()
                    );
                    (EventKind::PrivSudo, summary)
                }
                Some("su") => (EventKind::PrivSu, message.to_string()),
                Some("pkexec") => (EventKind::PrivSu, message.to_string()),
                _ => (EventKind::AuthSsh, message.to_string()),
            };

            sink.push_event(Event {
                run_id: ctx.run_id,
                ts_utc: ts_us,
                ts_precision: fp_core::TimePrecision::Exact,
                ts_raw: entry.realtime.clone().unwrap_or_default(),
                ts_raw_format: TimeFormat::JournaldRealtime,
                ts_tz_assumed: None,
                source_kind: self.kind().to_string(),
                source_inst: inst.id,
                kind,
                actor: Some(Actor {
                    uid: None,
                    user: None,
                    pid: None,
                    exe: entry.exe.as_ref().map(PathBuf::from),
                    app: entry.comm.clone(),
                }),
                subject: extract_field(message, "COMMAND=").unwrap_or_default(),
                subject_host: None,
                summary,
                attrs: json!({
                    "cursor": entry.cursor,
                    "comm": entry.comm,
                    "message": entry.message,
                }),
                provenance: Provenance {
                    file: inst.path.clone(),
                    file_sha256: None,
                    locator: entry.cursor.unwrap_or_default(),
                },
                confidence: Confidence::Direct,
            });
        }
        Ok(fp_core::CollectStats::empty())
    }
}

#[derive(Debug, Deserialize)]
struct JournalEntry {
    #[serde(rename = "__REALTIME_TIMESTAMP")]
    realtime: Option<String>,
    #[serde(rename = "__CURSOR")]
    cursor: Option<String>,
    #[serde(rename = "_COMM")]
    comm: Option<String>,
    #[serde(rename = "_EXE")]
    exe: Option<String>,
    #[serde(rename = "MESSAGE")]
    message: Option<String>,
}

impl JournalEntry {
    fn realtime_us(&self) -> Option<i64> {
        self.realtime.as_ref().and_then(|s| s.parse().ok())
    }
}

fn extract_field(msg: &str, prefix: &str) -> Option<String> {
    msg.split_whitespace()
        .find(|w| w.starts_with(prefix))
        .map(|w| w.strip_prefix(prefix).unwrap_or(w).to_string())
}
