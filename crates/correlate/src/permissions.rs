use anyhow::Result;
use fp_core::net::extract_host;
use fp_core::{Confidence, Link, LinkType};
use fp_store::Store;
use serde_json::Value;
use std::collections::HashMap;

/// Joins visited hosts with the `web.site_permission` state their origin holds.
pub fn correlate_site_permissions(store: &mut Store) -> Result<usize> {
    let mut by_host: HashMap<String, Vec<(i64, String)>> = HashMap::new();
    for st in store.list_state_by_kind("web.site_permission")? {
        let Ok(val) = serde_json::from_str::<Value>(&st.value_json) else {
            continue;
        };
        // a permission belongs to one origin; `zoom.us` must not inherit `us09-web-zoom.us`'s camera
        let Some(host) = val
            .get("origin")
            .and_then(Value::as_str)
            .and_then(extract_host)
        else {
            continue;
        };
        let perm = val
            .get("permission")
            .and_then(Value::as_str)
            .unwrap_or("permission");
        let decision = val
            .get("decision")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        by_host
            .entry(host)
            .or_default()
            .push((st.rowid, format!("host holds {perm} ({decision})")));
    }
    if by_host.is_empty() {
        return Ok(0);
    }

    let mut links = Vec::new();
    for visit in store.get_events_by_kinds(&["web.visit", "web.redirect"])? {
        let Some(grants) = visit.subject_host.as_ref().and_then(|h| by_host.get(h)) else {
            continue;
        };
        for (rowid, why) in grants {
            links.push(Link {
                src_type: LinkType::Event,
                src_id: visit.id,
                rel: "site_permission".to_string(),
                dst_type: LinkType::State,
                dst_id: *rowid as u64,
                confidence: Confidence::Direct,
                why: Some(why.clone()),
            });
        }
    }

    Ok(store.insert_links(&links)?)
}
