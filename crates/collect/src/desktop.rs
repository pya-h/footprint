use crate::collector::{CollectCtx, Collector, Env, Sink};
use crate::event::{Origin, Stamp, new_event};
use crate::utils::{file_uri_to_path, in_window, read_evidence};
use anyhow::{Context, Result};
use fp_core::time::TimeFormat;
use fp_core::{Actor, EventKind, SourceInstance};
use quick_xml::events::{BytesStart, Event as XmlEvent};
use quick_xml::reader::Reader;
use serde_json::json;
use std::collections::HashMap;

const XBEL: &str = ".local/share/recently-used.xbel";

pub struct DesktopCollector;

#[derive(Debug, Default, PartialEq)]
struct Bookmark {
    href: String,
    added: Option<String>,
    modified: Option<String>,
    visited: Option<String>,
    mime: Option<String>,
    apps: Vec<AppUse>,
}

#[derive(Debug, PartialEq)]
struct AppUse {
    name: String,
    exec: Option<String>,
    count: Option<u64>,
    /// RFC 3339 `modified`, or the older unix `timestamp`
    when: Option<(String, TimeFormat)>,
}

impl Collector for DesktopCollector {
    fn per_user(&self) -> bool {
        true
    }

    fn kind(&self) -> &'static str {
        "desktop.recent"
    }

    fn discover(&self, env: &Env) -> Vec<SourceInstance> {
        let path = env.home.join(XBEL);
        if !path.is_file() {
            let mut inst = SourceInstance::new(self.kind(), "gtk-recent", None, path);
            inst.status = fp_core::Availability::NotInstalled;
            return vec![inst];
        }
        vec![SourceInstance::new(self.kind(), "gtk-recent", None, path)]
    }

    fn collect(&self, inst: &SourceInstance, ctx: &CollectCtx, sink: &mut Sink) -> Result<()> {
        let file = read_evidence(&inst.path)?;
        let text = std::str::from_utf8(&file.bytes).context("recently-used.xbel is not UTF-8")?;
        let bookmarks = parse_xbel(text).with_context(|| inst.path.display().to_string())?;
        let origin = Origin {
            file: &inst.path,
            sha256: Some(&file.sha256),
        };

        for bm in bookmarks {
            let path = file_uri_to_path(&bm.href);
            let fallback = bm
                .modified
                .as_ref()
                .or(bm.visited.as_ref())
                .map(|t| (t.clone(), TimeFormat::Rfc3339));
            let uses: Vec<(Option<&AppUse>, (String, TimeFormat))> = if bm.apps.is_empty() {
                fallback.into_iter().map(|w| (None, w)).collect()
            } else {
                bm.apps
                    .iter()
                    .filter_map(|a| Some((Some(a), a.when.clone().or_else(|| fallback.clone())?)))
                    .collect()
            };
            for (app, (raw, format)) in uses {
                let stamp = Stamp::parse(raw, format, None)?;
                if !in_window(ctx, stamp.ts_utc) {
                    continue;
                }
                let app_name = app.map(|a| a.name.as_str());
                let locator = match app_name {
                    Some(name) => format!("bookmark[href={}]/application[name={name}]", bm.href),
                    None => format!("bookmark[href={}]", bm.href),
                };
                sink.event(fp_core::Event {
                    actor: app_name.map(Actor::app),
                    attrs: json!({
                        "href": bm.href,
                        "mime_type": bm.mime,
                        "added": bm.added,
                        "visited": bm.visited,
                        "exec": app.and_then(|a| a.exec.as_deref()),
                        "count": app.and_then(|a| a.count),
                    }),
                    ..new_event(
                        ctx,
                        inst,
                        stamp,
                        EventKind::FileOpened,
                        path.clone(),
                        format!("{path} [{}]", app_name.unwrap_or("?")),
                        origin.at(locator),
                    )
                });
            }
        }
        Ok(())
    }
}

fn attributes(element: &BytesStart) -> Result<HashMap<String, String>> {
    element
        .attributes()
        .map(|attr| {
            let attr = attr?;
            Ok((
                String::from_utf8_lossy(attr.key.as_ref()).into_owned(),
                attr.unescape_value()?.into_owned(),
            ))
        })
        .collect()
}

fn parse_xbel(text: &str) -> Result<Vec<Bookmark>> {
    let mut reader = Reader::from_str(text);
    let mut bookmarks = Vec::new();
    let mut current: Option<Bookmark> = None;
    loop {
        match reader.read_event()? {
            XmlEvent::Start(e) | XmlEvent::Empty(e) => match e.name().as_ref() {
                b"bookmark" => {
                    let mut attrs = attributes(&e)?;
                    let bookmark = Bookmark {
                        href: attrs.remove("href").unwrap_or_default(),
                        added: attrs.remove("added"),
                        modified: attrs.remove("modified"),
                        visited: attrs.remove("visited"),
                        ..Bookmark::default()
                    };
                    if let Some(done) = current.replace(bookmark) {
                        bookmarks.push(done);
                    }
                }
                b"mime:mime-type" => {
                    if let Some(bm) = current.as_mut() {
                        bm.mime = attributes(&e)?.remove("type");
                    }
                }
                b"bookmark:application" => {
                    if let Some(bm) = current.as_mut() {
                        let mut attrs = attributes(&e)?;
                        let when = attrs
                            .remove("modified")
                            .map(|m| (m, TimeFormat::Rfc3339))
                            .or_else(|| {
                                attrs
                                    .remove("timestamp")
                                    .map(|t| (t, TimeFormat::UnixSeconds))
                            });
                        bm.apps.push(AppUse {
                            name: attrs.remove("name").unwrap_or_default(),
                            exec: attrs.remove("exec"),
                            count: attrs.get("count").and_then(|c| c.parse().ok()),
                            when,
                        });
                    }
                }
                _ => {}
            },
            XmlEvent::End(e) if e.name().as_ref() == b"bookmark" => {
                bookmarks.extend(current.take())
            }
            XmlEvent::Eof => break,
            _ => {}
        }
    }
    bookmarks.extend(current);
    Ok(bookmarks)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<xbel version="1.0" xmlns:bookmark="http://www.freedesktop.org/standards/desktop-bookmarks" xmlns:mime="http://www.freedesktop.org/standards/shared-mime-info">
  <bookmark href="file:///home/u/Downloads/Programs/zoom_amd64.deb" added="2026-10-16T17:36:40.100000Z" modified="2026-10-16T17:37:50.000000Z" visited="2026-10-16T17:36:40.100000Z">
    <info>
      <metadata owner="http://freedesktop.org">
        <mime:mime-type type="application/vnd.debian.binary-package"/>
        <bookmark:applications>
          <bookmark:application name="Files" exec="&apos;nautilus %u&apos;" modified="2026-10-16T17:37:10.000000Z" count="1"/>
          <bookmark:application name="App Center" exec="&apos;snap-store %U&apos;" modified="2026-10-16T17:37:50.000000Z" count="2"/>
        </bookmark:applications>
      </metadata>
    </info>
  </bookmark>
  <bookmark href="file:///home/u/My%20CV.pdf" added="2026-10-01T10:00:00Z" modified="2026-10-01T10:00:00Z" visited="2026-10-01T10:00:00Z">
    <info><metadata owner="http://freedesktop.org"><bookmark:applications>
      <bookmark:application name="evince" exec="evince %u" timestamp="1790848800" count="1"/>
    </bookmark:applications></metadata></info>
  </bookmark>
</xbel>"#;

    #[test]
    fn reads_attributes_and_applications() {
        let got = parse_xbel(SAMPLE).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(
            got[0].href,
            "file:///home/u/Downloads/Programs/zoom_amd64.deb"
        );
        assert_eq!(
            got[0].mime.as_deref(),
            Some("application/vnd.debian.binary-package")
        );
        assert_eq!(got[0].apps.len(), 2);
        assert_eq!(got[0].apps[0].exec.as_deref(), Some("'nautilus %u'"));
        assert_eq!(got[0].apps[1].count, Some(2));
        assert_eq!(
            got[1].apps[0].when,
            Some(("1790848800".to_string(), TimeFormat::UnixSeconds))
        );
    }

    #[test]
    fn broken_xml_is_an_error() {
        assert!(parse_xbel("<xbel><bookmark href=\"x\"></xbel>").is_err());
    }

    #[test]
    fn one_event_per_application() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recently-used.xbel");
        std::fs::write(&path, SAMPLE).unwrap();
        let inst = SourceInstance::new("desktop.recent", "gtk-recent", None, path);
        let ctx = CollectCtx {
            run_id: 1,
            since: None,
            until: None,
            assume_tz: None,
            temp_dir: dir.path().into(),
        };
        let mut sink = Sink::default();
        DesktopCollector.collect(&inst, &ctx, &mut sink).unwrap();
        let got: Vec<_> = sink.events.iter().map(|e| e.summary.as_str()).collect();
        assert_eq!(
            got,
            [
                "/home/u/Downloads/Programs/zoom_amd64.deb [Files]",
                "/home/u/Downloads/Programs/zoom_amd64.deb [App Center]",
                "/home/u/My CV.pdf [evince]",
            ]
        );
    }
}
