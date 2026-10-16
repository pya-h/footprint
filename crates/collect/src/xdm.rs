use crate::collector::{Collector, CollectCtx, Env, Sink};
use crate::utils::{copy_db_set, in_window};
use anyhow::Result;
use fp_core::{Actor, Artifact, Confidence, Event, EventKind, Provenance, SourceInstance, SourceStatus, time::TimeFormat};
use rusqlite::Connection;
use serde_json::json;
use std::path::PathBuf;

const XDM_PATHS: &[&str] = &[".xdm-app-data/downloads.db"];

pub struct XdmCollector;

impl XdmCollector {
    pub fn new() -> Self {
        Self
    }

    fn db_paths(env: &Env) -> Vec<PathBuf> {
        XDM_PATHS
            .iter()
            .map(|p| env.home.join(p))
            .filter(|p| p.exists())
            .collect()
    }
}

impl Collector for XdmCollector {
    fn kind(&self) -> &'static str {
        "dlmgr.xdm"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        Self::db_paths(env)
            .into_iter()
            .enumerate()
            .map(|(i, path)| SourceInstance {
                id: (i + 1) as u64,
                kind: self.kind().to_string(),
                app: "xdm".to_string(),
                profile: None,
                path,
                status: SourceStatus::Ok,
                note: None,
            })
            .collect()
    }

    fn collect(
        &self,
        inst: &SourceInstance,
        ctx: &CollectCtx,
        sink: &Sink,
    ) -> Result<fp_core::CollectStats> {
        let (copy, file_sha256) = copy_db_set(&inst.path, &ctx.temp_dir)?;
        let conn = Connection::open_with_flags(
            &copy,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;

        let mut stmt = conn.prepare(
            "SELECT name, primary_url, targetdir, size, date_added, completed FROM downloads"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(Download {
                name: row.get(0)?,
                primary_url: row.get(1)?,
                targetdir: row.get(2)?,
                size: row.get(3)?,
                date_added: row.get(4)?,
                completed: row.get(5)?,
            })
        })?;

        for row in rows {
            let row = row?;
            let norm = fp_core::time::normalize(
                &row.date_added.to_string(),
                &TimeFormat::DotNetBinary,
                None,
            )?;
            if !in_window(ctx, norm.ts_utc) {
                continue;
            }

            let host = extract_host(&row.primary_url);
            let target = row.targetdir.as_ref().map(|d| PathBuf::from(d).join(&row.name));
            let summary = if let Some(ref t) = target {
                format!("{} ← {} ({})", t.display(), row.primary_url, human_size(row.size.unwrap_or(0)))
            } else {
                format!("{} ({})", row.primary_url, human_size(row.size.unwrap_or(0)))
            };

            sink.push_event(Event {
                run_id: ctx.run_id,
                ts_utc: norm.ts_utc,
                ts_precision: norm.precision,
                ts_raw: row.date_added.to_string(),
                ts_raw_format: TimeFormat::DotNetBinary,
                ts_tz_assumed: norm.tz_assumed,
                source_kind: self.kind().to_string(),
                source_inst: inst.id,
                kind: EventKind::DlManagerDownload,
                actor: Some(Actor {
                    uid: None,
                    user: None,
                    pid: None,
                    exe: None,
                    app: Some("xdm".to_string()),
                }),
                subject: row.primary_url.clone(),
                subject_host: host,
                summary,
                attrs: json!({
                    "name": row.name,
                    "targetdir": row.targetdir,
                    "size": row.size,
                    "completed": row.completed,
                }),
                provenance: Provenance {
                    file: inst.path.clone(),
                    file_sha256: Some(file_sha256.clone()),
                    locator: format!("downloads.name={}", row.name),
                },
                confidence: Confidence::Direct,
            });

            if let Some(target) = target {
                sink.push_artifact(Artifact {
                    path: target,
                    sha256: None,
                    size: row.size,
                    mtime_ns: None,
                    ctime_ns: None,
                    btime_ns: None,
                    owner_pkg: None,
                });
            }
        }
        Ok(fp_core::CollectStats::empty())
    }
}

#[derive(Debug)]
struct Download {
    name: String,
    primary_url: String,
    targetdir: Option<String>,
    size: Option<i64>,
    date_added: i64,
    completed: Option<i64>,
}

fn extract_host(url: &str) -> Option<String> {
    url.split_once("://")
        .and_then(|(_, rest)| {
            let host = rest.split('/').next()?;
            Some(host.split(':').next().unwrap_or(host).to_lowercase())
        })
}

fn human_size(bytes: i64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = UNITS[0];
    for &u in UNITS.iter().skip(1) {
        if size < 1024.0 {
            break;
        }
        size /= 1024.0;
        unit = u;
    }
    format!("{:.1} {unit}", size)
}
