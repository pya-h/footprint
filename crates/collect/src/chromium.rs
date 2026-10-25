use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::event::{Origin, Stamp, new_event};
use crate::utils::{
    copy_db_set, extract_host, human_size, in_window, open_snapshot, read_evidence, stage_dir,
    table_has_columns,
};
use anyhow::{Context, Result};
use fp_core::time::TimeFormat;
use fp_core::{Actor, Artifact, EventKind, SourceInstance, StateRecord};
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// (browser, config dir relative to `~/.config`, flatpak app id)
const BROWSERS: &[(&str, &str, Option<&str>)] = &[
    ("Chrome", "google-chrome", Some("com.google.Chrome")),
    ("Chrome Beta", "google-chrome-beta", None),
    ("Chrome Dev", "google-chrome-unstable", None),
    ("Chromium", "chromium", Some("org.chromium.Chromium")),
    (
        "Brave",
        "BraveSoftware/Brave-Browser",
        Some("com.brave.Browser"),
    ),
    ("Brave Beta", "BraveSoftware/Brave-Browser-Beta", None),
    ("Brave Nightly", "BraveSoftware/Brave-Browser-Nightly", None),
    ("Edge", "microsoft-edge", Some("com.microsoft.Edge")),
    ("Edge Beta", "microsoft-edge-beta", None),
    ("Edge Dev", "microsoft-edge-dev", None),
    ("Vivaldi", "vivaldi", Some("com.vivaldi.Vivaldi")),
    ("Opera", "opera", Some("com.opera.Opera")),
];

/// Snap browsers keep their profile outside `~/.config`.
const SNAP_ROOTS: &[(&str, &str)] = &[
    ("Chromium", "snap/chromium/common/chromium"),
    (
        "Brave",
        "snap/brave/current/.config/BraveSoftware/Brave-Browser",
    ),
    ("Opera", "snap/opera/current/.config/opera"),
];

/// Content-setting types that are actual permissions a site asked for. Everything else under
/// `content_settings.exceptions` (engagement, client hints, shields metadata…) is context.
const PERMISSION_TYPES: &[&str] = &[
    "media_stream_camera",
    "media_stream_mic",
    "camera_pan_tilt_zoom",
    "notifications",
    "geolocation",
    "geolocation_with_options",
    "clipboard",
    "midi_sysex",
    "usb_guard",
    "serial_guard",
    "hid_guard",
    "bluetooth_guard",
    "bluetooth_scanning",
    "nfc_devices",
    "file_system_write_guard",
    "file_system_read_guard",
    "idle_detection",
    "sensors",
    "popups",
    "automatic_downloads",
    "protocol_handler",
    "window_placement",
    "local_fonts",
    "keyboard_lock",
    "pointer_lock",
    "display_media_system_audio",
    "captured_surface_control",
    "speaker_selection",
    "storage_access",
    "top_level_storage_access",
    "local_network_access",
    "direct_sockets",
    "payment_handler",
    "ar",
    "vr",
    "hand_tracking",
    "brave_ethereum",
    "brave_solana",
    "brave_cardano",
    "brave_google_sign_in",
];

/// `visits.transition` qualifier bits (`ui::PageTransition`).
const QUALIFIERS: &[(u32, &str)] = &[
    (0x0080_0000, "FORWARD_BACK"),
    (0x0100_0000, "FROM_ADDRESS_BAR"),
    (0x0200_0000, "HOME_PAGE"),
    (0x0400_0000, "FROM_API"),
    (0x1000_0000, "CHAIN_START"),
    (0x2000_0000, "CHAIN_END"),
    (0x4000_0000, "CLIENT_REDIRECT"),
    (0x8000_0000, "SERVER_REDIRECT"),
];
const CLIENT_REDIRECT: u32 = 0x4000_0000;
const SERVER_REDIRECT: u32 = 0x8000_0000;

/// `extensions.settings.<id>.location` values for extensions that ship inside the browser.
const COMPONENT_LOCATIONS: &[i64] = &[5, 10];

/// Microseconds between 1601-01-01 and 1970-01-01, for pushing the window into SQL.
const WEBKIT_EPOCH_OFFSET_US: i64 = 11_644_473_600_000_000;

pub struct ChromiumCollector;

impl ChromiumCollector {
    fn roots(env: &Env) -> Vec<(&'static str, PathBuf)> {
        let config = env.home.join(".config");
        let native = BROWSERS
            .iter()
            .map(|(name, dir, _)| (*name, config.join(dir)));
        let flatpak = BROWSERS.iter().filter_map(|(name, dir, app_id)| {
            Some((
                *name,
                env.home
                    .join(".var/app")
                    .join((*app_id)?)
                    .join("config")
                    .join(dir),
            ))
        });
        let snap = SNAP_ROOTS
            .iter()
            .map(|(name, rel)| (*name, env.home.join(rel)));
        native
            .chain(flatpak)
            .chain(snap)
            .filter(|(_, p)| p.is_dir())
            .collect()
    }

    fn collect_history(
        &self,
        inst: &SourceInstance,
        ctx: &CollectCtx,
        sink: &mut Sink,
    ) -> Result<()> {
        let history = inst.path.join("History");
        if !history.exists() {
            return Ok(());
        }
        let snapshot = copy_db_set(&history, &stage_dir(ctx, inst.id)?)?;
        let conn = open_snapshot(&snapshot)?;
        let origin = Origin {
            file: &history,
            sha256: Some(&snapshot.sha256),
        };
        let window = webkit_window(ctx);

        collect_visits(&conn, inst, ctx, &origin, window, sink).context("visits")?;
        if let Err(e) = collect_downloads(&conn, inst, ctx, &origin, window, sink) {
            sink.warn(format!("{}: downloads: {e:#}", history.display()));
        }
        if let Err(e) = collect_searches(&conn, inst, ctx, &origin, sink) {
            sink.warn(format!("{}: keyword searches: {e:#}", history.display()));
        }
        Ok(())
    }

    fn collect_preferences(
        &self,
        inst: &SourceInstance,
        ctx: &CollectCtx,
        sink: &mut Sink,
    ) -> Result<()> {
        let mut extensions = BTreeMap::new();
        for name in ["Preferences", "Secure Preferences"] {
            let path = inst.path.join(name);
            if !path.exists() {
                continue;
            }
            let file = read_evidence(&path)?;
            let json: Value =
                serde_json::from_slice(&file.bytes).with_context(|| path.display().to_string())?;
            let origin = Origin {
                file: &path,
                sha256: Some(&file.sha256),
            };
            if let Some(exceptions) = json
                .pointer("/profile/content_settings/exceptions")
                .and_then(Value::as_object)
            {
                collect_permissions(exceptions, inst, ctx, &origin, sink);
            }
            if let Some(settings) = json
                .pointer("/extensions/settings")
                .and_then(Value::as_object)
            {
                for (id, info) in settings {
                    let Some(info) = info.as_object() else {
                        continue;
                    };
                    let provenance = origin.at(format!("{name}:extensions.settings.{id}"));
                    let (merged, source) = extensions
                        .entry(id.clone())
                        .or_insert_with(|| (Map::new(), provenance.clone()));
                    merged.extend(info.clone());
                    *source = provenance;
                }
            }
        }
        for (id, (info, provenance)) in extensions {
            collect_extension(&id, &info, provenance, inst, ctx, sink);
        }
        Ok(())
    }
}

impl Collector for ChromiumCollector {
    fn per_user(&self) -> bool {
        true
    }

    fn kind(&self) -> &'static str {
        "chromium.history"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let mut out = Vec::new();
        for (app, root) in Self::roots(env) {
            for (profile, path) in list_profiles(&root) {
                let mut inst = SourceInstance::new(self.kind(), app, Some(profile), path);
                if !inst.path.join("History").exists() && !inst.path.join("Preferences").exists() {
                    inst.status = fp_core::Availability::Empty;
                    inst.note = Some("no History or Preferences".into());
                }
                out.push(inst);
            }
        }
        out
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        self.collect_history(inst, ctx, sink)?;
        if let Err(e) = self.collect_preferences(inst, ctx, sink) {
            sink.warn(format!("preferences: {e:#}"));
        }
        Ok(())
    }
}

fn webkit_window(ctx: &CollectCtx) -> (i64, i64) {
    (
        ctx.since
            .map_or(i64::MIN, |t| t.saturating_add(WEBKIT_EPOCH_OFFSET_US)),
        ctx.until
            .map_or(i64::MAX, |t| t.saturating_add(WEBKIT_EPOCH_OFFSET_US)),
    )
}

fn collect_visits(
    conn: &Connection,
    inst: &SourceInstance,
    ctx: &CollectCtx,
    origin: &Origin,
    (lo, hi): (i64, i64),
    sink: &mut Sink,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT v.id, v.visit_time, v.from_visit, v.transition, u.url, u.title, fu.url
         FROM visits v
         JOIN urls u ON u.id = v.url
         LEFT JOIN visits fv ON fv.id = v.from_visit AND v.from_visit != 0
         LEFT JOIN urls fu ON fu.id = fv.url
         WHERE v.visit_time BETWEEN ?1 AND ?2
         ORDER BY v.visit_time",
    )?;
    let mut rows = stmt.query([lo, hi])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let visit_time: i64 = row.get(1)?;
        let from_visit: i64 = row.get(2)?;
        let transition = row.get::<_, i64>(3)? as u32;
        let url: String = row.get(4)?;
        let title: Option<String> = row.get::<_, Option<String>>(5)?.filter(|t| !t.is_empty());
        let from_url: Option<String> = row.get(6)?;

        let stamp = Stamp::parse(visit_time.to_string(), TimeFormat::WebKitMicros, None)?;
        let redirect = match transition {
            t if t & SERVER_REDIRECT != 0 => Some("3xx"),
            t if t & CLIENT_REDIRECT != 0 => Some("client"),
            _ => None,
        };
        let (kind, summary) = match (redirect, &from_url) {
            (Some(how), Some(from)) => (EventKind::WebRedirect, format!("{from} → {url} [{how}]")),
            (Some(how), None) => (EventKind::WebRedirect, format!("→ {url} [{how}]")),
            (None, _) => (
                EventKind::WebVisit,
                title
                    .as_ref()
                    .map_or_else(|| url.clone(), |t| format!("{url} — {t}")),
            ),
        };
        let qualifiers: Vec<&str> = QUALIFIERS
            .iter()
            .filter(|(bit, _)| transition & bit != 0)
            .map(|(_, name)| *name)
            .collect();

        sink.event(fp_core::Event {
            actor: Some(Actor::app(inst.app.as_str())),
            subject_host: extract_host(&url),
            attrs: json!({
                "visit_id": id,
                "from_visit": from_visit,
                "from_url": from_url,
                "title": title,
                "transition": transition,
                "transition_core": transition & 0xFF,
                "transition_qualifiers": qualifiers,
            }),
            ..new_event(
                ctx,
                inst,
                stamp,
                kind,
                url,
                summary,
                origin.at(format!("visits.id={id}")),
            )
        });
    }
    Ok(())
}

struct Download {
    start_time: i64,
    target_path: String,
    total_bytes: i64,
    state: i64,
    tab_url: Option<String>,
    referrer: Option<String>,
    mime_type: Option<String>,
    chain: Vec<String>,
}

/// `DownloadItem::DownloadState`
fn download_state(state: i64) -> &'static str {
    match state {
        0 => "in_progress",
        1 => "complete",
        2 => "cancelled",
        3 | 4 => "interrupted",
        _ => "unknown",
    }
}

fn collect_downloads(
    conn: &Connection,
    inst: &SourceInstance,
    ctx: &CollectCtx,
    origin: &Origin,
    (lo, hi): (i64, i64),
    sink: &mut Sink,
) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT d.id, d.start_time, d.target_path, d.total_bytes, d.state, d.tab_url, d.referrer,
                d.mime_type, c.url
         FROM downloads d
         LEFT JOIN downloads_url_chains c ON c.id = d.id
         WHERE d.start_time BETWEEN ?1 AND ?2
         ORDER BY d.id, c.chain_index",
    )?;
    let mut downloads: BTreeMap<i64, Download> = BTreeMap::new();
    let mut rows = stmt.query([lo, hi])?;
    while let Some(row) = rows.next()? {
        let chain_url: Option<String> = row.get(8)?;
        let entry = match downloads.entry(row.get(0)?) {
            std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::btree_map::Entry::Vacant(e) => e.insert(Download {
                start_time: row.get(1)?,
                target_path: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                total_bytes: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                state: row.get::<_, Option<i64>>(4)?.unwrap_or(-1),
                tab_url: row.get::<_, Option<String>>(5)?.filter(|s| !s.is_empty()),
                referrer: row.get::<_, Option<String>>(6)?.filter(|s| !s.is_empty()),
                mime_type: row.get::<_, Option<String>>(7)?.filter(|s| !s.is_empty()),
                chain: Vec::new(),
            }),
        };
        entry.chain.extend(chain_url);
    }

    for (id, d) in downloads {
        let stamp = Stamp::parse(d.start_time.to_string(), TimeFormat::WebKitMicros, None)?;
        let url = d
            .chain
            .last()
            .or(d.tab_url.as_ref())
            .cloned()
            .unwrap_or_default();
        let size = human_size(d.total_bytes);
        let state = download_state(d.state);
        let summary = if d.target_path.is_empty() {
            format!("{url} ({size}, {state}, not saved)")
        } else {
            format!("{} ← {url} ({size}, {state})", d.target_path)
        };
        if !d.target_path.is_empty() {
            sink.artifact(Artifact::referenced(
                PathBuf::from(&d.target_path),
                Some(d.total_bytes),
            ));
        }
        sink.event(fp_core::Event {
            actor: Some(Actor::app(inst.app.as_str())),
            subject_host: extract_host(&url),
            attrs: json!({
                "download_id": id,
                "target_path": d.target_path,
                "total_bytes": d.total_bytes,
                "state": state,
                "mime_type": d.mime_type,
                "tab_url": d.tab_url,
                "referrer": d.referrer,
                "url_chain": d.chain,
            }),
            ..new_event(
                ctx,
                inst,
                stamp,
                EventKind::WebDownload,
                url,
                summary,
                origin.at(format!("downloads.id={id}")),
            )
        });
    }
    Ok(())
}

fn collect_searches(
    conn: &Connection,
    inst: &SourceInstance,
    ctx: &CollectCtx,
    origin: &Origin,
    sink: &mut Sink,
) -> Result<()> {
    if !table_has_columns(conn, "keyword_search_terms", &["url_id", "term"])? {
        return Ok(());
    }
    let mut stmt = conn.prepare(
        "SELECT k.url_id, k.term, u.url, u.last_visit_time
         FROM keyword_search_terms k JOIN urls u ON u.id = k.url_id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let url_id: i64 = row.get(0)?;
        let term: String = row.get(1)?;
        let url: String = row.get(2)?;
        let stamp = Stamp::parse(
            row.get::<_, i64>(3)?.to_string(),
            TimeFormat::WebKitMicros,
            None,
        )?;
        if !in_window(ctx, stamp.ts_utc) {
            continue;
        }
        sink.event(fp_core::Event {
            actor: Some(Actor::app(inst.app.as_str())),
            subject_host: extract_host(&url),
            attrs: json!({ "url": url, "time_is": "url last_visit_time" }),
            ..new_event(
                ctx,
                inst,
                stamp,
                EventKind::WebSearch,
                term.clone(),
                format!("search: {term}"),
                origin.at(format!("keyword_search_terms.url_id={url_id}")),
            )
        });
    }
    Ok(())
}

fn collect_permissions(
    exceptions: &Map<String, Value>,
    inst: &SourceInstance,
    ctx: &CollectCtx,
    origin: &Origin,
    sink: &mut Sink,
) {
    for (perm, origins) in exceptions
        .iter()
        .filter(|(k, _)| PERMISSION_TYPES.contains(&k.as_str()))
    {
        let Some(origins) = origins.as_object() else {
            continue;
        };
        for (pattern, entry) in origins {
            let site = pattern.split(',').next().unwrap_or(pattern);
            let setting = entry.get("setting").and_then(Value::as_i64);
            let last_modified = entry.get("last_modified").and_then(Value::as_str);
            let last_used = entry.get("last_used").and_then(Value::as_str);
            let decision = match setting {
                Some(1) => "allow",
                Some(2) => "block",
                Some(3) => "ask",
                Some(_) => "other",
                None => "not persisted",
            };
            let locator = format!("profile.content_settings.exceptions.{perm}.{pattern}");

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
                    "last_modified": last_modified,
                    "last_used": last_used,
                }),
                provenance: origin.at(locator.clone()),
            });

            let changes = [
                (
                    last_modified.filter(|_| setting.is_some()),
                    EventKind::WebPermissionChange,
                    format!("{site}: {perm} = {decision}"),
                ),
                (
                    last_used,
                    EventKind::WebPermissionUsed,
                    format!("{site}: {perm} used ({decision})"),
                ),
            ];
            for (raw, kind, summary) in changes {
                let Some(stamp) =
                    raw.and_then(|r| Stamp::parse(r, TimeFormat::WebKitMicros, None).ok())
                else {
                    continue;
                };
                if stamp.ts_utc <= 0 || !in_window(ctx, stamp.ts_utc) {
                    continue;
                }
                sink.event(fp_core::Event {
                    actor: Some(Actor::app(inst.app.as_str())),
                    subject_host: extract_host(site),
                    attrs: json!({ "permission": perm, "setting": setting, "decision": decision }),
                    ..new_event(
                        ctx,
                        inst,
                        stamp,
                        kind,
                        site.to_string(),
                        summary,
                        origin.at(locator.clone()),
                    )
                });
            }
        }
    }
}

fn collect_extension(
    id: &str,
    info: &Map<String, Value>,
    provenance: fp_core::Provenance,
    inst: &SourceInstance,
    ctx: &CollectCtx,
    sink: &mut Sink,
) {
    let name = info
        .get("manifest")
        .and_then(|m| m.get("name"))
        .and_then(Value::as_str)
        .unwrap_or(id);
    let version = info
        .get("manifest")
        .and_then(|m| m.get("version"))
        .and_then(Value::as_str);
    let location = info.get("location").and_then(Value::as_i64);
    let component = location.is_some_and(|l| COMPONENT_LOCATIONS.contains(&l));
    let install_time = info
        .get("first_install_time")
        .or_else(|| info.get("install_time"))
        .and_then(Value::as_str);

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
            "component": component,
            "from_webstore": info.get("from_webstore"),
            "install_time": install_time,
            "path": info.get("path"),
            "disable_reasons": info.get("disable_reasons"),
        }),
        provenance: provenance.clone(),
    });

    if component {
        return;
    }
    let Some(stamp) =
        install_time.and_then(|r| Stamp::parse(r, TimeFormat::WebKitMicros, None).ok())
    else {
        return;
    };
    if !in_window(ctx, stamp.ts_utc) {
        return;
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
            provenance,
        )
    });
}

/// `Default/` and `Profile N/`; a root that holds `History` itself (Opera) is a profile too.
fn list_profiles(root: &Path) -> Vec<(String, PathBuf)> {
    let mut profiles = Vec::new();
    let default = root.join("Default");
    if default.is_dir() {
        profiles.push(("Default".to_string(), default));
    }
    if root.join("History").exists() {
        let name = if profiles.is_empty() {
            "Default"
        } else {
            "root"
        };
        profiles.push((name.to_string(), root.to_path_buf()));
    }
    let mut numbered: Vec<(String, PathBuf)> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            (name.starts_with("Profile ") && entry.path().is_dir()).then(|| (name, entry.path()))
        })
        .collect();
    numbered.sort();
    profiles.extend(numbered);
    profiles
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn profiles_default_and_numbered() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for d in [
            "Default",
            "Profile 2",
            "Profile 1",
            "System Profile",
            "Guest Profile",
        ] {
            fs::create_dir(root.join(d)).unwrap();
        }
        let got: Vec<_> = list_profiles(root).into_iter().map(|(n, _)| n).collect();
        assert_eq!(got, ["Default", "Profile 1", "Profile 2"]);
    }

    #[test]
    fn opera_root_profile_points_at_root() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("History"), b"").unwrap();
        assert_eq!(
            list_profiles(tmp.path()),
            vec![("Default".to_string(), tmp.path().to_path_buf())]
        );
    }

    #[test]
    fn flatpak_roots_use_explicit_app_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let brave = tmp
            .path()
            .join(".var/app/com.brave.Browser/config/BraveSoftware/Brave-Browser");
        let decoy = tmp
            .path()
            .join(".var/app/com.google.Chrome/config/BraveSoftware/Brave-Browser");
        fs::create_dir_all(&brave).unwrap();
        fs::create_dir_all(&decoy).unwrap();
        let env = Env::test(tmp.path().into(), "/".into());
        assert_eq!(ChromiumCollector::roots(&env), vec![("Brave", brave)]);
    }
}
