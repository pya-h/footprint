use anyhow::Result;
use fp_core::{Confidence, Link, LinkType};
use fp_store::{ArtifactRow, Store};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Correlates downloads with artifacts on disk and heuristically links download manager entries to browser visits.
pub fn correlate_downloads(store: &mut Store, root: &Path) -> Result<usize> {
    let mut total_links = 0;
    total_links += link_downloads_to_artifacts(store, root)?;
    total_links += link_download_manager_to_browser(store)?;
    Ok(total_links)
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut File::open(path)?, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

/// File times are set by whoever wrote the file: saturate rather than overflow i64 ns.
fn file_time_ns(sec: i64, nsec: i64) -> i64 {
    sec.saturating_mul(1_000_000_000).saturating_add(nsec)
}

/// Where a recorded path lives now: inside `root` first (a mounted image), then as recorded
/// (a live `--home` used next to an image `--root`).
pub(crate) fn resolve_path(recorded: &str, root: &Path) -> Option<PathBuf> {
    let p = Path::new(recorded);
    let in_root = root.join(p.strip_prefix("/").unwrap_or(p));
    [in_root, p.to_path_buf()].into_iter().find(|c| c.exists())
}

fn link_downloads_to_artifacts(store: &mut Store, root: &Path) -> Result<usize> {
    let download_events = store.get_events_by_kinds(&["web.download", "dl.manager_download"])?;
    let mut links = Vec::new();

    for ev in &download_events {
        let target_path = extract_target_path(ev);
        let Some(target) = target_path.filter(|p| !p.is_empty()) else {
            continue;
        };

        // Find or get artifact
        let art: Option<ArtifactRow> = store.get_artifact_by_path(&target)?;
        let art = match art {
            Some(a) => a,
            None => {
                // Check if file exists to get initial size
                let size = resolve_path(&target, root)
                    .and_then(|p| std::fs::metadata(&p).ok())
                    .map(|m| m.len() as i64);
                // Insert into artifacts table
                store.conn().execute(
                    "INSERT INTO artifacts (path, size, first_seen_run) VALUES (?1, ?2, ?3)
                     ON CONFLICT(path) DO NOTHING",
                    rusqlite::params![target, size, ev.run_id],
                )?;
                match store.get_artifact_by_path(&target)? {
                    Some(a) => a,
                    None => continue,
                }
            }
        };

        // a download target named like a wallet or key file is hashed by nobody
        // only a regular file is opened: a FIFO planted at a download's path would block
        if art.sha256.is_none()
            && !fp_core::deny::is_secret(Path::new(&target))
            && let Some(real_path) = resolve_path(&target, root)
            && let Ok(meta) = std::fs::metadata(&real_path)
            && meta.is_file()
        {
            let sha256 = sha256_file(&real_path).ok();
            store.update_artifact_metadata(
                art.id,
                &fp_store::ArtifactMeta {
                    sha256: sha256.as_deref(),
                    size: Some(meta.len() as i64),
                    mtime_ns: Some(file_time_ns(meta.mtime(), meta.mtime_nsec())),
                    ctime_ns: Some(file_time_ns(meta.ctime(), meta.ctime_nsec())),
                    btime_ns: None,
                    owner_pkg: None,
                },
            )?;
        }

        // Link artifact to download event
        links.push(Link {
            src_type: LinkType::Artifact,
            src_id: art.id,
            rel: "downloaded_as".to_string(),
            dst_type: LinkType::Event,
            dst_id: ev.id,
            confidence: Confidence::Direct,
            why: Some("download target path matches artifact path".to_string()),
        });
    }

    Ok(store.insert_links(&links)?)
}

fn extract_target_path(ev: &fp_store::EventDetail) -> Option<String> {
    let attrs_str = ev.attrs_json.as_deref()?;
    let attrs: Value = serde_json::from_str(attrs_str).ok()?;

    if let Some(target) = attrs.get("target_path").and_then(Value::as_str)
        && !target.is_empty()
    {
        return Some(target.to_string());
    }

    if let (Some(targetdir), Some(name)) = (
        attrs.get("targetdir").and_then(Value::as_str),
        attrs.get("name").and_then(Value::as_str),
    ) {
        let path = Path::new(targetdir).join(name);
        return Some(path.to_string_lossy().to_string());
    }

    None
}

fn domains_match(a: &str, b: &str) -> bool {
    a == b
        || a.ends_with(&format!(".{b}"))
        || b.ends_with(&format!(".{a}"))
        || a.split('.').rev().take(2).collect::<Vec<_>>()
            == b.split('.').rev().take(2).collect::<Vec<_>>()
}

fn link_download_manager_to_browser(store: &mut Store) -> Result<usize> {
    let dlmgr_events = store.get_events_by_kinds(&["dl.manager_download"])?;
    let visit_events = store.get_events_by_kinds(&["web.visit", "web.redirect"])?;
    let browser_downloads = store.get_events_by_kinds(&["web.download"])?;

    let mut links = Vec::new();

    for dl in &dlmgr_events {
        let dl_host = match &dl.subject_host {
            Some(h) => h.as_str(),
            None => continue,
        };

        // Window: up to 15 minutes before the download
        let window_us = 15 * 60 * 1_000_000;
        let mut candidates = visit_events
            .iter()
            .filter(|v| {
                v.ts_utc <= dl.ts_utc
                    && (dl.ts_utc - v.ts_utc) <= window_us
                    && v.subject_host
                        .as_deref()
                        .is_some_and(|vh| domains_match(vh, dl_host))
            })
            .collect::<Vec<_>>();

        // Sort descending by timestamp (closest visit first)
        candidates.sort_by_key(|v| std::cmp::Reverse(v.ts_utc));

        // Prioritize a visit whose subject contains "download"
        let best_visit = candidates
            .iter()
            .find(|v| v.subject.as_deref().is_some_and(|s| s.contains("download")))
            .or_else(|| candidates.first());

        if let Some(visit) = best_visit {
            let diff_m = (dl.ts_utc - visit.ts_utc) as f64 / 60_000_000.0;
            let why = format!(
                "heuristic: download manager entry within {:.1}m of {} visit",
                diff_m,
                visit.subject_host.as_deref().unwrap_or("browser")
            );

            // Check if there was an intercepted browser download row
            let intercepted = browser_downloads.iter().any(|bd| {
                bd.ts_utc <= dl.ts_utc
                    && (dl.ts_utc - bd.ts_utc) <= window_us
                    && (bd
                        .summary
                        .as_deref()
                        .is_some_and(|s| s.contains("cancelled") || s.contains("not saved"))
                        || bd.attrs_json.as_deref().is_some_and(|a| {
                            a.contains("\"total_bytes\":0") || a.contains("\"state\":\"cancelled\"")
                        }))
            });

            let full_why = if intercepted {
                format!("{why} (browser download cancelled/intercepted)")
            } else {
                why
            };

            links.push(Link {
                src_type: LinkType::Event,
                src_id: dl.id,
                rel: "navigated_from".to_string(),
                dst_type: LinkType::Event,
                dst_id: visit.id,
                confidence: Confidence::Heuristic,
                why: Some(full_why),
            });
        }
    }

    Ok(store.insert_links(&links)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_core::time::TimeFormat;
    use fp_core::{Actor, Confidence, Event, EventKind, Provenance, SourceInstance, TimePrecision};
    use fp_store::{Batch, NewRun};
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn a_fifo_at_a_download_path_is_never_opened() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("zoom_amd64.deb");
        let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) }, 0);

        let mut store = Store::open_or_create(dir.path().join("case.db")).unwrap();
        let run = store
            .insert_run(&NewRun {
                host: "h",
                user: "u",
                uid: 1000,
                euid: 1000,
                version: "0",
                tz: "UTC",
                args: "",
            })
            .unwrap();
        let inst = SourceInstance::new("dlmgr.xdm", "xdm", None, PathBuf::from("/x"));
        let id = store.insert_source(run, &inst).unwrap();
        let event = Event {
            run_id: run,
            source_kind: inst.kind.clone(),
            ts_utc: 1,
            ts_precision: TimePrecision::Exact,
            ts_raw: "1".into(),
            ts_raw_format: TimeFormat::UnixSeconds,
            ts_tz_assumed: None,
            source_inst: id,
            kind: EventKind::DlManagerDownload,
            actor: Some(Actor::app("xdm")),
            subject: "https://example.com/zoom_amd64.deb".into(),
            subject_host: Some("example.com".into()),
            summary: String::new(),
            attrs: json!({ "target_path": fifo.to_str().unwrap() }),
            provenance: Provenance {
                file: PathBuf::from("/x"),
                file_sha256: None,
                locator: "downloads.id=1".into(),
            },
            confidence: Confidence::Direct,
        };
        store
            .ingest(
                run,
                &Batch {
                    source_identity: &inst.identity(),
                    events: &[event],
                    states: &[],
                    artifacts: &[],
                },
            )
            .unwrap();

        // returns instead of blocking on the FIFO; the artifact stays unhashed
        correlate_downloads(&mut store, Path::new("/")).unwrap();
        let artifact = store
            .get_artifact_by_path(fifo.to_str().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(artifact.sha256, None);
    }
}
