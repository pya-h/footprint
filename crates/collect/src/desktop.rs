use crate::collector::{Collector, CollectCtx, Env, Sink};
use crate::utils::in_window;
use anyhow::Result;
use fp_core::{Actor, Confidence, Event, EventKind, Provenance, SourceInstance, SourceStatus, time::TimeFormat};
use serde_json::json;
use std::fs;

const XBEL: &str = ".local/share/recently-used.xbel";

pub struct DesktopCollector;

impl DesktopCollector {
    pub fn new() -> Self {
        Self
    }
}

impl Collector for DesktopCollector {
    fn kind(&self) -> &'static str {
        "desktop.recent"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let path = env.home.join(XBEL);
        if path.exists() {
            vec![SourceInstance {
                id: 1,
                kind: self.kind().to_string(),
                app: "gtk-recent".to_string(),
                profile: None,
                path,
                status: SourceStatus::Ok,
                note: None,
            }]
        } else {
            Vec::new()
        }
    }

    fn collect(
        &self,
        inst: &SourceInstance,
        ctx: &CollectCtx,
        sink: &Sink,
    ) -> Result<fp_core::CollectStats> {
        let text = fs::read_to_string(&inst.path)?;
        let doc: serde_json::Value = match quick_xml::de::from_str(&text) {
            Ok(v) => v,
            Err(_) => return Ok(fp_core::CollectStats::empty()),
        };

        if let Some(bookmarks) = doc.get("bookmark") {
            if let Some(arr) = bookmarks.as_array() {
                for (i, bm) in arr.iter().enumerate() {
                    let href = bm.get("href").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let added = bm.get("added").and_then(|v| v.as_str()).map(String::from);
                    let visited = bm.get("visited").and_then(|v| v.as_str()).map(String::from);
                    let modified = bm.get("modified").and_then(|v| v.as_str()).map(String::from);
                    let app = bm.get("applications")
                        .and_then(|a| a.get("application"))
                        .and_then(|a| a.get("name"))
                        .and_then(|v| v.as_str())
                        .map(String::from);

                    let ts_raw = modified.as_ref().or(visited.as_ref()).or(added.as_ref()).cloned();
                    let Some(raw) = ts_raw else { continue };
                    let Ok(norm) = fp_core::time::normalize(&raw, &TimeFormat::Rfc3339, None) else { continue };
                    if !in_window(ctx, norm.ts_utc) {
                        continue;
                    }

                    sink.push_event(Event {
                        run_id: ctx.run_id,
                        ts_utc: norm.ts_utc,
                        ts_precision: norm.precision,
                        ts_raw: raw,
                        ts_raw_format: TimeFormat::Rfc3339,
                        ts_tz_assumed: norm.tz_assumed,
                        source_kind: self.kind().to_string(),
                        source_inst: inst.id,
                        kind: EventKind::FileOpened,
                        actor: app.as_ref().map(|a| Actor {
                            uid: None,
                            user: None,
                            pid: None,
                            exe: None,
                            app: Some(a.clone()),
                        }),
                        subject: href.clone(),
                        subject_host: None,
                        summary: format!("{} [{}]", href, app.as_deref().unwrap_or("?")),
                        attrs: json!({
                            "added": added,
                            "visited": visited,
                            "modified": modified,
                            "application": app,
                            "index": i,
                        }),
                        provenance: Provenance {
                            file: inst.path.clone(),
                            file_sha256: None,
                            locator: format!("recently-used.xbel:bookmark[{i}]"),
                        },
                        confidence: Confidence::Direct,
                    });
                }
            }
        }
        Ok(fp_core::CollectStats::empty())
    }
}
