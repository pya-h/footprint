//! Download fallbacks: xattr origin URLs in download directories,
//! and detect-only download managers (uGet, Motrix, aria2, KGet, JDownloader).

use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::event::{Origin, Stamp, new_event};
use crate::probe::Probe;
use crate::utils::{extract_host, human_size, in_window, read_evidence};
use anyhow::Result;
use fp_core::time::TimeFormat;
use fp_core::{Actor, Artifact, Availability, EventKind, SourceInstance};
use serde_json::json;
use std::fs;

const KNOWN_MANAGERS: &[(&str, &str)] = &[
    ("uget", ".config/uGet"),
    ("motrix", ".config/Motrix"),
    ("aria2", ".aria2"),
    ("kget", ".local/share/kget"),
    ("jdownloader", ".jd"),
    ("jdownloader", ".jdownloader"),
];

pub struct XattrDownloadCollector;

/// Download directories: `XDG_DOWNLOAD_DIR` from `user-dirs.dirs` (localized names land
/// there), falling back to `~/Downloads`. `$HOME` in the value expands to the user's home; an
/// absolute value resolves under the environment's root, never the host's.
fn download_dirs(env: &Env) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    let config = env.home.join(".config/user-dirs.dirs");
    if let Ok(file) = read_evidence(&config) {
        for line in String::from_utf8_lossy(&file.bytes).lines() {
            let Some(value) = line.trim().strip_prefix("XDG_DOWNLOAD_DIR=") else {
                continue;
            };
            let value = value.trim().trim_matches('"');
            let path = match value.strip_prefix("$HOME") {
                Some(rest) => env.home.join(rest.trim_start_matches('/')),
                None if value.starts_with('/') => env.system_path(value),
                None => continue,
            };
            if !dirs.contains(&path) {
                dirs.push(path);
            }
        }
    }
    let default = env.home.join("Downloads");
    if !dirs.contains(&default) {
        dirs.push(default);
    }
    dirs
}

impl Collector for XattrDownloadCollector {
    fn per_user(&self) -> bool {
        true
    }

    fn kind(&self) -> &'static str {
        "dlmgr.xattr"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let dirs = download_dirs(env);
        let present: Vec<_> = dirs.iter().filter(|d| d.is_dir()).collect();
        if present.is_empty() {
            let mut inst = SourceInstance::new(self.kind(), "xattr", None, dirs[0].clone());
            inst.status = Availability::NotInstalled;
            return vec![inst];
        }
        present
            .into_iter()
            .map(|d| SourceInstance::new(self.kind(), "xattr", None, d.clone()))
            .collect()
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let entries = match fs::read_dir(&inst.path) {
            Ok(iter) => iter,
            Err(e) => {
                sink.warn(format!("cannot read {}: {e}", inst.path.display()));
                return Ok(());
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(origin_url) = Probe::get_xattr(&path, "user.xdg.origin.url") else {
                continue;
            };
            let referrer_url = Probe::get_xattr(&path, "user.xdg.referrer.url");
            // a file removed between the listing and the stat is gone, not an error
            let Ok(meta) = fs::metadata(&path) else {
                continue;
            };
            let size = meta.len() as i64;
            let mtime_ns = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0);

            let stamp = Stamp::parse(mtime_ns.to_string(), TimeFormat::FileTime, None)?;
            if !in_window(ctx, stamp.ts_utc) {
                continue;
            }

            let file_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            let summary = format!(
                "{} ← {origin_url} ({}) [xattr]",
                path.display(),
                human_size(size)
            );
            let origin = Origin {
                file: &inst.path,
                sha256: None,
            };

            sink.artifact(Artifact::referenced(path.clone(), Some(size)));
            sink.event(fp_core::Event {
                actor: Some(Actor::app("xattr")),
                subject_host: extract_host(&origin_url),
                attrs: json!({
                    "target_path": path.to_string_lossy(),
                    "file_name": file_name,
                    "origin_url": origin_url,
                    "referrer_url": referrer_url,
                    "size": size,
                }),
                ..new_event(
                    ctx,
                    inst,
                    stamp,
                    EventKind::WebDownload,
                    origin_url,
                    summary,
                    origin.at(format!("xattr:{}", path.display())),
                )
            });
        }
        Ok(())
    }
}

pub struct DlmgrDetectCollector;

impl Collector for DlmgrDetectCollector {
    fn per_user(&self) -> bool {
        true
    }

    fn kind(&self) -> &'static str {
        "dlmgr.detect"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let mut out = Vec::new();
        for &(app, rel) in KNOWN_MANAGERS {
            let path = env.home.join(rel);
            let mut inst = SourceInstance::new(self.kind(), app, None, path.clone());
            if path.exists() {
                inst.status = Availability::Unsupported {
                    reason: format!(
                        "detected {app} at {}; parser not yet implemented",
                        path.display()
                    ),
                };
            } else {
                inst.status = Availability::NotInstalled;
            }
            out.push(inst);
        }
        out
    }

    fn collect(&self, _inst: &SourceInstance, _ctx: &CollectCtx, _sink: &mut Sink) -> Result<()> {
        // detect-only; never collects
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn honors_xdg_download_dir() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home/u");
        std::fs::create_dir_all(home.join(".config")).unwrap();
        std::fs::create_dir_all(home.join("Descargas")).unwrap();
        // localized download dir via XDG: the fallback name differs from the default
        std::fs::write(
            home.join(".config/user-dirs.dirs"),
            "XDG_DOWNLOAD_DIR=\"$HOME/Descargas\"\nXDG_DESKTOP_DIR=\"$HOME/Desktop\"\n",
        )
        .unwrap();

        let env = Env::test(home.clone(), PathBuf::from("/"));
        let instances = XattrDownloadCollector.discover(&env);
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].path, home.join("Descargas"));

        // an absolute value is read inside the environment's root, not on the host
        let root = dir.path().join("image");
        std::fs::create_dir_all(root.join("data/dl")).unwrap();
        std::fs::write(
            home.join(".config/user-dirs.dirs"),
            "XDG_DOWNLOAD_DIR=\"/data/dl\"\n",
        )
        .unwrap();
        let instances = XattrDownloadCollector.discover(&Env::test(home.clone(), root.clone()));
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].path, root.join("data/dl"));

        // without a config the default ~/Downloads is used
        std::fs::remove_file(home.join(".config/user-dirs.dirs")).unwrap();
        let instances = XattrDownloadCollector.discover(&env);
        assert_eq!(instances[0].path, home.join("Downloads"));
        assert_eq!(instances[0].status, Availability::NotInstalled);
    }

    #[test]
    fn discovers_detect_managers() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::test(dir.path().to_path_buf(), PathBuf::from("/"));
        let instances = DlmgrDetectCollector.discover(&env);
        assert!(!instances.is_empty());
        assert!(
            instances
                .iter()
                .all(|i| i.status == Availability::NotInstalled)
        );

        // simulate detected uget
        fs::create_dir_all(dir.path().join(".config/uGet")).unwrap();
        let instances = DlmgrDetectCollector.discover(&env);
        let uget = instances.iter().find(|i| i.app == "uget").unwrap();
        assert!(matches!(uget.status, Availability::Unsupported { .. }));
    }
}
