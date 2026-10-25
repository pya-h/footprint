use anyhow::Result;
use fp_store::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffChange {
    Added,
    Removed,
    Changed,
}

#[derive(Debug, Clone)]
pub struct DiffEntry {
    pub change: DiffChange,
    pub kind: String,
    pub key: String,
    /// a one-line human description of what changed
    pub detail: String,
    pub run_a: Option<serde_json::Value>,
    pub run_b: Option<serde_json::Value>,
}

/// Compares the state records of two runs by `(kind, key)`.
pub fn diff_runs(store: &Store, run_a: u64, run_b: u64) -> Result<Vec<DiffEntry>> {
    let states_a = store.list_state(Some(run_a), None, None)?;
    let states_b = store.list_state(Some(run_b), None, None)?;

    let mut map_a: std::collections::HashMap<(String, String), String> =
        std::collections::HashMap::new();
    for row in &states_a {
        map_a.insert((row.kind.clone(), row.key.clone()), row.value_json.clone());
    }
    let mut map_b: std::collections::HashMap<(String, String), String> =
        std::collections::HashMap::new();
    for row in &states_b {
        map_b.insert((row.kind.clone(), row.key.clone()), row.value_json.clone());
    }

    let mut keys: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
    keys.extend(map_a.keys().cloned());
    keys.extend(map_b.keys().cloned());

    let mut out = Vec::new();
    for (kind, key) in keys {
        match (
            map_a.get(&(kind.clone(), key.clone())),
            map_b.get(&(kind.clone(), key.clone())),
        ) {
            (None, Some(b)) => out.push(DiffEntry {
                change: DiffChange::Added,
                detail: added_summary(b),
                kind,
                key,
                run_a: None,
                run_b: serde_json::from_str(b).ok(),
            }),
            (Some(a), None) => out.push(DiffEntry {
                change: DiffChange::Removed,
                detail: "removed".into(),
                kind,
                key,
                run_a: serde_json::from_str(a).ok(),
                run_b: None,
            }),
            (Some(a), Some(b)) if a != b => out.push(DiffEntry {
                change: DiffChange::Changed,
                detail: changed_summary(a, b),
                kind,
                key,
                run_a: serde_json::from_str(a).ok(),
                run_b: serde_json::from_str(b).ok(),
            }),
            _ => {}
        }
    }
    Ok(out)
}

fn added_summary(value_json: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(value_json) else {
        return "added".into();
    };
    if let Some(flags) = v.get("flags").and_then(|f| f.as_array())
        && !flags.is_empty()
    {
        let flag_strs: Vec<&str> = flags.iter().filter_map(|s| s.as_str()).collect();
        return format!("added [{}]", flag_strs.join(", "));
    }
    "added".into()
}

fn changed_summary(old_json: &str, new_json: &str) -> String {
    let Ok(old_v) = serde_json::from_str::<serde_json::Value>(old_json) else {
        return "content changed".into();
    };
    let Ok(new_v) = serde_json::from_str::<serde_json::Value>(new_json) else {
        return "content changed".into();
    };
    let mut changes = Vec::new();
    if old_v.get("sha256") != new_v.get("sha256") {
        changes.push("sha256 changed");
    }
    if old_v.get("decision") != new_v.get("decision") {
        let old_d = old_v
            .get("decision")
            .and_then(|d| d.as_str())
            .unwrap_or("none");
        let new_d = new_v
            .get("decision")
            .and_then(|d| d.as_str())
            .unwrap_or("none");
        return format!("decision: {old_d} -> {new_d}");
    }
    if old_v.get("flags") != new_v.get("flags") {
        changes.push("flags changed");
    }
    if old_v.get("owner_pkg") != new_v.get("owner_pkg") {
        changes.push("package owner changed");
    }
    if changes.is_empty() {
        "metadata changed".into()
    } else {
        changes.join(", ")
    }
}
