use crate::collector::{Collector, CollectCtx, Env, Sink};
use crate::utils::in_window;
use anyhow::Result;
use fp_core::{Actor, Confidence, Event, EventKind, Provenance, SourceInstance, SourceStatus, time::TimeFormat};
use serde_json::json;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

const DPKG_LOGS: &[&str] = &["/var/log/dpkg.log"];
const APT_HISTORY: &[&str] = &["/var/log/apt/history.log"];

pub struct PkgCollector;

impl PkgCollector {
    pub fn new() -> Self {
        Self
    }
}

impl Collector for PkgCollector {
    fn kind(&self) -> &'static str {
        "pkg.dpkg"
    }

    fn discover(&self, _env: &Env) -> Vec<SourceInstance> {
        let mut id = 1u64;
        let mut out = Vec::new();
        for path in DPKG_LOGS.iter().map(PathBuf::from) {
            if path.exists() {
                out.push(SourceInstance {
                    id,
                    kind: self.kind().to_string(),
                    app: "dpkg".to_string(),
                    profile: None,
                    path,
                    status: SourceStatus::Ok,
                    note: None,
                });
                id += 1;
            }
        }
        for path in APT_HISTORY.iter().map(PathBuf::from) {
            if path.exists() {
                out.push(SourceInstance {
                    id,
                    kind: "pkg.apt".to_string(),
                    app: "apt".to_string(),
                    profile: None,
                    path,
                    status: SourceStatus::Ok,
                    note: None,
                });
                id += 1;
            }
        }
        out
    }

    fn collect(
        &self,
        inst: &SourceInstance,
        ctx: &CollectCtx,
        sink: &Sink,
    ) -> Result<fp_core::CollectStats> {
        if inst.kind == "pkg.apt" {
            collect_apt(inst, ctx, sink)
        } else {
            collect_dpkg(inst, ctx, sink)
        }
    }
}

fn collect_dpkg(
    inst: &SourceInstance,
    ctx: &CollectCtx,
    sink: &Sink,
) -> Result<fp_core::CollectStats> {
    let file = fs::File::open(&inst.path)?;
    let reader = BufReader::new(file);
    let format = TimeFormat::LocalNaive("%Y-%m-%d %H:%M:%S".to_string());

    for (line_no, line) in reader.lines().enumerate() {
        let line = line?;
        if line.len() < 20 { continue; }
        let raw = &line[..19];
        let rest = &line[20..];
        let norm = fp_core::time::normalize(raw, &format, ctx.assume_tz.as_deref())?;
        if !in_window(ctx, norm.ts_utc) {
            continue;
        }

        let parts: Vec<&str> = rest.split_whitespace().collect();
        let (kind, summary) = match parts.get(0) {
            Some(&"install") => {
                let pkg = parts.get(1).unwrap_or(&"?");
                let ver = parts.get(3).unwrap_or(&"?");
                (EventKind::PkgInstall, format!("{pkg} {ver}"))
            }
            Some(&"upgrade") => {
                let pkg = parts.get(1).unwrap_or(&"?");
                let ver = parts.get(3).unwrap_or(&"?");
                (EventKind::PkgUpgrade, format!("{pkg} {ver}"))
            }
            Some(&"remove") => {
                let pkg = parts.get(1).unwrap_or(&"?");
                (EventKind::PkgRemove, pkg.to_string())
            }
            _ => continue,
        };

        sink.push_event(Event {
            run_id: ctx.run_id,
            ts_utc: norm.ts_utc,
            ts_precision: norm.precision,
            ts_raw: raw.to_string(),
            ts_raw_format: format.clone(),
            ts_tz_assumed: norm.tz_assumed,
            source_kind: inst.kind.clone(),
            source_inst: inst.id,
            kind,
            actor: None,
            subject: summary.clone(),
            subject_host: None,
            summary,
            attrs: json!({ "line": line_no + 1, "raw": line }),
            provenance: Provenance {
                file: inst.path.clone(),
                file_sha256: None,
                locator: format!("{}:line {}", inst.path.display(), line_no + 1),
            },
            confidence: Confidence::Direct,
        });
    }
    Ok(fp_core::CollectStats::empty())
}

fn collect_apt(
    inst: &SourceInstance,
    ctx: &CollectCtx,
    sink: &Sink,
) -> Result<fp_core::CollectStats> {
    let text = fs::read_to_string(&inst.path)?;
    let format = TimeFormat::LocalNaive("%Y-%m-%d %H:%M:%S".to_string());
    let mut current_start: Option<String> = None;
    let mut current_cmdline: Option<String> = None;
    let mut current_actor: Option<String> = None;

    for (line_no, line) in text.lines().enumerate() {
        if line.starts_with("Start-Date: ") {
            current_start = Some(line.strip_prefix("Start-Date: ").unwrap_or("").trim().to_string());
            current_cmdline = None;
            current_actor = None;
        } else if line.starts_with("Commandline: ") {
            current_cmdline = Some(line.strip_prefix("Commandline: ").unwrap_or("").to_string());
        } else if line.starts_with("Requested-By: ") {
            current_actor = Some(line.strip_prefix("Requested-By: ").unwrap_or("").to_string());
        } else if line.starts_with("Install: ") || line.starts_with("Upgrade: ") || line.starts_with("Remove: ") {
            let kind = if line.starts_with("Install: ") {
                EventKind::PkgInstall
            } else if line.starts_with("Upgrade: ") {
                EventKind::PkgUpgrade
            } else {
                EventKind::PkgRemove
            };
            let raw_ts = current_start.as_deref().unwrap_or("");
            let Ok(norm) = fp_core::time::normalize(raw_ts, &format, ctx.assume_tz.as_deref()) else { continue };
            if !in_window(ctx, norm.ts_utc) {
                continue;
            }

            let summary = format!(
                "{} ({})",
                line,
                current_cmdline.as_deref().unwrap_or("-")
            );

            sink.push_event(Event {
                run_id: ctx.run_id,
                ts_utc: norm.ts_utc,
                ts_precision: norm.precision,
                ts_raw: raw_ts.to_string(),
                ts_raw_format: format.clone(),
                ts_tz_assumed: norm.tz_assumed,
                source_kind: inst.kind.clone(),
                source_inst: inst.id,
                kind,
                actor: current_actor.as_ref().map(|a| parse_actor(a)),
                subject: current_cmdline.clone().unwrap_or_default(),
                subject_host: None,
                summary,
                attrs: json!({
                    "commandline": current_cmdline,
                    "actor": current_actor,
                }),
                provenance: Provenance {
                    file: inst.path.clone(),
                    file_sha256: None,
                    locator: format!("{}:line {}", inst.path.display(), line_no + 1),
                },
                confidence: Confidence::Direct,
            });
        }
    }
    Ok(fp_core::CollectStats::empty())
}

fn parse_actor(s: &str) -> Actor {
    let parts: Vec<&str> = s.split_whitespace().collect();
    let user = parts.first().map(|s| s.to_string());
    let uid = parts.get(1).and_then(|p| p.trim_start_matches('(').trim_end_matches(')').parse().ok());
    Actor { uid, user, pid: None, exe: None, app: Some("apt".to_string()) }
}
