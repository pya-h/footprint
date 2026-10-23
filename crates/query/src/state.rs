use anyhow::Result;
use fp_store::Store;

#[derive(Debug, Clone)]
pub struct StateView {
    pub rowid: i64,
    pub run_id: u64,
    pub kind: String,
    pub key: String,
    pub value_json: String,
    pub parsed: Option<serde_json::Value>,
    pub locator: Option<String>,
}

pub fn state_records(
    store: &Store,
    run_id: Option<u64>,
    kind: Option<&str>,
    match_text: Option<&str>,
) -> Result<Vec<StateView>> {
    Ok(store
        .list_state(run_id, kind, match_text)?
        .into_iter()
        .map(|row| {
            let parsed = serde_json::from_str(&row.value_json).ok();
            StateView {
                rowid: row.rowid,
                run_id: row.run_id,
                kind: row.kind,
                key: row.key,
                value_json: row.value_json,
                parsed,
                locator: row.locator,
            }
        })
        .collect())
}

/// The human-interesting bits of a state value, for one-line displays.
pub fn state_details(parsed: Option<&serde_json::Value>) -> Vec<String> {
    let Some(val) = parsed else {
        return Vec::new();
    };
    let mut parts = Vec::new();
    if let Some(flags) = val.get("flags").and_then(|f| f.as_array())
        && !flags.is_empty()
    {
        let flag_strs: Vec<&str> = flags.iter().filter_map(|s| s.as_str()).collect();
        parts.push(format!("flags: [{}]", flag_strs.join(", ")));
    }
    if let Some(pkg) = val.get("owner_pkg").and_then(|p| p.as_str()) {
        parts.push(format!("pkg: {pkg}"));
    }
    if let Some(integ) = val.get("integrity").and_then(|i| i.as_str())
        && integ != "ok"
        && integ != "unowned"
    {
        parts.push(format!("integrity: {integ}"));
    }
    if let Some(decision) = val.get("decision").and_then(|d| d.as_str()) {
        parts.push(format!("decision: {decision}"));
    }
    if let Some(ww) = val
        .get("details")
        .and_then(|d| d.get("world_writable_path_entries"))
        .and_then(|w| w.as_array())
        && !ww.is_empty()
    {
        let paths: Vec<&str> = ww.iter().filter_map(|p| p.as_str()).collect();
        parts.push(format!("world-writable: {}", paths.join(", ")));
    }
    if let Some(u0) = val
        .get("details")
        .and_then(|d| d.get("uid0_users"))
        .and_then(|u| u.as_array())
    {
        let non_root: Vec<&str> = u0
            .iter()
            .filter_map(|u| u.as_str())
            .filter(|&u| u != "root")
            .collect();
        if !non_root.is_empty() {
            parts.push(format!("uid0 non-root: {}", non_root.join(", ")));
        }
    }
    parts
}
