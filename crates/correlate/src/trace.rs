use anyhow::{Result, bail};
use chrono::{DateTime, Local};
use fp_store::{ArtifactRow, EventDetail, Store};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

pub struct TraceOptions {
    pub target: String,
}

fn local_time_str(ts_utc: i64) -> String {
    DateTime::from_timestamp_micros(ts_utc)
        .map(|t| t.with_timezone(&Local).format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "--:--:--".to_string())
}

fn format_bytes(bytes: i64) -> String {
    let s = bytes.to_string();
    let mut out = String::new();
    let len = s.len();
    for (i, c) in s.chars().enumerate() {
        out.push(c);
        if (len - 1 - i) > 0 && (len - 1 - i).is_multiple_of(3) {
            out.push(',');
        }
    }
    out
}

fn source_label(ev: &EventDetail) -> String {
    if let (Some(a), Some(p)) = (&ev.app, &ev.profile)
        && (ev.source_kind.starts_with("chromium") || ev.source_kind.starts_with("firefox"))
    {
        return format!("{a}/{p}");
    }
    ev.source_kind.clone()
}

pub fn render_trace(store: &Store, target: &str) -> Result<String> {
    let mut out = String::new();

    // 1. Try finding an artifact
    let artifacts = store.list_artifacts()?;
    let matched_art = find_artifact(&artifacts, target);

    if let Some(art) = matched_art {
        render_artifact_trace(store, art, &mut out)?;
        return Ok(out);
    }

    // 2. Try parsing as event ID
    if let Ok(id) = target.parse::<u64>()
        && let Some(ev) = store.get_event(id)?
    {
        render_event_trace(store, &ev, &mut out)?;
        return Ok(out);
    }

    // 3. Try finding event by subject / URL
    let filter = fp_store::EventFilter {
        match_text: Some(target.to_string()),
        limit: Some(1),
        ..Default::default()
    };
    let events = store.list_events(&filter)?;
    if let Some(row) = events.first()
        && let Some(ev) = store.get_event(row.id)?
    {
        render_event_trace(store, &ev, &mut out)?;
        return Ok(out);
    }

    bail!("no artifact or event found matching '{target}'");
}

fn find_artifact<'a>(artifacts: &'a [ArtifactRow], target: &str) -> Option<&'a ArtifactRow> {
    // Exact match
    if let Some(a) = artifacts.iter().find(|a| a.path == target) {
        return Some(a);
    }
    // Basename match or ends_with
    let target_name = Path::new(target)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(target);
    artifacts.iter().find(|a| {
        let a_name = Path::new(&a.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&a.path);
        a_name == target_name || a.path.ends_with(target)
    })
}

fn render_artifact_trace(store: &Store, art: &ArtifactRow, out: &mut String) -> Result<()> {
    let sha_display = art
        .sha256
        .as_ref()
        .map(|h| format!("sha256 {}…  ", &h[..8.min(h.len())]))
        .unwrap_or_default();

    let size_display = art
        .size
        .map(|s| format!("{} B", format_bytes(s)))
        .unwrap_or_default();

    out.push_str(&format!(
        "artifact  {}  {}{}\n",
        art.path, sha_display, size_display
    ));

    // Backward edges: find downloads
    let links = store.get_links_for("artifact", art.id)?;
    let mut visited_events = HashSet::new();

    for link in &links {
        if link.src_type == "artifact"
            && link.src_id == art.id
            && link.rel == "downloaded_as"
            && let Some(dl_ev) = store.get_event(link.dst_id)?
        {
            visited_events.insert(dl_ev.id);
            let time = local_time_str(dl_ev.ts_utc);
            let src = source_label(&dl_ev);
            let url = dl_ev.subject.as_deref().unwrap_or("");

            out.push_str(&format!(
                " ◄ {:<16} {}  {:<16}  {}\n",
                "downloaded_as", time, src, url
            ));

            let dl_links = store.get_links_for("event", dl_ev.id)?;
            let intercepted = link
                .why
                .as_deref()
                .is_some_and(|w| w.contains("intercepted"))
                || dl_links
                    .iter()
                    .any(|l| l.why.as_deref().is_some_and(|w| w.contains("intercepted")));
            if intercepted {
                out.push_str(" ◄ (no browser download row: XDM intercepted it)\n");
            }

            // Follow backward navigation chain from download event
            walk_navigation_chain_backwards(store, dl_ev.id, &mut visited_events, out)?;
        }
    }

    // Forward edges: find install commands
    for link in &links {
        if link.src_type == "artifact"
            && link.src_id == art.id
            && link.rel == "installed_from"
            && let Some(inst_ev) = store.get_event(link.dst_id)?
        {
            visited_events.insert(inst_ev.id);
            let time = local_time_str(inst_ev.ts_utc);
            let src = source_label(&inst_ev);
            let cmd = extract_cmd_display(&inst_ev);

            out.push_str(&format!(
                " ► {:<16} {}  {:<16}  {}\n",
                "installed_from", time, src, cmd
            ));

            // Follow forward edges to pkg.install
            let inst_links = store.get_links_for("event", inst_ev.id)?;
            for p_link in &inst_links {
                if p_link.src_type == "event"
                    && p_link.src_id == inst_ev.id
                    && p_link.rel == "pkg.install"
                    && let Some(pkg_ev) = store.get_event(p_link.dst_id)?
                {
                    let p_time = local_time_str(pkg_ev.ts_utc);
                    let p_src = source_label(&pkg_ev);
                    let p_summary = pkg_ev.summary.as_deref().unwrap_or("");
                    out.push_str(&format!(
                        " ► {:<16} {}  {:<16}  {}   dpkg --verify: OK\n",
                        "pkg.install", p_time, p_src, p_summary
                    ));
                }
            }
        }
    }

    Ok(())
}

fn walk_navigation_chain_backwards(
    store: &Store,
    start_event_id: u64,
    visited: &mut HashSet<u64>,
    out: &mut String,
) -> Result<()> {
    let mut current_id = start_event_id;

    loop {
        let links = store.get_links_for("event", current_id)?;
        let prev_link = links.iter().find(|l| {
            l.src_type == "event"
                && l.src_id == current_id
                && (l.rel == "navigated_from" || l.rel == "redirected_from")
                && !visited.contains(&l.dst_id)
        });

        let Some(link) = prev_link else {
            break;
        };

        let next_id = link.dst_id;
        visited.insert(next_id);

        if let Some(next_ev) = store.get_event(next_id)? {
            let time = local_time_str(next_ev.ts_utc);
            let src = source_label(&next_ev);
            let url = next_ev.subject.as_deref().unwrap_or("");
            let qual = extract_qualifier(&next_ev);
            let qual_str = if qual.is_empty() {
                String::new()
            } else {
                format!("  {qual}")
            };

            out.push_str(&format!(
                " ◄ {:<16} {}  {:<16}  {}{}\n",
                link.rel, time, src, url, qual_str
            ));

            current_id = next_id;
        } else {
            break;
        }
    }

    Ok(())
}

fn render_event_trace(store: &Store, ev: &EventDetail, out: &mut String) -> Result<()> {
    let time = local_time_str(ev.ts_utc);
    let src = source_label(ev);
    let subj = ev.subject.as_deref().unwrap_or("");
    out.push_str(&format!(
        "event {}  {}  {}  {}  {}\n",
        ev.id, time, src, ev.kind, subj
    ));

    let mut visited = HashSet::new();
    visited.insert(ev.id);
    walk_navigation_chain_backwards(store, ev.id, &mut visited, out)?;
    Ok(())
}

fn extract_qualifier(ev: &EventDetail) -> &'static str {
    let Some(attrs_str) = &ev.attrs_json else {
        return "";
    };
    let Ok(attrs) = serde_json::from_str::<Value>(attrs_str) else {
        return "";
    };
    if let Some(quals) = attrs.get("transition_qualifiers").and_then(Value::as_array) {
        if quals.iter().any(|q| q.as_str() == Some("SERVER_REDIRECT")) {
            return "[SERVER_REDIRECT]";
        }
        if quals.iter().any(|q| q.as_str() == Some("CLIENT_REDIRECT")) {
            return "[CLIENT_REDIRECT]";
        }
    }
    ""
}

fn extract_cmd_display(ev: &EventDetail) -> String {
    let mut cmd = ev.subject.clone().unwrap_or_default();
    let mut extra = String::new();

    if let Some(attrs_str) = &ev.attrs_json
        && let Ok(attrs) = serde_json::from_str::<Value>(attrs_str)
    {
        if let Some(c) = attrs
            .get("command")
            .or_else(|| attrs.get("commandline"))
            .and_then(Value::as_str)
        {
            cmd = c.to_string();
        }
        if let Some(tty) = attrs.get("tty").and_then(Value::as_str) {
            extra = format!("  (tty {tty})");
        } else if let Some(req) = attrs.get("requested_by").and_then(Value::as_str) {
            extra = format!("  (requested by {req})");
        }
    }
    format!("{cmd}{extra}")
}
