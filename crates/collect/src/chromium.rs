use crate::collector::{Collector, CollectCtx, Env, Sink};
use crate::utils::copy_db_set;
use anyhow::{Context, Result};
use fp_core::{Actor, Artifact, Confidence, Event, EventKind, Provenance, SourceInstance, SourceStatus, StateRecord, time::{self, TimeFormat}};
use rusqlite::Connection;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const BROWSERS: &[(&str, &[&str])] = &[
    ("Chrome", &[".config/google-chrome"]),
    ("Chromium", &[".config/chromium", "snap/chromium/common/chromium"]),
    (
        "Brave",
        &[
            ".config/BraveSoftware/Brave-Browser",
            "snap/brave/current/.config/BraveSoftware/Brave-Browser",
        ],
    ),
    ("Edge", &[".config/microsoft-edge"]),
    ("Vivaldi", &[".config/vivaldi"]),
    ("Opera", &[".config/opera"]),
];

pub struct ChromiumCollector;

impl ChromiumCollector {
    pub fn new() -> Self {
        Self
    }

    fn roots(env: &Env) -> Vec<(String, PathBuf)> {
        let mut out = Vec::new();
        for (name, rels) in BROWSERS {
            for rel in *rels {
                let flatpak = env.home.join(format!(".var/app/*/config/{rel}"));
                if let Some(p) = first_glob(&flatpak) {
                    out.push((name.to_string(), p));
                }
                let native = env.home.join(rel);
                if native.is_dir() {
                    out.push((name.to_string(), native));
                }
            }
        }
        out
    }

    fn collect_profile(
        &self,
        inst: &SourceInstance,
        ctx: &CollectCtx,
        sink: &Sink,
    ) -> Result<()> {
        let history = inst.path.join("History");
        if !history.exists() {
            return Ok(());
        }

        let (copy, file_sha256) = copy_db_set(&history, &ctx.temp_dir)?;
        let conn = Connection::open_with_flags(
            &copy,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;

        let provenance = |locator: String| Provenance {
            file: history.clone(),
            file_sha256: Some(file_sha256.clone()),
            locator,
        };

        self.collect_visits(&conn, inst, ctx, sink, provenance.clone())?;
        self.collect_downloads(&conn, inst, ctx, sink, provenance.clone())?;

        let prefs = inst.path.join("Preferences");
        if prefs.exists() {
            self.collect_preferences(&prefs, inst, ctx, sink, provenance.clone())?;
        }

        Ok(())
    }

    fn collect_visits(
        &self,
        conn: &Connection,
        inst: &SourceInstance,
        ctx: &CollectCtx,
        sink: &Sink,
        provenance: impl Fn(String) -> Provenance,
    ) -> Result<()> {
        let mut stmt = conn.prepare(
            "SELECT v.id, v.visit_time, v.from_visit, v.transition, u.url, u.title
             FROM visits v JOIN urls u ON u.id = v.url
             ORDER BY v.visit_time"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(VisitRow {
                id: row.get(0)?,
                visit_time: row.get(1)?,
                from_visit: row.get(2)?,
                transition: row.get::<_, i64>(3)? as u32,
                url: row.get(4)?,
                title: row.get(5)?,
            })
        })?;

        for row in rows {
            let row = row?;
            let norm = time::normalize(&row.visit_time.to_string(), &TimeFormat::WebKitMicros, None)?;
            if !in_window(ctx, norm.ts_utc) {
                continue;
            }

            let core = row.transition & 0xFF;
            let qualifiers = transition_qualifiers(row.transition);
            let is_redirect = row.transition & 0x8000_0000 != 0;
            let kind = if is_redirect {
                EventKind::WebRedirect
            } else {
                EventKind::WebVisit
            };

            let subject_host = extract_host(&row.url);
            let summary = if let Some(ref title) = row.title {
                format!("{} — {}", row.url, title)
            } else {
                row.url.clone()
            };

            sink.push_event(Event {
                run_id: ctx.run_id,
                ts_utc: norm.ts_utc,
                ts_precision: norm.precision,
                ts_raw: row.visit_time.to_string(),
                ts_raw_format: TimeFormat::WebKitMicros,
                ts_tz_assumed: norm.tz_assumed,
                source_kind: self.kind().to_string(),
                source_inst: inst.id,
                kind,
                actor: Some(Actor {
                    uid: None,
                    user: None,
                    pid: None,
                    exe: None,
                    app: Some(inst.app.clone()),
                }),
                subject: row.url,
                subject_host,
                summary,
                attrs: json!({
                    "visit_id": row.id,
                    "from_visit": row.from_visit,
                    "transition": row.transition,
                    "transition_core": core,
                    "transition_qualifiers": qualifiers,
                }),
                provenance: provenance(format!("visits.id={}", row.id)),
                confidence: Confidence::Direct,
            });
        }
        Ok(())
    }

    fn collect_downloads(
        &self,
        conn: &Connection,
        inst: &SourceInstance,
        ctx: &CollectCtx,
        sink: &Sink,
        provenance: impl Fn(String) -> Provenance,
    ) -> Result<()> {
        let mut stmt = conn.prepare(
            "SELECT d.id, d.start_time, d.target_path, d.total_bytes, d.state, d.tab_url, d.referrer,
                    c.chain_index, c.url
             FROM downloads d
             LEFT JOIN downloads_url_chains c ON c.id = d.id
             ORDER BY d.start_time, c.chain_index"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(DownloadRow {
                id: row.get(0)?,
                start_time: row.get(1)?,
                target_path: row.get(2)?,
                total_bytes: row.get(3)?,
                state: row.get::<_, i64>(4)? as u32,
                tab_url: row.get(5)?,
                referrer: row.get(6)?,
                chain_index: row.get(7)?,
                chain_url: row.get(8)?,
            })
        })?;

        let mut by_id: HashMap<i64, Vec<DownloadRow>> = HashMap::new();
        for row in rows {
            let row = row?;
            by_id.entry(row.id).or_default().push(row);
        }

        for (id, rows) in by_id {
            let first = rows.first().context("empty download group")?;
            let norm = time::normalize(&first.start_time.to_string(), &TimeFormat::WebKitMicros, None)?;
            if !in_window(ctx, norm.ts_utc) {
                continue;
            }

            let chain: Vec<String> = rows.iter().filter_map(|r| r.chain_url.clone()).collect();
            let url = chain.last().cloned().unwrap_or_default();
            let subject_host = extract_host(&url);
            let size = first.total_bytes.unwrap_or(0);

            sink.push_event(Event {
                run_id: ctx.run_id,
                ts_utc: norm.ts_utc,
                ts_precision: norm.precision,
                ts_raw: first.start_time.to_string(),
                ts_raw_format: TimeFormat::WebKitMicros,
                ts_tz_assumed: norm.tz_assumed,
                source_kind: self.kind().to_string(),
                source_inst: inst.id,
                kind: EventKind::WebDownload,
                actor: Some(Actor {
                    uid: None,
                    user: None,
                    pid: None,
                    exe: None,
                    app: Some(inst.app.clone()),
                }),
                subject: url.clone(),
                subject_host,
                summary: format!(
                    "{} → {} ({}",
                    first.target_path.as_deref().unwrap_or("?"),
                    url,
                    human_size(size)
                ),
                attrs: json!({
                    "download_id": id,
                    "target_path": first.target_path,
                    "total_bytes": size,
                    "state": first.state,
                    "tab_url": first.tab_url,
                    "referrer": first.referrer,
                    "url_chain": chain,
                }),
                provenance: provenance(format!("downloads.id={}", id)),
                confidence: Confidence::Direct,
            });

            if let Some(ref target) = first.target_path {
                let path = PathBuf::from(target);
                sink.push_artifact(Artifact {
                    path,
                    sha256: None,
                    size: first.total_bytes,
                    mtime_ns: None,
                    ctime_ns: None,
                    btime_ns: None,
                    owner_pkg: None,
                });
            }
        }
        Ok(())
    }

    fn collect_preferences(
        &self,
        prefs: &Path,
        inst: &SourceInstance,
        ctx: &CollectCtx,
        sink: &Sink,
        provenance: impl Fn(String) -> Provenance,
    ) -> Result<()> {
        let text = std::fs::read_to_string(prefs)?;
        let json: serde_json::Value = serde_json::from_str(&text)?;

        if let Some(exceptions) = json
            .get("profile")
            .and_then(|p| p.get("content_settings"))
            .and_then(|c| c.get("exceptions"))
        {
            for (perm_type, origins) in exceptions.as_object().unwrap_or(&serde_json::Map::new()) {
                for (origin, settings) in origins.as_object().unwrap_or(&serde_json::Map::new()) {
                    let setting = settings.get("setting").and_then(|v| v.as_i64());
                    let last_modified = settings
                        .get("last_modified")
                        .and_then(|v| v.as_str());
                    let host = origin.split_once("://")
                        .map(|(_, rest)| rest.split_once(':').map(|(h, _)| h).unwrap_or(rest))
                        .unwrap_or(origin)
                        .to_string();

                    sink.push_state(StateRecord {
                        run_id: ctx.run_id,
                        kind: "web.site_permission".to_string(),
                        key: format!("{}|{}|{}", inst.app, inst.profile.as_deref().unwrap_or("Default"), origin),
                        value: json!({
                            "permission": perm_type,
                            "origin": origin,
                            "setting": setting,
                            "last_modified": last_modified,
                        }),
                        provenance: provenance(format!(
                            "Preferences:profile.content_settings.exceptions.{perm_type}.{origin}"
                        )),
                    });

                    if let Some(raw) = last_modified {
                        if let Ok(norm) = time::normalize(raw, &TimeFormat::WebKitMicros, None) {
                            sink.push_event(Event {
                                run_id: ctx.run_id,
                                ts_utc: norm.ts_utc,
                                ts_precision: norm.precision,
                                ts_raw: raw.to_string(),
                                ts_raw_format: TimeFormat::WebKitMicros,
                                ts_tz_assumed: norm.tz_assumed,
                                source_kind: self.kind().to_string(),
                                source_inst: inst.id,
                                kind: EventKind::WebPermissionChange,
                                actor: Some(Actor {
                                    uid: None,
                                    user: None,
                                    pid: None,
                                    exe: None,
                                    app: Some(inst.app.clone()),
                                }),
                                subject: origin.to_string(),
                                subject_host: Some(host),
                                summary: format!("{origin}: {perm_type} = {setting:?}"),
                                attrs: json!({
                                    "permission": perm_type,
                                    "setting": setting,
                                }),
                                provenance: provenance(format!(
                                    "Preferences:profile.content_settings.exceptions.{perm_type}.{origin}"
                                )),
                                confidence: Confidence::Direct,
                            });
                        }
                    }
                }
            }
        }

        if let Some(extensions) = json
            .get("extensions")
            .and_then(|e| e.get("settings"))
            .and_then(|s| s.as_object())
        {
            for (ext_id, info) in extensions {
                let install_time = info
                    .get("install_time")
                    .or_else(|| info.get("first_install_time"))
                    .and_then(|v| v.as_str());
                let name = info
                    .get("manifest")
                    .and_then(|m| m.get("name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(ext_id);

                sink.push_state(StateRecord {
                    run_id: ctx.run_id,
                    kind: "ext.installed".to_string(),
                    key: format!("{}|{}", inst.app, ext_id),
                    value: json!({
                        "id": ext_id,
                        "name": name,
                        "info": info,
                    }),
                    provenance: provenance(format!("Preferences:extensions.settings.{ext_id}")),
                });

                if let Some(raw) = install_time {
                    if let Ok(norm) = time::normalize(raw, &TimeFormat::WebKitMicros, None) {
                        sink.push_event(Event {
                            run_id: ctx.run_id,
                            ts_utc: norm.ts_utc,
                            ts_precision: norm.precision,
                            ts_raw: raw.to_string(),
                            ts_raw_format: TimeFormat::WebKitMicros,
                            ts_tz_assumed: norm.tz_assumed,
                            source_kind: self.kind().to_string(),
                            source_inst: inst.id,
                            kind: EventKind::ExtInstalled,
                            actor: Some(Actor {
                                uid: None,
                                user: None,
                                pid: None,
                                exe: None,
                                app: Some(inst.app.clone()),
                            }),
                            subject: ext_id.to_string(),
                            subject_host: None,
                            summary: format!("{name} ({ext_id})"),
                            attrs: json!({ "name": name }),
                            provenance: provenance(format!("Preferences:extensions.settings.{ext_id}")),
                            confidence: Confidence::Direct,
                        });
                    }
                }
            }
        }

        Ok(())
    }
}

impl Collector for ChromiumCollector {
    fn kind(&self) -> &'static str {
        "chromium.history"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let mut id = 1u64;
        let mut out = Vec::new();
        for (app, root) in Self::roots(env) {
            for profile in list_profiles(&root) {
                let path = root.join(&profile);
                let history = path.join("History");
                let status = if history.exists() {
                    SourceStatus::Ok
                } else {
                    SourceStatus::Unavailable
                };
                out.push(SourceInstance {
                    id,
                    kind: self.kind().to_string(),
                    app: app.clone(),
                    profile: Some(profile),
                    path,
                    status,
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
        match self.collect_profile(inst, ctx, sink) {
            Ok(()) => Ok(fp_core::CollectStats::empty()),
            Err(e) => {
                sink.error(format!("{}: {}", inst.path.display(), e));
                Ok(fp_core::CollectStats::empty())
            }
        }
    }
}

#[derive(Debug)]
struct VisitRow {
    id: i64,
    visit_time: i64,
    from_visit: i64,
    transition: u32,
    url: String,
    title: Option<String>,
}

#[derive(Debug, Clone)]
struct DownloadRow {
    id: i64,
    start_time: i64,
    target_path: Option<String>,
    total_bytes: Option<i64>,
    state: u32,
    tab_url: Option<String>,
    referrer: Option<String>,
    #[allow(dead_code)]
    chain_index: Option<i64>,
    chain_url: Option<String>,
}

fn transition_qualifiers(transition: u32) -> Vec<&'static str> {
    let mut q = Vec::new();
    if transition & 0x0080_0000 != 0 { q.push("FORWARD_BACK"); }
    if transition & 0x0100_0000 != 0 { q.push("FROM_ADDRESS_BAR"); }
    if transition & 0x0200_0000 != 0 { q.push("HOME_PAGE"); }
    if transition & 0x1000_0000 != 0 { q.push("CHAIN_START"); }
    if transition & 0x2000_0000 != 0 { q.push("CHAIN_END"); }
    if transition & 0x4000_0000 != 0 { q.push("CLIENT_REDIRECT"); }
    if transition & 0x8000_0000 != 0 { q.push("SERVER_REDIRECT"); }
    q
}

fn extract_host(url: &str) -> Option<String> {
    url.split_once("://")
        .and_then(|(_, rest)| {
            let host = rest.split('/').next()?;
            Some(host.split(':').next().unwrap_or(host).to_lowercase())
        })
}

fn in_window(ctx: &CollectCtx, ts: i64) -> bool {
    ctx.since.map(|lo| ts >= lo).unwrap_or(true)
        && ctx.until.map(|hi| ts <= hi).unwrap_or(true)
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

fn first_glob(pattern: &Path) -> Option<PathBuf> {
    glob::glob(pattern.to_string_lossy().as_ref())
        .ok()?
        .filter_map(Result::ok)
        .next()
}

fn list_profiles(root: &Path) -> Vec<String> {
    let mut profiles = Vec::new();
    if root.join("History").exists() {
        profiles.push("Default".to_string());
    }
    if root.join("Default").is_dir() {
        profiles.push("Default".to_string());
    }
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            if s.starts_with("Profile ") && entry.path().is_dir() {
                profiles.push(s.into_owned());
            }
        }
    }
    profiles.sort();
    profiles.dedup();
    profiles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_profiles_finds_default_and_profiles() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join("Default")).unwrap();
        std::fs::create_dir(root.join("Profile 1")).unwrap();
        std::fs::create_dir(root.join("Profile 2")).unwrap();
        let got = list_profiles(root);
        assert_eq!(got, vec!["Default", "Profile 1", "Profile 2"]);
    }

    #[test]
    fn extract_host_handles_url() {
        assert_eq!(
            extract_host("https://us09-web-zoom.com/j/123"),
            Some("us09-web-zoom.com".to_string())
        );
    }
}
