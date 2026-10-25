use anyhow::Result;
use fp_core::{Confidence, Link, LinkType};
use fp_store::Store;
use serde_json::Value;
use std::collections::HashMap;

/// A browser history file: the source kind and the file the visits came from.
type HistoryFile = (String, Option<String>);

/// Correlates browser visits and redirects using `from_visit` pointers.
pub fn correlate_navigation_chains(store: &mut Store) -> Result<usize> {
    let events = store.get_events_by_kinds(&["web.visit", "web.redirect"])?;
    // visit ids are per history file, and a redirect collected in a later run points at a
    // visit stored under an earlier run's source row: key by the file, never by `source_id`
    let history = |ev: &fp_store::EventDetail| (ev.source_kind.clone(), ev.prov_file.clone());
    let mut visit_map: HashMap<(HistoryFile, i64), u64> = HashMap::new();
    for ev in &events {
        if let Some(attrs_str) = &ev.attrs_json
            && let Ok(attrs) = serde_json::from_str::<Value>(attrs_str)
            && let Some(vid) = attrs.get("visit_id").and_then(Value::as_i64)
        {
            visit_map.insert((history(ev), vid), ev.id);
        }
    }

    let mut links = Vec::new();
    for ev in &events {
        let Some(attrs_str) = &ev.attrs_json else {
            continue;
        };
        let Ok(attrs) = serde_json::from_str::<Value>(attrs_str) else {
            continue;
        };
        let Some(from_visit) = attrs.get("from_visit").and_then(Value::as_i64) else {
            continue;
        };
        if from_visit == 0 {
            continue;
        }

        if let Some(&src_id) = visit_map.get(&(history(ev), from_visit)) {
            let is_redirect = ev.kind == "web.redirect"
                || attrs
                    .get("transition_qualifiers")
                    .and_then(Value::as_array)
                    .is_some_and(|quals| {
                        quals.iter().any(|q| {
                            q.as_str() == Some("SERVER_REDIRECT")
                                || q.as_str() == Some("CLIENT_REDIRECT")
                        })
                    });

            let qualifier =
                if let Some(quals) = attrs.get("transition_qualifiers").and_then(Value::as_array) {
                    if quals.iter().any(|q| q.as_str() == Some("SERVER_REDIRECT")) {
                        " [SERVER_REDIRECT]"
                    } else if quals.iter().any(|q| q.as_str() == Some("CLIENT_REDIRECT")) {
                        " [CLIENT_REDIRECT]"
                    } else {
                        ""
                    }
                } else {
                    ""
                };

            let (rel, why) = if is_redirect {
                (
                    "redirected_from".to_string(),
                    format!("visits.from_visit={from_visit}{qualifier}"),
                )
            } else {
                (
                    "navigated_from".to_string(),
                    format!("visits.from_visit={from_visit}{qualifier}"),
                )
            };

            links.push(Link {
                src_type: LinkType::Event,
                src_id: ev.id,
                rel,
                dst_type: LinkType::Event,
                dst_id: src_id,
                confidence: Confidence::Direct,
                why: Some(why),
            });
        }
    }

    let count = store.insert_links(&links)?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_core::time::TimeFormat;
    use fp_core::{Actor, Event, EventKind, Provenance, SourceInstance, TimePrecision};
    use fp_store::{Batch, NewRun};
    use serde_json::json;
    use std::path::PathBuf;

    fn visit(kind: EventKind, visit_id: i64, from_visit: i64, ts_utc: i64) -> Event {
        Event {
            run_id: 1,
            source_kind: "chromium.history".into(),
            ts_utc,
            ts_precision: TimePrecision::Exact,
            ts_raw: ts_utc.to_string(),
            ts_raw_format: TimeFormat::UnixSeconds,
            ts_tz_assumed: None,
            source_inst: 0,
            kind,
            actor: Some(Actor::app("Brave")),
            subject: format!("https://example.com/{visit_id}"),
            subject_host: Some("example.com".into()),
            summary: String::new(),
            attrs: json!({ "visit_id": visit_id, "from_visit": from_visit }),
            provenance: Provenance {
                file: PathBuf::from("/home/u/.config/BraveSoftware/Brave-Browser/Default/History"),
                file_sha256: None,
                locator: format!("visits.id={visit_id}"),
            },
            confidence: Confidence::Direct,
        }
    }

    /// One collect of the same profile: a new run, so a new source row.
    fn collect(store: &mut Store, events: &[Event]) {
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
        let inst = SourceInstance::new(
            "chromium.history",
            "Brave",
            Some("Default".into()),
            PathBuf::from("/home/u/.config/BraveSoftware/Brave-Browser/Default"),
        );
        let id = store.insert_source(run, &inst).unwrap();
        let events: Vec<Event> = events
            .iter()
            .map(|e| Event {
                source_inst: id,
                ..e.clone()
            })
            .collect();
        store
            .ingest(
                run,
                &Batch {
                    source_identity: &inst.identity(),
                    events: &events,
                    states: &[],
                    artifacts: &[],
                },
            )
            .unwrap();
    }

    #[test]
    fn a_redirect_links_to_a_visit_from_an_earlier_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open_or_create(dir.path().join("case.db")).unwrap();
        collect(&mut store, &[visit(EventKind::WebVisit, 1, 0, 100)]);
        collect(&mut store, &[visit(EventKind::WebRedirect, 2, 1, 105)]);

        assert_eq!(correlate_navigation_chains(&mut store).unwrap(), 1);
        let links = store.list_links().unwrap();
        assert_eq!(links[0].rel, "redirected_from");
    }
}
