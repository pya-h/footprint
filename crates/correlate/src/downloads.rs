use anyhow::Result;
use fp_core::{Confidence, Link, LinkType};
use fp_store::{ArtifactRow, Store};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{self, Read};
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
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn resolve_path(target_path: &str, root: &Path) -> Option<PathBuf> {
    let p = Path::new(target_path);
    if p.exists() {
        return Some(p.to_path_buf());
    }
    if let Ok(stripped) = p.strip_prefix("/") {
        let candidate = root.join(stripped);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
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

        // If artifact doesn't have sha256/mtime yet, stat & hash it if present on disk
        if art.sha256.is_none()
            && let Some(real_path) = resolve_path(&target, root)
            && let Ok(meta) = std::fs::metadata(&real_path)
        {
            let size = meta.len() as i64;
            let mtime_ns = meta.mtime_nsec();
            let ctime_ns = meta.ctime_nsec();
            let sha256 = sha256_file(&real_path).ok();
            let _ = store.update_artifact_metadata(
                art.id,
                &fp_store::ArtifactMeta {
                    sha256: sha256.as_deref(),
                    size: Some(size),
                    mtime_ns: Some(mtime_ns),
                    ctime_ns: Some(ctime_ns),
                    btime_ns: None,
                    owner_pkg: None,
                },
            );
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
