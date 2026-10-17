use anyhow::Result;
use fp_core::{Confidence, Link, LinkType};
use fp_store::Store;
use serde_json::Value;

/// Joins `web.visit` hosts with `web.site_permission` state records.
pub fn correlate_site_permissions(store: &mut Store) -> Result<usize> {
    let state_records = store.list_state_by_kind("web.site_permission")?;
    let visit_events = store.get_events_by_kinds(&["web.visit", "web.redirect"])?;

    let mut links = Vec::new();

    for st in &state_records {
        let Ok(val) = serde_json::from_str::<Value>(&st.value_json) else {
            continue;
        };
        let origin = val.get("origin").and_then(Value::as_str).unwrap_or("");
        let perm = val
            .get("permission")
            .and_then(Value::as_str)
            .unwrap_or("permission");
        let decision = val
            .get("decision")
            .and_then(Value::as_str)
            .unwrap_or("unknown");

        for visit in &visit_events {
            let Some(host) = &visit.subject_host else {
                continue;
            };
            if !host.is_empty() && (origin.contains(host) || st.key.contains(host)) {
                links.push(Link {
                    src_type: LinkType::Event,
                    src_id: visit.id,
                    rel: "site_permission".to_string(),
                    dst_type: LinkType::State,
                    dst_id: st.rowid as u64,
                    confidence: Confidence::Direct,
                    why: Some(format!("host holds {perm} ({decision})")),
                });
            }
        }
    }

    Ok(store.insert_links(&links)?)
}
