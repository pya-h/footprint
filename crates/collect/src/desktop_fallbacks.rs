//! Desktop activity fallbacks: KDE RecentDocuments, kactivitymanagerd and Zeitgeist.

use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::event::{Origin, Stamp, new_event};
use crate::utils::{file_uri_to_path, in_window, read_evidence};
use anyhow::Result;
use fp_core::time::TimeFormat;
use fp_core::{Actor, Availability, EventKind, SourceInstance};
use serde_json::json;
use std::fs;

const KDE_RECENT: &str = ".local/share/RecentDocuments";
const ZEITGEIST_DB: &str = ".local/share/zeitgeist/activity.sqlite";
const KACTIVITY_DB: &str = ".local/share/kactivitymanagerd/resources/database";

pub struct KdeRecentCollector;

impl Collector for KdeRecentCollector {
    fn per_user(&self) -> bool {
        true
    }

    fn kind(&self) -> &'static str {
        "desktop.kde"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let path = env.home.join(KDE_RECENT);
        if !path.is_dir() {
            let mut inst = SourceInstance::new(self.kind(), "kde-recent", None, path);
            inst.status = Availability::NotInstalled;
            return vec![inst];
        }
        vec![SourceInstance::new(self.kind(), "kde-recent", None, path)]
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
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let file = match read_evidence(&path) {
                Ok(file) => file,
                Err(e) => {
                    sink.warn(format!("{}: {e:#}", path.display()));
                    continue;
                }
            };
            let text = String::from_utf8_lossy(&file.bytes);
            let mut target_url = None;
            for line in text.lines() {
                let line = line.trim();
                if let Some(url) = line
                    .strip_prefix("URL=")
                    .or_else(|| line.strip_prefix("URL[$e]="))
                {
                    target_url = Some(url.to_string());
                    break;
                }
            }

            let Some(url) = target_url else {
                continue;
            };
            let meta = fs::metadata(&path)?;
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

            let target_path = file_uri_to_path(&url);
            let summary = format!("{target_path} [KDE]");
            let origin = Origin {
                file: &path,
                sha256: Some(&file.sha256),
            };

            sink.event(fp_core::Event {
                actor: Some(Actor::app("kde")),
                attrs: json!({
                    "url": url,
                    "target_path": target_path,
                    "desktop_file": path.to_string_lossy(),
                }),
                ..new_event(
                    ctx,
                    inst,
                    stamp,
                    EventKind::FileOpened,
                    target_path,
                    summary,
                    origin.at(format!("file:{}", path.display())),
                )
            });
        }
        Ok(())
    }
}

/// Detect-only until a parser is verified against a real Plasma database.
pub struct KActivityCollector;

impl Collector for KActivityCollector {
    fn kind(&self) -> &'static str {
        "desktop.kactivity"
    }

    fn per_user(&self) -> bool {
        true
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let mut inst = SourceInstance::new(
            self.kind(),
            "kactivitymanagerd",
            None,
            env.home.join(KACTIVITY_DB),
        );
        inst.status = if inst.path.exists() {
            Availability::Unsupported {
                reason: "kactivitymanagerd detected; parser not yet implemented".into(),
            }
        } else {
            Availability::NotInstalled
        };
        vec![inst]
    }

    fn collect(&self, _inst: &SourceInstance, _ctx: &CollectCtx, _sink: &mut Sink) -> Result<()> {
        Ok(())
    }
}

pub struct ZeitgeistCollector;

impl Collector for ZeitgeistCollector {
    fn per_user(&self) -> bool {
        true
    }

    fn kind(&self) -> &'static str {
        "desktop.zeitgeist"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let path = env.home.join(ZEITGEIST_DB);
        let mut inst = SourceInstance::new(self.kind(), "zeitgeist", None, path);
        if !inst.path.exists() {
            inst.status = Availability::NotInstalled;
        } else {
            inst.status = Availability::Unsupported {
                reason: "zeitgeist detected; parser not yet implemented".into(),
            };
        }
        vec![inst]
    }

    fn collect(&self, _inst: &SourceInstance, _ctx: &CollectCtx, _sink: &mut Sink) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_kde_recent_desktop_file() {
        let dir = tempfile::tempdir().unwrap();
        let kde_dir = dir.path().join("RecentDocuments");
        fs::create_dir_all(&kde_dir).unwrap();
        let entry = kde_dir.join("zoom.deb.desktop");
        fs::write(
            &entry,
            "[Desktop Entry]\nType=Link\nURL=file:///home/paya/Downloads/zoom_amd64.deb\n",
        )
        .unwrap();

        let inst = SourceInstance::new("desktop.kde", "kde-recent", None, kde_dir);
        let ctx = CollectCtx {
            run_id: 1,
            since: None,
            until: None,
            assume_tz: None,
            temp_dir: dir.path().join("tmp"),
        };
        let mut sink = Sink::default();
        KdeRecentCollector.collect(&inst, &ctx, &mut sink).unwrap();

        assert_eq!(sink.events.len(), 1);
        assert_eq!(sink.events[0].kind, EventKind::FileOpened);
        assert_eq!(
            sink.events[0].subject,
            "/home/paya/Downloads/zoom_amd64.deb"
        );
    }
}
