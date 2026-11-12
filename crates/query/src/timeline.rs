use anyhow::Result;
use fp_core::Confidence;
use fp_correlate::match_event_iocs;
use fp_store::{EventFilter, EventTiming, Store};

/// Everything `list` filters on. Times are unix µs, UTC.
#[derive(Debug, Default, Clone)]
pub struct TimelineFilter {
    pub since: Option<i64>,
    pub until: Option<i64>,
    /// event kinds; `web.*` matches a family
    pub kinds: Vec<String>,
    pub kinds_not: Vec<String>,
    /// hosts, each also matching its subdomains
    pub hosts: Vec<String>,
    pub hosts_not: Vec<String>,
    /// source kinds, apps, or `app/profile`
    pub sources: Vec<String>,
    pub sources_not: Vec<String>,
    pub confidence: Vec<Confidence>,
    /// The run that first stored an event, not every run that observed it.
    pub run_id: Option<u64>,
    pub timing: EventTiming,
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

impl From<&TimelineFilter> for EventFilter {
    fn from(filter: &TimelineFilter) -> Self {
        Self {
            since: filter.since,
            until: filter.until,
            kinds: filter.kinds.clone(),
            kinds_not: filter.kinds_not.clone(),
            hosts: filter.hosts.clone(),
            hosts_not: filter.hosts_not.clone(),
            sources: filter.sources.clone(),
            sources_not: filter.sources_not.clone(),
            confidence: filter.confidence.clone(),
            run_id: filter.run_id,
            timing: filter.timing,
            match_text: filter.match_text.clone(),
            limit: filter.limit,
            offset: filter.offset,
            reverse: filter.reverse,
            // the keyset cursor is paging, not scope: the query layer sets it per page.
            after: None,
        }
    }
}

impl TimelineRow {
    fn from_event(row: fp_store::EventRow, iocs: &[fp_store::IocRow]) -> Self {
        let ioc_hits = match_event_iocs(
            row.subject_host.as_deref(),
            row.subject.as_deref(),
            row.summary.as_deref(),
            iocs,
        );
        Self {
            id: row.id,
            run_id: row.run_id,
            ts_utc: row.ts_utc,
            ts_precision: row.ts_precision,
            kind: row.kind,
            source_kind: row.source_kind,
            app: row.app,
            profile: row.profile,
            subject: row.subject,
            subject_host: row.subject_host,
            summary: row.summary,
            confidence: row.confidence,
            ioc_hits,
        }
    }
}

fn rows_with_iocs(store: &Store, events: Vec<fp_store::EventRow>) -> Result<Vec<TimelineRow>> {
    let iocs = store.list_iocs()?;
    Ok(events
        .into_iter()
        .map(|row| TimelineRow::from_event(row, &iocs))
        .collect())
}

pub fn timeline(store: &Store, filter: &TimelineFilter) -> Result<Vec<TimelineRow>> {
    rows_with_iocs(store, store.list_events(&EventFilter::from(filter))?)
}

/// One keyset page after `cursor` (a `(ts_utc, id)` from the last row of the previous page),
/// in the scope's order. Cheaper than `timeline_page` for sequential scrolling because it
/// never pays the `OFFSET` cost; one extra row tells whether more follow.
pub fn timeline_after(
    store: &Store,
    filter: &TimelineFilter,
    cursor: Option<(i64, u64)>,
    size: usize,
) -> Result<TimelinePage> {
    let page_filter = EventFilter {
        after: cursor,
        limit: Some(size + 1),
        offset: None,
        ..EventFilter::from(filter)
    };
    let mut rows = rows_with_iocs(store, store.list_events(&page_filter)?)?;
    let has_more = rows.len() > size;
    rows.truncate(size);
    Ok(TimelinePage { rows, has_more })
}

/// The ascending row offset to scroll to so `ts` is the first visible row: the count of
/// scoped events strictly before `ts`.
pub fn seek_offset(store: &Store, filter: &TimelineFilter, ts: i64) -> Result<u64> {
    Ok(store.seek_offset(&EventFilter::from(filter), ts)?)
}

/// The neighbours of event `id` in chronological order: up to `before` earlier and up to
/// `after` later events, excluding the event itself. `scope` narrows the neighbourhood to
/// the same filter the table shows when given. Empty when the event is unknown.
pub fn events_around(
    store: &Store,
    id: u64,
    before: usize,
    after: usize,
    scope: Option<&TimelineFilter>,
) -> Result<Vec<TimelineRow>> {
    let Some(event) = store.get_event(id)? else {
        return Ok(Vec::new());
    };
    let scope = scope.map(EventFilter::from);
    let neighbours = store.events_around(event.ts_utc, id, before, after, scope.as_ref())?;
    rows_with_iocs(store, neighbours)
}

/// Full-text search over subjects, summaries and attributes (FTS5 syntax), with IOC hits.
pub fn search(store: &Store, query: &str) -> Result<Vec<TimelineRow>> {
    rows_with_iocs(store, store.search_events(query)?)
}
