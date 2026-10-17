use anyhow::Result;
use fp_core::{Confidence, Link, LinkType};
use fp_store::Store;
use serde_json::Value;
use std::collections::HashMap;

/// Correlates browser visits and redirects using `from_visit` pointers.
pub fn correlate_navigation_chains(store: &mut Store) -> Result<usize> {
    let events = store.get_events_by_kinds(&["web.visit", "web.redirect"])?;
    // Map (source_id, visit_id) -> event.id
    let mut visit_map: HashMap<(u64, i64), u64> = HashMap::new();
    for ev in &events {
        if let Some(attrs_str) = &ev.attrs_json
            && let Ok(attrs) = serde_json::from_str::<Value>(attrs_str)
            && let Some(vid) = attrs.get("visit_id").and_then(Value::as_i64)
        {
            visit_map.insert((ev.source_id, vid), ev.id);
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

        if let Some(&src_id) = visit_map.get(&(ev.source_id, from_visit)) {
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
