use anyhow::{Result, bail};
use fp_store::{ArtifactRow, EventDetail, Store};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

pub struct TraceOptions {
    pub target: String,
}

/// What the trace starts from: an artifact or a single event.
#[derive(Debug, Clone)]
pub enum TraceHead {
    Artifact {
        id: u64,
        path: String,
        sha256: Option<String>,
        size: Option<i64>,
    },
    Event {
        id: u64,
        ts_utc: i64,
        source: String,
        kind: String,
        subject: String,
    },
}

/// One line of provenance below the head. `arrow` is which way the edge points:
/// backward (◄, where the evidence came from) or forward (►, what it led to).
#[derive(Debug, Clone)]
pub struct TraceRow {
    pub arrow: TraceArrow,
    pub rel: String,
    pub ts_utc: Option<i64>,
    pub source: String,
    pub text: String,
    pub confidence: Option<String>,
    /// set when the row is an event the UI can jump to
    pub event_id: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceArrow {
    Back,
    Forward,
    Note,
}

#[derive(Debug, Clone)]
pub struct TraceTree {
    pub head: TraceHead,
    pub rows: Vec<TraceRow>,
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

/// Builds the provenance tree for an artifact path, URL, or event id.
pub fn trace_tree(store: &Store, target: &str) -> Result<TraceTree> {
    let artifacts = store.list_artifacts()?;
    if let Some(art) = find_artifact(&artifacts, target) {
        return artifact_tree(store, art);
    }
    if let Ok(id) = target.parse::<u64>()
        && let Some(ev) = store.get_event(id)?
    {
        return event_tree(store, &ev);
    }
    let filter = fp_store::EventFilter {
        match_text: Some(target.to_string()),
        limit: Some(1),
        ..Default::default()
    };
    let events = store.list_events(&filter)?;
    if let Some(row) = events.first()
        && let Some(ev) = store.get_event(row.id)?
    {
        return event_tree(store, &ev);
    }
    bail!("no artifact or event found matching '{target}'")
}

/// Renders the tree in the text form: `rel time source text`, `◄`/`►` per edge.
/// `time` turns a UTC µs instant into the clock text shown (the caller picks the zone).
pub fn render_tree(tree: &TraceTree, time: &dyn Fn(i64) -> String) -> String {
    let mut out = String::new();
    match &tree.head {
        TraceHead::Artifact {
            path, sha256, size, ..
        } => {
            let sha_display = sha256
                .as_ref()
                .map(|h| format!("sha256 {}…  ", &h[..8.min(h.len())]))
                .unwrap_or_default();
            let size_display = size
                .map(|s| format!("{} B", format_bytes(s)))
                .unwrap_or_default();
            out.push_str(&format!("artifact  {path}  {sha_display}{size_display}\n"));
        }
        TraceHead::Event {
            id,
            ts_utc,
            source,
            kind,
            subject,
        } => {
            out.push_str(&format!(
                "event {id}  {}  {source}  {kind}  {subject}\n",
                time(*ts_utc)
            ));
        }
    }
    for row in &tree.rows {
        let time = row
            .ts_utc
            .map(time)
            .unwrap_or_else(|| "        ".to_string());
        let arrow = match row.arrow {
            TraceArrow::Back => "◄",
            TraceArrow::Forward => "►",
            TraceArrow::Note => "◄",
        };
        if row.arrow == TraceArrow::Note {
            out.push_str(&format!(" {arrow} {}\n", row.text));
        } else {
            out.push_str(&format!(
                " {arrow} {:<16} {time}  {:<16}  {}\n",
                row.rel, row.source, row.text
            ));
        }
    }
    out
}

pub fn render_trace(store: &Store, target: &str, time: &dyn Fn(i64) -> String) -> Result<String> {
    Ok(render_tree(&trace_tree(store, target)?, time))
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

fn artifact_tree(store: &Store, art: &ArtifactRow) -> Result<TraceTree> {
    let head = TraceHead::Artifact {
        id: art.id,
        path: art.path.clone(),
        sha256: art.sha256.clone(),
        size: art.size,
    };
    let links = store.get_links_for("artifact", art.id)?;
    let mut rows = Vec::new();
    let mut visited_events = HashSet::new();

    for link in &links {
        if link.src_type == "artifact"
            && link.src_id == art.id
            && link.rel == "downloaded_as"
            && let Some(dl_ev) = store.get_event(link.dst_id)?
        {
            visited_events.insert(dl_ev.id);
            rows.push(TraceRow {
                arrow: TraceArrow::Back,
                rel: "downloaded_as".into(),
                ts_utc: Some(dl_ev.ts_utc),
                source: source_label(&dl_ev),
                text: dl_ev.subject.clone().unwrap_or_default(),
                confidence: Some(link.confidence.clone()),
                event_id: Some(dl_ev.id),
            });

            let dl_links = store.get_links_for("event", dl_ev.id)?;
            let intercepted = link
                .why
                .as_deref()
                .is_some_and(|w| w.contains("intercepted"))
                || dl_links
                    .iter()
                    .any(|l| l.why.as_deref().is_some_and(|w| w.contains("intercepted")));
            if intercepted {
                rows.push(TraceRow {
                    arrow: TraceArrow::Note,
                    rel: String::new(),
                    ts_utc: None,
                    source: String::new(),
                    text: "(no browser download row: XDM intercepted it)".into(),
                    confidence: None,
                    event_id: None,
                });
            }

            // Follow backward navigation chain from download event
            walk_navigation_chain_backwards(store, dl_ev.id, &mut visited_events, &mut rows)?;
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
            let (cmd, extra) = cmd_display(&inst_ev);
            rows.push(TraceRow {
                arrow: TraceArrow::Forward,
                rel: "installed_from".into(),
                ts_utc: Some(inst_ev.ts_utc),
                source: source_label(&inst_ev),
                text: format!("{cmd}{extra}"),
                confidence: Some(link.confidence.clone()),
                event_id: Some(inst_ev.id),
            });

            // Follow forward edges to pkg.install
            let inst_links = store.get_links_for("event", inst_ev.id)?;
            for p_link in &inst_links {
                if p_link.src_type == "event"
                    && p_link.src_id == inst_ev.id
                    && p_link.rel == "pkg.install"
                    && let Some(pkg_ev) = store.get_event(p_link.dst_id)?
                {
                    rows.push(TraceRow {
                        arrow: TraceArrow::Forward,
                        rel: "pkg.install".into(),
                        ts_utc: Some(pkg_ev.ts_utc),
                        source: source_label(&pkg_ev),
                        text: pkg_ev.summary.clone().unwrap_or_default(),
                        confidence: Some(p_link.confidence.clone()),
                        event_id: Some(pkg_ev.id),
                    });
                }
            }
        }
    }

    Ok(TraceTree { head, rows })
}

fn event_tree(store: &Store, ev: &EventDetail) -> Result<TraceTree> {
    let head = TraceHead::Event {
        id: ev.id,
        ts_utc: ev.ts_utc,
        source: source_label(ev),
        kind: ev.kind.clone(),
        subject: ev.subject.clone().unwrap_or_default(),
    };
    let mut rows = Vec::new();
    let mut visited = HashSet::new();
    visited.insert(ev.id);
    walk_navigation_chain_backwards(store, ev.id, &mut visited, &mut rows)?;
    Ok(TraceTree { head, rows })
}

fn walk_navigation_chain_backwards(
    store: &Store,
    start_event_id: u64,
    visited: &mut HashSet<u64>,
    rows: &mut Vec<TraceRow>,
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
            let qual = extract_qualifier(&next_ev);
            let qual_str = if qual.is_empty() {
                String::new()
            } else {
                format!("  {qual}")
            };
            rows.push(TraceRow {
                arrow: TraceArrow::Back,
                rel: link.rel.clone(),
                ts_utc: Some(next_ev.ts_utc),
                source: source_label(&next_ev),
                text: format!("{}{qual_str}", next_ev.subject.clone().unwrap_or_default()),
                confidence: Some(link.confidence.clone()),
                event_id: Some(next_ev.id),
            });
            current_id = next_id;
        } else {
            break;
        }
    }

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

/// The command (or commandline) an install/sudo event carries, with tty/requester context.
pub fn cmd_display(ev: &EventDetail) -> (String, String) {
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
    (cmd, extra)
}
