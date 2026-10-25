use anyhow::Result;
use fp_store::{LinkRow, Store};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkDirection {
    /// the link leaves this event (`event -rel-> target`)
    Outgoing,
    /// the link points at this event (`source -rel-> event`)
    Incoming,
}

#[derive(Debug, Clone)]
pub struct LinkView {
    pub rel: String,
    pub direction: LinkDirection,
    pub target_type: String,
    pub target_id: u64,
    /// a short description of the target: summary, path, or state kind/key
    pub target_label: String,
    pub confidence: String,
    pub why: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EventDetailView {
    pub id: u64,
    pub run_id: u64,
    pub ts_utc: i64,
    pub ts_precision: String,
    pub ts_raw: Option<String>,
    pub ts_raw_format: Option<String>,
    pub ts_tz_assumed: Option<String>,
    pub kind: String,
    pub source_kind: String,
    pub app: Option<String>,
    pub profile: Option<String>,
    pub subject: Option<String>,
    pub subject_host: Option<String>,
    pub summary: Option<String>,
    pub confidence: String,
    pub actor: Option<serde_json::Value>,
    pub attrs: Option<serde_json::Value>,
    pub prov_file: Option<String>,
    pub prov_sha256: Option<String>,
    pub locator: Option<String>,
    pub links: Vec<LinkView>,
}

impl EventDetailView {
    /// `app/profile` for browser-like sources, the app or kind otherwise.
    pub fn source_label(&self) -> String {
        match (&self.app, &self.profile) {
            (Some(a), Some(p)) => format!("{a}/{p}"),
            (Some(a), None) => a.clone(),
            _ => self.source_kind.clone(),
        }
    }
}

pub fn event_detail(store: &Store, id: u64) -> Result<Option<EventDetailView>> {
    let Some(ev) = store.get_event(id)? else {
        return Ok(None);
    };
    let links = store
        .get_links_for("event", id)?
        .into_iter()
        .map(|l| link_view(store, &ev, l))
        .collect::<Result<Vec<_>>>()?;
    Ok(Some(EventDetailView {
        id: ev.id,
        run_id: ev.run_id,
        ts_utc: ev.ts_utc,
        ts_precision: ev.ts_precision,
        ts_raw: ev.ts_raw,
        ts_raw_format: ev.ts_raw_format,
        ts_tz_assumed: ev.ts_tz_assumed,
        kind: ev.kind,
        source_kind: ev.source_kind,
        app: ev.app,
        profile: ev.profile,
        subject: ev.subject,
        subject_host: ev.subject_host,
        summary: ev.summary,
        confidence: ev.confidence,
        actor: ev
            .actor_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok()),
        attrs: ev
            .attrs_json
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok()),
        prov_file: ev.prov_file,
        prov_sha256: ev.prov_sha256,
        locator: ev.locator,
        links,
    }))
}

fn link_view(store: &Store, ev: &fp_store::EventDetail, link: LinkRow) -> Result<LinkView> {
    let outgoing = link.src_type == "event" && link.src_id == ev.id;
    let (target_type, target_id) = if outgoing {
        (link.dst_type, link.dst_id)
    } else {
        (link.src_type, link.src_id)
    };
    Ok(LinkView {
        rel: link.rel,
        direction: if outgoing {
            LinkDirection::Outgoing
        } else {
            LinkDirection::Incoming
        },
        target_label: target_label(store, &target_type, target_id)?,
        target_type,
        target_id,
        confidence: link.confidence,
        why: link.why,
    })
}

fn target_label(store: &Store, target_type: &str, target_id: u64) -> Result<String> {
    match target_type {
        "event" => Ok(store
            .get_event(target_id)?
            .map(|e| {
                e.summary
                    .or(e.subject)
                    .unwrap_or_else(|| format!("event {}", e.id))
            })
            .unwrap_or_else(|| format!("event {target_id}"))),
        "artifact" => Ok(store
            .get_artifact(target_id)?
            .map(|a| a.path)
            .unwrap_or_else(|| format!("artifact {target_id}"))),
        "state" => {
            let mut stmt = store
                .conn()
                .prepare("SELECT kind, key FROM state WHERE rowid = ?1")?;
            let row = stmt
                .query_row([target_id], |r| {
                    Ok(format!(
                        "{}: {}",
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?
                    ))
                })
                .ok();
            Ok(row.unwrap_or_else(|| format!("state {target_id}")))
        }
        "ioc" => Ok(store
            .list_iocs()?
            .into_iter()
            .find(|i| i.id == target_id)
            .map(|i| format!("{} {}", i.ioc_type, i.value))
            .unwrap_or_else(|| format!("ioc {target_id}"))),
        other => Ok(format!("{other} {target_id}")),
    }
}
