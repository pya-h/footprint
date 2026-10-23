use anyhow::Result;
use fp_correlate::match_event_iocs;
use fp_store::{EventFilter, Store};

/// Everything `list` filters on. Times are unix µs, UTC.
#[derive(Debug, Default, Clone)]
pub struct TimelineFilter {
    pub since: Option<i64>,
    pub until: Option<i64>,
    /// event kinds; `web.*` matches a family
    pub kinds: Vec<String>,
    /// hosts, each also matching its subdomains
    pub hosts: Vec<String>,
    /// source kinds, apps, or `app/profile`
    pub sources: Vec<String>,
    /// substring of subject, summary or attributes
    pub match_text: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub reverse: bool,
}

/// One page of a large timeline: one extra row is fetched to know whether more exist.
pub struct TimelinePage {
    pub rows: Vec<TimelineRow>,
    pub has_more: bool,
}

pub const PAGE_SIZE: usize = 500;

/// Fetches `size` rows starting at `offset` under `filter`.
pub fn timeline_page(
    store: &Store,
    filter: &TimelineFilter,
    offset: usize,
    size: usize,
) -> Result<TimelinePage> {
    let page_filter = TimelineFilter {
        limit: Some(size + 1),
        offset: Some(offset),
        ..filter.clone()
    };
    let mut rows = timeline(store, &page_filter)?;
    let has_more = rows.len() > size;
    rows.truncate(size);
    Ok(TimelinePage { rows, has_more })
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct TimelineRow {
    pub id: u64,
    pub run_id: u64,
    pub ts_utc: i64,
    pub ts_precision: String,
    pub kind: String,
    pub source_kind: String,
    pub app: Option<String>,
    pub profile: Option<String>,
    pub subject: Option<String>,
    pub subject_host: Option<String>,
    pub summary: Option<String>,
    pub confidence: String,
    pub ioc_hits: Vec<String>,
}

impl TimelineRow {
    /// `app/profile` for browser-like sources, the app or kind otherwise.
    pub fn source_label(&self) -> String {
        match (&self.app, &self.profile) {
            (Some(a), Some(p)) => format!("{a}/{p}"),
            (Some(a), None) => a.clone(),
            _ => self.source_kind.clone(),
        }
    }

    /// The one line a timeline shows: summary, falling back to the subject.
    pub fn display_text(&self) -> String {
        self.summary
            .clone()
            .or_else(|| self.subject.clone())
            .unwrap_or_default()
    }
}

pub fn timeline(store: &Store, filter: &TimelineFilter) -> Result<Vec<TimelineRow>> {
    let store_filter = EventFilter {
        since: filter.since,
        until: filter.until,
        kinds: filter.kinds.clone(),
        hosts: filter.hosts.clone(),
        match_text: filter.match_text.clone(),
        limit: filter.limit,
        offset: filter.offset,
        reverse: filter.reverse,
    };
    let iocs = store.list_iocs().unwrap_or_default();
    let mut rows = store
        .list_events(&store_filter)?
        .into_iter()
        .map(|r| {
            let ioc_hits = match_event_iocs(
                r.subject_host.as_deref(),
                r.subject.as_deref(),
                r.summary.as_deref(),
                &iocs,
            );
            TimelineRow {
                id: r.id,
                run_id: r.run_id,
                ts_utc: r.ts_utc,
                ts_precision: r.ts_precision,
                kind: r.kind,
                source_kind: r.source_kind,
                app: r.app,
                profile: r.profile,
                subject: r.subject,
                subject_host: r.subject_host,
                summary: r.summary,
                confidence: r.confidence,
                ioc_hits,
            }
        })
        .collect::<Vec<_>>();
    if !filter.sources.is_empty() {
        rows.retain(|r| source_selected(r, &filter.sources));
    }
    Ok(rows)
}

/// A source pattern matches the app, the `app/profile`, the event kind, or a kind family.
pub fn source_selected(row: &TimelineRow, patterns: &[String]) -> bool {
    let app = row.app.as_deref().unwrap_or("");
    let source_str = match (&row.app, &row.profile) {
        (Some(a), Some(p)) => format!("{a}/{p}"),
        (Some(a), None) => a.clone(),
        _ => String::new(),
    };
    patterns
        .iter()
        .any(|s| s == &row.kind || s == app || s == &source_str || row.kind.starts_with(s.as_str()))
}

/// Full-text search over subjects, summaries and attributes (FTS5 syntax), with IOC hits.
pub fn search(store: &Store, query: &str) -> Result<Vec<TimelineRow>> {
    let iocs = store.list_iocs().unwrap_or_default();
    Ok(store
        .search_events(query)?
        .into_iter()
        .map(|r| {
            let ioc_hits = match_event_iocs(
                r.subject_host.as_deref(),
                r.subject.as_deref(),
                r.summary.as_deref(),
                &iocs,
            );
            TimelineRow {
                id: r.id,
                run_id: r.run_id,
                ts_utc: r.ts_utc,
                ts_precision: r.ts_precision,
                kind: r.kind,
                source_kind: r.source_kind,
                app: r.app,
                profile: r.profile,
                subject: r.subject,
                subject_host: r.subject_host,
                summary: r.summary,
                confidence: r.confidence,
                ioc_hits,
            }
        })
        .collect())
}
