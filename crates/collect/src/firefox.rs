//! Firefox family (Firefox, Zen, LibreWolf): places, downloads, permissions, extensions.
//!
//! The same copy-then-read-only rule as chromium applies; `cookies.sqlite`, `logins.json`
//! and `key4.db` are on the deny list and never opened.

use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::event::{Origin, Stamp, new_event};
use crate::utils::{
    copy_db_set, extract_host, file_uri_to_path, human_size, in_window, open_snapshot,
    read_evidence, stage_dir, table_has_columns,
};
use anyhow::Result;
use fp_core::time::TimeFormat;
use fp_core::{Actor, Artifact, Availability, EventKind, SourceInstance, StateRecord};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// (browser, config root relative to $HOME)
const ROOTS: &[(&str, &str)] = &[
    ("Firefox", ".mozilla/firefox"),
    // newer Firefox releases follow XDG for new profiles
    ("Firefox", ".config/mozilla/firefox"),
    ("Firefox", "snap/firefox/common/.mozilla/firefox"),
    ("Firefox", ".var/app/org.mozilla.firefox/.mozilla/firefox"),
    ("Zen", ".zen"),
    ("LibreWolf", ".librewolf"),
];

/// `moz_historyvisits.visit_type` values that are redirects.
const PERMANENT_REDIRECT: i64 = 5;
const TEMPORARY_REDIRECT: i64 = 6;

fn visit_type_name(t: i64) -> &'static str {
    match t {
        1 => "link",
        2 => "typed",
        3 => "bookmark",
        4 => "embed",
        5 => "permanent_redirect",
        6 => "temporary_redirect",
        7 => "download",
        8 => "framed",
        _ => "unknown",
    }
}

/// permission types in `moz_perms` that are site grants, not internal state
const PERMISSION_TYPES: &[&str] = &[
    "camera",
    "microphone",
    "desktop-notification",
    "geo",
    "popup",
    "persistent-storage",
    "screen-wake-lock",
    "orientation-lock",
    "open-protocol-handler",
    "vertical-scroll",
];

pub struct FirefoxCollector;

impl FirefoxCollector {
    fn profiles(env: &Env) -> Vec<(&'static str, String, PathBuf)> {
        let mut out = Vec::new();
        for &(browser, rel) in ROOTS {
            let root = env.home.join(rel);
            if !root.is_dir() {
                continue;
            }
            // profiles.ini carries the real names (`default-release`, `default-esr`); the
            // directory is a random prefix. Anything on disk but not in the ini (or with no
            // ini at all) falls back to the prefix.
            let mut seen: Vec<PathBuf> = Vec::new();
            for (name, path) in parse_profiles_ini(env, &root) {
                if has_evidence(&path) && !seen.contains(&path) {
                    seen.push(path.clone());
                    out.push((browser, name, path));
                }
            }
            let Ok(entries) = std::fs::read_dir(&root) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() || !has_evidence(&path) || seen.contains(&path) {
                    continue;
                }
                let name = entry
                    .file_name()
                    .to_string_lossy()
                    .split('.')
                    .next()
                    .unwrap_or("default")
                    .to_string();
                seen.push(path.clone());
                out.push((browser, name, path));
            }
        }
        out
    }
}

fn has_evidence(path: &std::path::Path) -> bool {
    ["places.sqlite", "permissions.sqlite", "extensions.json"]
        .iter()
        .any(|f| path.join(f).exists())
}

/// `[Profile0] Name=… Path=… IsRelative=…` entries, in file order. An absolute `Path` is
/// resolved under the environment's root, so a mounted image never points at the host.
fn parse_profiles_ini(env: &Env, root: &std::path::Path) -> Vec<(String, PathBuf)> {
    let Ok(file) = read_evidence(&root.join("profiles.ini")) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&file.bytes);
    let mut out = Vec::new();
    let mut name: Option<String> = None;
    let mut path: Option<PathBuf> = None;
    let mut relative = true;
    let flush = |name: &mut Option<String>,
                 path: &mut Option<PathBuf>,
                 relative: bool,
                 out: &mut Vec<(String, PathBuf)>| {
        if let (Some(n), Some(p)) = (name.take(), path.take()) {
            let path = if relative {
                root.join(&p)
            } else {
                env.system_path(&p.to_string_lossy())
            };
            out.push((n, path));
        }
    };
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            flush(&mut name, &mut path, relative, &mut out);
            relative = true;
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "Name" => name = Some(value.trim().to_string()),
            "Path" => path = Some(PathBuf::from(value.trim())),
            "IsRelative" => relative = value.trim() != "0",
            _ => {}
        }
    }
    flush(&mut name, &mut path, relative, &mut out);
    out
}

impl Collector for FirefoxCollector {
    fn per_user(&self) -> bool {
        true
    }

    fn kind(&self) -> &'static str {
        "firefox.history"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let mut out = Vec::new();
        for (browser, profile, path) in Self::profiles(env) {
            let mut inst = SourceInstance::new(self.kind(), browser, Some(profile), path);
            if !inst.path.join("places.sqlite").exists() {
                inst.status = Availability::Empty;
                inst.note = Some("no places.sqlite".into());
            }
            out.push(inst);
        }
        out
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let places = inst.path.join("places.sqlite");
        if places.exists() {
            let snapshot = copy_db_set(&places, &stage_dir(ctx, inst.id)?)?;
            let conn = open_snapshot(&snapshot)?;
            let origin = Origin {
                file: &places,
                sha256: Some(&snapshot.sha256),
            };
            let window = (ctx.since.unwrap_or(i64::MIN), ctx.until.unwrap_or(i64::MAX));
            collect_visits(&conn, inst, ctx, &origin, window, sink)?;
            if let Err(e) = collect_downloads(&conn, inst, ctx, &origin, sink) {
                sink.warn(format!("{}: downloads: {e:#}", places.display()));
            }
        }
        let permissions = inst.path.join("permissions.sqlite");
        if permissions.exists() {
            match collect_permissions(&permissions, inst, ctx, sink) {
                Ok(()) => {}
                Err(e) => sink.warn(format!("{}: {e:#}", permissions.display())),
            }
        }
        let extensions = inst.path.join("extensions.json");
        if extensions.exists() {
            match collect_extensions(&extensions, inst, ctx, sink) {
                Ok(()) => {}
                Err(e) => sink.warn(format!("{}: {e:#}", extensions.display())),
            }
        }
        Ok(())
    }
}

fn collect_visits(
    conn: &Connection,
    inst: &SourceInstance,
    ctx: &CollectCtx,
    origin: &Origin,
    (lo, hi): (i64, i64),
    sink: &mut Sink,
) -> Result<()> {
    if !table_has_columns(
        conn,
        "moz_historyvisits",
        &["id", "from_visit", "place_id", "visit_date", "visit_type"],
    )? || !table_has_columns(conn, "moz_places", &["id", "url", "title"])?
    {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT v.id, v.visit_date, v.from_visit, v.visit_type, p.url, p.title, fp.url
         FROM moz_historyvisits v
         JOIN moz_places p ON p.id = v.place_id
         LEFT JOIN moz_historyvisits fv ON fv.id = v.from_visit AND v.from_visit != 0
         LEFT JOIN moz_places fp ON fp.id = fv.place_id
         WHERE v.visit_date BETWEEN ?1 AND ?2
         ORDER BY v.visit_date",
    )?;
    let mut rows = stmt.query([lo, hi])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let visit_date: i64 = row.get(1)?;
        let from_visit: i64 = row.get(2)?;
        let visit_type: i64 = row.get(3)?;
        let url: String = row.get(4)?;
        let title: Option<String> = row.get::<_, Option<String>>(5)?.filter(|t| !t.is_empty());
        let from_url: Option<String> = row.get(6)?;

        let stamp = Stamp::parse(visit_date.to_string(), TimeFormat::PRTime, None)?;
        let (kind, summary) = match (visit_type, &from_url) {
            (PERMANENT_REDIRECT | TEMPORARY_REDIRECT, Some(from)) => (
                EventKind::WebRedirect,
                format!("{from} → {url} [{}]", visit_type_name(visit_type)),
            ),
            (PERMANENT_REDIRECT | TEMPORARY_REDIRECT, None) => (
                EventKind::WebRedirect,
                format!("→ {url} [{}]", visit_type_name(visit_type)),
            ),
            _ => (
                EventKind::WebVisit,
                title
                    .as_ref()
                    .map_or_else(|| url.clone(), |t| format!("{url} — {t}")),
            ),
        };
        sink.event(fp_core::Event {
            actor: Some(Actor::app(inst.app.as_str())),
            subject_host: extract_host(&url),
            attrs: json!({
                "visit_id": id,
                "from_visit": from_visit,
                "from_url": from_url,
                "title": title,
                "visit_type": visit_type,
                "visit_type_name": visit_type_name(visit_type),
            }),
            ..new_event(
                ctx,
                inst,
                stamp,
                kind,
                url,
                summary,
                origin.at(format!("moz_historyvisits.id={id}")),
            )
        });
    }
    Ok(())
}

struct FirefoxDownload {
    url: String,
    target: Option<String>,
    size: Option<i64>,
    end_time: Option<i64>,
}

/// Modern Firefox writes the `downloads/metaData` `endTime` in milliseconds (JS `Date.now()`;
/// a real parsed record shows `1748605371660` — 13 digits, its last three matching the
/// annotation's own microsecond `lastModified`'s milliseconds); legacy nsDownloadManager
/// wrote PRTime µs. Above this floor the value is µs, below it ms: 10^14 ms is the year
/// 5138 and 10^14 µs is 1973, so real values never collide.
const PRTIME_FLOOR: i64 = 100_000_000_000_000;

fn end_time_format(end_time: i64) -> TimeFormat {
    if end_time >= PRTIME_FLOOR {
        TimeFormat::PRTime
    } else {
        TimeFormat::UnixMillis
    }
}

/// Downloads are `moz_annos` rows named `downloads/destinationFileURI` / `downloads/metaData`.
fn collect_downloads(
    conn: &Connection,
    inst: &SourceInstance,
    ctx: &CollectCtx,
    origin: &Origin,
    sink: &mut Sink,
) -> Result<()> {
    if !table_has_columns(
        conn,
        "moz_annos",
        &["place_id", "anno_attribute_id", "content"],
    )? || !table_has_columns(conn, "moz_anno_attributes", &["id", "name"])?
    {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT p.id, p.url, ma.name, a.content
         FROM moz_annos a
         JOIN moz_anno_attributes ma ON ma.id = a.anno_attribute_id
         JOIN moz_places p ON p.id = a.place_id
         WHERE ma.name = 'downloads/destinationFileURI' OR ma.name = 'downloads/metaData'",
    )?;
    let mut rows = stmt.query([])?;
    // keyed by place id: the annotations of one download hang off its source url's place
    let mut downloads: BTreeMap<i64, FirefoxDownload> = BTreeMap::new();
    while let Some(row) = rows.next()? {
        let place_id: i64 = row.get(0)?;
        let url: String = row.get(1)?;
        let name: String = row.get(2)?;
        let content: String = row.get(3)?;
        let entry = downloads
            .entry(place_id)
            .or_insert_with(|| FirefoxDownload {
                url,
                target: None,
                size: None,
                end_time: None,
            });
        if name == "downloads/destinationFileURI" {
            entry.target = Some(file_uri_to_path(&content));
        } else if let Ok(meta) = serde_json::from_str::<Value>(&content) {
            entry.size = meta.get("fileSize").and_then(Value::as_i64);
            entry.end_time = meta.get("endTime").and_then(Value::as_i64);
        }
    }

    for (place_id, d) in &downloads {
        let Some(end_time) = d.end_time.filter(|&t| t > 0) else {
            continue;
        };
        let stamp = Stamp::parse(end_time.to_string(), end_time_format(end_time), None)?;
        if !in_window(ctx, stamp.ts_utc) {
            continue;
        }
        let target = d.target.clone().unwrap_or_default();
        let size = d.size.unwrap_or(0);
        let size_text = human_size(size);
        let summary = if target.is_empty() {
            format!("{} ({size_text}, not saved)", d.url)
        } else {
            format!("{} ← {} ({size_text})", target, d.url)
        };
        if !target.is_empty() {
            sink.artifact(Artifact::referenced(PathBuf::from(&target), d.size));
        }
        sink.event(fp_core::Event {
            actor: Some(Actor::app(inst.app.as_str())),
            subject_host: extract_host(&d.url),
            attrs: json!({
                "target_path": target,
                "total_bytes": d.size,
                "source_url": d.url,
            }),
            ..new_event(
                ctx,
                inst,
                stamp,
                EventKind::WebDownload,
                d.url.clone(),
                summary,
                origin.at(format!("moz_annos.place_id={place_id}")),
            )
        });
    }
    Ok(())
}

fn collect_permissions(
    path: &std::path::Path,
    inst: &SourceInstance,
    ctx: &CollectCtx,
    sink: &mut Sink,
) -> Result<()> {
    let snapshot = copy_db_set(path, &stage_dir(ctx, inst.id)?)?;
    let conn = open_snapshot(&snapshot)?;
    if !table_has_columns(
        &conn,
        "moz_perms",
        &["id", "origin", "type", "permission", "modificationTime"],
    )? {
        return Ok(());
    }
    let origin = Origin {
        file: path,
        sha256: Some(&snapshot.sha256),
    };
    let mut stmt = conn.prepare(
        "SELECT id, origin, type, permission, modificationTime FROM moz_perms ORDER BY id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let site: String = row.get(1)?;
        let perm: String = row.get(2)?;
        if !PERMISSION_TYPES.contains(&perm.as_str()) {
            continue;
        }
        let setting: i64 = row.get(3)?;
        let modified: i64 = row.get(4)?;
        let decision = match setting {
            1 => "allow",
            2 => "deny",
            _ => "other",
        };

        sink.state(StateRecord {
            run_id: ctx.run_id,
            kind: "web.site_permission".into(),
            key: format!("{}|{site}|{perm}", inst.path.display()),
            value: json!({
                "browser": inst.app,
                "profile": inst.profile,
                "permission": perm,
                "origin": site,
                "setting": setting,
                "decision": decision,
            }),
            provenance: origin.at(format!("moz_perms.id={id}")),
        });

        if modified > 0 {
            let stamp = Stamp::parse(modified.to_string(), TimeFormat::PRTime, None)?;
            if in_window(ctx, stamp.ts_utc) {
                sink.event(fp_core::Event {
                    actor: Some(Actor::app(inst.app.as_str())),
                    subject_host: extract_host(&site),
                    attrs: json!({ "permission": perm, "setting": setting, "decision": decision }),
                    ..new_event(
                        ctx,
                        inst,
                        stamp,
                        EventKind::WebPermissionChange,
                        site.clone(),
                        format!("{site}: {perm} = {decision}"),
                        origin.at(format!("moz_perms.id={id}")),
                    )
                });
            }
        }
    }
    Ok(())
}

fn collect_extensions(
    path: &std::path::Path,
    inst: &SourceInstance,
    ctx: &CollectCtx,
    sink: &mut Sink,
) -> Result<()> {
    let file = read_evidence(path)?;
    let json: Value = serde_json::from_slice(&file.bytes)?;
    let Some(addons) = json.get("addons").and_then(Value::as_array) else {
        return Ok(());
    };
    let origin = Origin {
        file: path,
        sha256: Some(&file.sha256),
    };
    for addon in addons {
        let Some(obj) = addon.as_object() else {
            continue;
        };
        let id = obj.get("id").and_then(Value::as_str).unwrap_or("?");
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| {
                obj.get("defaultLocale")
                    .and_then(|l| l.get("name"))
                    .and_then(Value::as_str)
            })
            .unwrap_or(id);
        let version = obj.get("version").and_then(Value::as_str);
        // system/builtin addons ship with the app; state only, like chromium component extensions
        let location = obj.get("location").and_then(Value::as_str).unwrap_or("");
        let system = matches!(location, "app-system-defaults" | "app-system");

        sink.state(StateRecord {
            run_id: ctx.run_id,
            kind: "ext.installed".into(),
            key: format!("{}|{id}", inst.path.display()),
            value: json!({
                "browser": inst.app,
                "profile": inst.profile,
                "id": id,
                "name": name,
                "version": version,
                "location": location,
                "system": system,
                "active": obj.get("active"),
            }),
            provenance: origin.at(format!("addons.id={id}")),
        });

        if system {
            continue;
        }
        // installDate is milliseconds since the unix epoch
        let Some(raw) = obj.get("installDate").and_then(Value::as_i64) else {
            continue;
        };
        let stamp = Stamp::parse(raw.to_string(), TimeFormat::UnixMillis, None)?;
        if !in_window(ctx, stamp.ts_utc) {
            continue;
        }
        sink.event(fp_core::Event {
            actor: Some(Actor::app(inst.app.as_str())),
            attrs: json!({ "name": name, "version": version, "location": location }),
            ..new_event(
                ctx,
                inst,
                stamp,
                EventKind::ExtInstalled,
                id.to_string(),
                format!("{name} ({id})"),
                origin.at(format!("addons.id={id}")),
            )
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_profiles_with_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join(".mozilla/firefox/abc.default-release");
        std::fs::create_dir_all(&profile).unwrap();
        std::fs::write(profile.join("places.sqlite"), b"").unwrap();
        std::fs::create_dir_all(dir.path().join(".mozilla/firefox/emptyprofile")).unwrap();

        let env = Env::test(dir.path().to_path_buf(), PathBuf::from("/"));
        let instances = FirefoxCollector.discover(&env);
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].app, "Firefox");
        assert_eq!(instances[0].profile.as_deref(), Some("abc"));
    }

    #[test]
    fn profiles_ini_names_profiles_and_absolute_paths_stay_in_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home/u");
        let root = dir.path().join("image");
        let ff = home.join(".mozilla/firefox");
        std::fs::create_dir_all(ff.join("r4nd0m.default-esr")).unwrap();
        std::fs::write(ff.join("r4nd0m.default-esr/places.sqlite"), b"").unwrap();
        let elsewhere = root.join("data/ffprofile");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("places.sqlite"), b"").unwrap();
        std::fs::write(
            ff.join("profiles.ini"),
            "[Profile0]\nName=default-esr\nIsRelative=1\nPath=r4nd0m.default-esr\n\n\
             [Profile1]\nName=work\nIsRelative=0\nPath=/data/ffprofile\n",
        )
        .unwrap();

        let env = Env::test(home, root);
        let mut found: Vec<_> = FirefoxCollector::profiles(&env)
            .into_iter()
            .map(|(_, name, path)| (name, path))
            .collect();
        found.sort();
        assert_eq!(
            found,
            vec![
                ("default-esr".to_string(), ff.join("r4nd0m.default-esr")),
                ("work".to_string(), elsewhere),
            ]
        );
    }

    #[test]
    fn zen_and_librewolf_roots_are_covered() {
        let dir = tempfile::tempdir().unwrap();
        let zen = dir.path().join(".zen/xyz.default");
        std::fs::create_dir_all(&zen).unwrap();
        std::fs::write(zen.join("extensions.json"), b"{}").unwrap();

        let env = Env::test(dir.path().to_path_buf(), PathBuf::from("/"));
        let instances = FirefoxCollector.discover(&env);
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].app, "Zen");
    }

    #[test]
    fn download_end_time_in_ms_or_us() {
        let ms = Stamp::parse("1792170893000", end_time_format(1_792_170_893_000), None).unwrap();
        let us = Stamp::parse(
            "1792170893000000",
            end_time_format(1_792_170_893_000_000),
            None,
        )
        .unwrap();
        assert_eq!(ms.ts_utc, us.ts_utc);
    }

    #[test]
    fn redirect_and_plain_visits() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("places.sqlite");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE moz_places (id INTEGER PRIMARY KEY, url TEXT, title TEXT);
             CREATE TABLE moz_historyvisits (id INTEGER PRIMARY KEY, from_visit INTEGER,
                 place_id INTEGER NOT NULL, visit_date INTEGER NOT NULL, visit_type INTEGER NOT NULL);
             INSERT INTO moz_places VALUES (1, 'https://page-redirect.com/f', '');
             INSERT INTO moz_places VALUES (2, 'https://us09-web-zoom.com/j/1', 'Zoom');
             INSERT INTO moz_historyvisits VALUES (1, 0, 1, 1792172917000000, 1);
             INSERT INTO moz_historyvisits VALUES (2, 1, 2, 1792172918000000, 6);",
        )
        .unwrap();
        drop(conn);

        let stage = tempfile::tempdir().unwrap();
        let snapshot = copy_db_set(&db, stage.path()).unwrap();
        let conn = open_snapshot(&snapshot).unwrap();
        let inst = SourceInstance::new(
            "firefox.history",
            "Firefox",
            Some("abc".into()),
            dir.path().join("p"),
        );
        let ctx = CollectCtx {
            run_id: 1,
            since: None,
            until: None,
            assume_tz: None,
            temp_dir: dir.path().join("tmp"),
            own_files: Vec::new(),
        };
        let mut sink = Sink::default();
        collect_visits(
            &conn,
            &inst,
            &ctx,
            &Origin {
                file: std::path::Path::new("/p/places.sqlite"),
                sha256: None,
            },
            (i64::MIN, i64::MAX),
            &mut sink,
        )
        .unwrap();

        assert_eq!(sink.events.len(), 2);
        assert_eq!(sink.events[0].kind, EventKind::WebVisit);
        assert_eq!(sink.events[1].kind, EventKind::WebRedirect);
        assert_eq!(
            sink.events[1].summary,
            "https://page-redirect.com/f → https://us09-web-zoom.com/j/1 [temporary_redirect]"
        );
        assert_eq!(sink.events[1].attrs["from_visit"], 1);
    }
}
