use crate::category::{Category, category};
use crate::{TimelineFilter, Tz, format_time};
use fp_store::{EventFilter, EventTiming, FacetField, FacetPage, GroupField, Store, StoreError};
use std::collections::HashMap;
use std::path::Path;
use thiserror::Error;

pub use fp_store::{HostSort, HostSummary};

const SECOND_US: u128 = 1_000_000;
const MINUTE_US: u128 = 60 * SECOND_US;
const HOUR_US: u128 = 60 * MINUTE_US;
const DAY_US: u128 = 24 * HOUR_US;
const PIXELS_PER_BUCKET: usize = 4;
pub const MAX_HISTOGRAM_BUCKETS: usize = 600;
const BUCKET_LADDER: [u128; 9] = [
    SECOND_US,
    5 * SECOND_US,
    30 * SECOND_US,
    MINUTE_US,
    5 * MINUTE_US,
    15 * MINUTE_US,
    HOUR_US,
    6 * HOUR_US,
    DAY_US,
];

#[derive(Debug, Error)]
pub enum OverviewError {
    #[error("histogram width must be greater than zero")]
    InvalidWidth,
    #[error("histogram span must be greater than zero")]
    InvalidSpan,
    #[error("histogram bucket calculation overflowed")]
    BucketOverflow,
    #[error("facet limit {0} must be between 1 and 500")]
    InvalidLimit(usize),
    #[error("histogram count overflowed")]
    CountOverflow,
    #[error("event timestamp {0} is outside the histogram window")]
    EventOutsideWindow(i64),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// UTC Unix microseconds; both endpoints are included, including `i64::MAX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeWindow {
    pub start_utc: i64,
    pub end_utc: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistogramBucket {
    /// Inclusive endpoints, clipped to the selected window.
    pub start_utc: i64,
    pub end_utc: i64,
    /// Counts in [`Category::ALL`] order.
    pub counts: [u64; Category::ALL.len()],
}

impl HistogramBucket {
    pub fn count(&self, category: Category) -> u64 {
        self.counts[category as usize]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Histogram {
    pub window: TimeWindow,
    pub bucket_width_us: u128,
    pub buckets: Vec<HistogramBucket>,
}

/// Picks a UTC duration for roughly one bucket per four pixels, capped at 600.
/// Widths below four pixels still receive one bucket.
pub fn bucket_for(span_us: u128, width_px: usize) -> Result<u128, OverviewError> {
    if span_us == 0 {
        return Err(OverviewError::InvalidSpan);
    }
    if width_px == 0 {
        return Err(OverviewError::InvalidWidth);
    }
    let target = (width_px / PIXELS_PER_BUCKET).clamp(1, MAX_HISTOGRAM_BUCKETS);
    let desired = span_us.div_ceil(target as u128);
    if let Some(width) = BUCKET_LADDER.into_iter().find(|width| *width >= desired) {
        return Ok(width);
    }
    desired
        .div_ceil(DAY_US)
        .checked_mul(DAY_US)
        .ok_or(OverviewError::BucketOverflow)
}

fn timestamp_at(start: i64, offset: u128) -> Result<i64, OverviewError> {
    let offset = i128::try_from(offset).map_err(|_| OverviewError::BucketOverflow)?;
    let timestamp = i128::from(start)
        .checked_add(offset)
        .ok_or(OverviewError::BucketOverflow)?;
    i64::try_from(timestamp).map_err(|_| OverviewError::BucketOverflow)
}

fn prepare_buckets(
    window: TimeWindow,
    width_px: usize,
) -> Result<(u128, Vec<(i64, i64)>), OverviewError> {
    if window.start_utc > window.end_utc {
        return Err(StoreError::InvalidTimeRange {
            start: window.start_utc,
            end: window.end_utc,
        }
        .into());
    }
    let span = (i128::from(window.end_utc) - i128::from(window.start_utc) + 1) as u128;
    let bucket_width_us = bucket_for(span, width_px)?;
    let count = usize::try_from(span.div_ceil(bucket_width_us))
        .map_err(|_| OverviewError::BucketOverflow)?;
    if count > MAX_HISTOGRAM_BUCKETS {
        return Err(OverviewError::BucketOverflow);
    }
    let mut bounds = Vec::with_capacity(count);
    for index in 0..count {
        let start = index as u128 * bucket_width_us;
        let end = (start + bucket_width_us).min(span) - 1;
        bounds.push((
            timestamp_at(window.start_utc, start)?,
            timestamp_at(window.start_utc, end)?,
        ));
    }
    Ok((bucket_width_us, bounds))
}

fn bucket_index(
    ts: i64,
    start_utc: i64,
    bucket_width_us: u128,
    bucket_len: usize,
) -> Option<usize> {
    let offset = i128::from(ts) - i128::from(start_utc);
    let idx = u128::try_from(offset)
        .ok()
        .and_then(|offset| usize::try_from(offset / bucket_width_us).ok())?;
    (idx < bucket_len).then_some(idx)
}

/// Aggregates scoped timed events into UTC buckets anchored at the window start.
/// The scope's time bounds are intersected with the window; paging and order are ignored.
/// Empty windows of data still return zero-filled buckets, and untimed events are excluded.
pub fn histogram(
    store: &Store,
    filter: &TimelineFilter,
    window: TimeWindow,
    width_px: usize,
) -> Result<Histogram, OverviewError> {
    let (bucket_width_us, bounds) = prepare_buckets(window, width_px)?;
    let mut buckets: Vec<HistogramBucket> = bounds
        .into_iter()
        .map(|(start_utc, end_utc)| HistogramBucket {
            start_utc,
            end_utc,
            counts: [0; Category::ALL.len()],
        })
        .collect();

    let mut outside_window = None;
    let mut overflow = false;
    let span_delta = i128::from(window.end_utc) - i128::from(window.start_utc);
    if span_delta <= i128::from(i64::MAX)
        && let Ok(width_i64) = i64::try_from(bucket_width_us)
    {
        store.visit_histogram_buckets(
            &EventFilter::from(filter),
            window.start_utc,
            window.end_utc,
            width_i64,
            |idx, kind, count| {
                if let Some(bucket) = buckets.get_mut(idx) {
                    let slot = &mut bucket.counts[category(kind) as usize];
                    if let Some(next) = slot.checked_add(count) {
                        *slot = next;
                    } else {
                        overflow = true;
                    }
                } else {
                    outside_window = Some(window.end_utc);
                }
            },
        )?;
    } else {
        store.visit_timed_events(
            &EventFilter::from(filter),
            window.start_utc,
            window.end_utc,
            |ts, kind| {
                if let Some(bucket) =
                    bucket_index(ts, window.start_utc, bucket_width_us, buckets.len())
                        .and_then(|index| buckets.get_mut(index))
                {
                    let count = &mut bucket.counts[category(kind) as usize];
                    if let Some(next) = count.checked_add(1) {
                        *count = next;
                    } else {
                        overflow = true;
                    }
                } else {
                    outside_window = Some(ts);
                }
            },
        )?;
    }
    if let Some(ts) = outside_window {
        return Err(OverviewError::EventOutsideWindow(ts));
    }
    if overflow {
        return Err(OverviewError::CountOverflow);
    }
    Ok(Histogram {
        window,
        bucket_width_us,
        buckets,
    })
}

pub const MAX_FACET_ROWS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Facet {
    Kind,
    Source,
    Host,
    Confidence,
    Run,
}

impl Facet {
    fn field(self) -> FacetField {
        match self {
            Self::Kind => FacetField::Kind,
            Self::Source => FacetField::Source,
            Self::Host => FacetField::Host,
            Self::Confidence => FacetField::Confidence,
            Self::Run => FacetField::Run,
        }
    }

    /// The scope without this facet's own selections, so excluded values keep their counts.
    pub fn without(self, filter: &TimelineFilter) -> TimelineFilter {
        let mut scope = filter.clone();
        match self {
            Self::Kind => {
                scope.kinds.clear();
                scope.kinds_not.clear();
            }
            Self::Source => {
                scope.sources.clear();
                scope.sources_not.clear();
            }
            Self::Host => {
                scope.hosts.clear();
                scope.hosts_not.clear();
            }
            Self::Confidence => scope.confidence.clear(),
            Self::Run => scope.run_id = None,
        }
        scope
    }
}

/// Counts for one facet under the scope minus that facet's own selections.
/// Paging and order are ignored; `limit` is between 1 and [`MAX_FACET_ROWS`].
pub fn facets(
    store: &Store,
    filter: &TimelineFilter,
    facet: Facet,
    limit: usize,
) -> Result<FacetPage, OverviewError> {
    if limit == 0 || limit > MAX_FACET_ROWS {
        return Err(OverviewError::InvalidLimit(limit));
    }
    let scope = facet.without(filter);
    Ok(store.facet_counts(&EventFilter::from(&scope), facet.field(), limit)?)
}

/// Event counts per category, folded from kind counts under the scope minus kind selections.
pub fn category_counts(
    store: &Store,
    filter: &TimelineFilter,
) -> Result<[u64; Category::ALL.len()], OverviewError> {
    let scope = Facet::Kind.without(filter);
    let mut counts = [0u64; Category::ALL.len()];
    let mut overflow = false;
    store.visit_kind_counts(&EventFilter::from(&scope), |kind, count| {
        let slot = &mut counts[category(kind) as usize];
        match slot.checked_add(count) {
            Some(total) => *slot = total,
            None => overflow = true,
        }
    })?;
    if overflow {
        return Err(OverviewError::CountOverflow);
    }
    Ok(counts)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimingCounts {
    pub timed: u64,
    pub untimed: u64,
}

/// Timed and untimed counts under the scope minus its timing selection.
pub fn timing_counts(
    store: &Store,
    filter: &TimelineFilter,
) -> Result<TimingCounts, OverviewError> {
    let count = |timing| {
        store.count_events(&EventFilter {
            timing,
            ..EventFilter::from(filter)
        })
    };
    Ok(TimingCounts {
        timed: count(EventTiming::Timed)?,
        untimed: count(EventTiming::Untimed)?,
    })
}

/// Dimensions the `list --group-by` aggregation can count events over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    Dir,
    Host,
    Source,
    Kind,
    Minute,
}

/// One aggregated group: its display key, how many events it holds, and the first
/// and last timed instant among them (`None` when every member is untimed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupRow {
    pub key: String,
    pub count: u64,
    pub first_ts: Option<i64>,
    pub last_ts: Option<i64>,
}

/// The directory key for a subject, matching the CLI's historical `--group-by dir`:
/// the parent path when the subject looks like a path, `/` when it has no parent,
/// and `-` when the subject holds no path at all.
fn dir_key(subject: Option<&str>) -> String {
    let subject = subject.unwrap_or("");
    if subject.contains('/') {
        Path::new(subject)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "/".into())
    } else {
        "-".into()
    }
}

/// Counts events per value of a dimension under the scope, newest-first by count then key.
/// `Host`/`Source`/`Kind` aggregate in SQL; `Dir` and `Minute` fold in Rust because they
/// depend on path parsing and zone-aware time rendering. `limit` bounds the number of
/// groups (`None` returns them all); `tz` only affects `Minute`.
pub fn group(
    store: &Store,
    filter: &TimelineFilter,
    by: GroupBy,
    limit: Option<usize>,
    tz: Tz,
) -> Result<Vec<GroupRow>, OverviewError> {
    let mut rows: Vec<GroupRow> = match by {
        GroupBy::Host | GroupBy::Source | GroupBy::Kind => {
            let field = match by {
                GroupBy::Host => GroupField::Host,
                GroupBy::Source => GroupField::Source,
                GroupBy::Kind => GroupField::Kind,
                _ => unreachable!(),
            };
            store
                .group_counts(&EventFilter::from(filter), field, None)?
                .into_iter()
                .map(|g| GroupRow {
                    key: g.key.unwrap_or_else(|| "-".into()),
                    count: g.count,
                    first_ts: g.first_ts,
                    last_ts: g.last_ts,
                })
                .collect()
        }
        GroupBy::Dir | GroupBy::Minute => {
            // like the SQL dimensions, the filter's own paging must not truncate the fold
            let unpaged = EventFilter {
                limit: None,
                offset: None,
                ..EventFilter::from(filter)
            };
            let mut folded: HashMap<String, GroupRow> = HashMap::new();
            for row in store.list_events(&unpaged)? {
                let key = match by {
                    GroupBy::Dir => dir_key(row.subject.as_deref()),
                    GroupBy::Minute => {
                        format_time(row.ts_utc, &row.ts_precision, tz, "%Y-%m-%d %H:%M")
                    }
                    _ => unreachable!(),
                };
                let timed = row.ts_precision != "order_only";
                let entry = folded.entry(key.clone()).or_insert_with(|| GroupRow {
                    key,
                    count: 0,
                    first_ts: None,
                    last_ts: None,
                });
                entry.count = entry
                    .count
                    .checked_add(1)
                    .ok_or(OverviewError::CountOverflow)?;
                if timed {
                    entry.first_ts = Some(entry.first_ts.map_or(row.ts_utc, |t| t.min(row.ts_utc)));
                    entry.last_ts = Some(entry.last_ts.map_or(row.ts_utc, |t| t.max(row.ts_utc)));
                }
            }
            folded.into_values().collect()
        }
    };
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key.cmp(&b.key)));
    if let Some(limit) = limit {
        rows.truncate(limit);
    }
    Ok(rows)
}

/// The busiest hosts in scope, top `limit` by `sort`, with case-wide first-seen for the
/// `NEW` badge. `limit` is 1..=[`MAX_FACET_ROWS`].
pub fn hosts(
    store: &Store,
    filter: &TimelineFilter,
    sort: HostSort,
    limit: usize,
) -> Result<Vec<HostSummary>, OverviewError> {
    if limit == 0 || limit > MAX_FACET_ROWS {
        return Err(OverviewError::InvalidLimit(limit));
    }
    Ok(store.host_summaries(&EventFilter::from(filter), sort, limit)?)
}

/// Host summary enriched with scoped category breakdown, distinct source labels, and
/// `ioc_match` status for the page's hosts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostOverview {
    pub summary: HostSummary,
    pub category_counts: [u64; Category::ALL.len()],
    pub sources: Vec<String>,
    pub has_ioc: bool,
}

pub fn hosts_overview(
    store: &Store,
    filter: &TimelineFilter,
    host_substr: Option<&str>,
    sort: HostSort,
    limit: usize,
) -> Result<Vec<HostOverview>, OverviewError> {
    if limit == 0 || limit > MAX_FACET_ROWS {
        return Err(OverviewError::InvalidLimit(limit));
    }
    let event_filter = EventFilter::from(filter);
    let summaries = store.host_summaries_matching(&event_filter, host_substr, sort, limit)?;
    let host_refs: Vec<&str> = summaries.iter().map(|s| s.host.as_str()).collect();
    let breakdown = store.host_breakdown(&event_filter, &host_refs)?;

    let mut cats_by_host: HashMap<&str, [u64; Category::ALL.len()]> = HashMap::new();
    for (host, kind, count) in &breakdown.kind_counts {
        let entry = cats_by_host
            .entry(host.as_str())
            .or_insert([0; Category::ALL.len()]);
        entry[category(kind) as usize] += count;
    }

    let mut srcs_by_host: HashMap<&str, Vec<String>> = HashMap::new();
    for (host, src) in &breakdown.sources {
        srcs_by_host
            .entry(host.as_str())
            .or_default()
            .push(src.clone());
    }

    let ioc_set: std::collections::HashSet<&str> =
        breakdown.ioc_hosts.iter().map(String::as_str).collect();

    Ok(summaries
        .into_iter()
        .map(|summary| {
            let category_counts = cats_by_host
                .get(summary.host.as_str())
                .copied()
                .unwrap_or([0; Category::ALL.len()]);
            let sources = srcs_by_host
                .remove(summary.host.as_str())
                .unwrap_or_default();
            let has_ioc = ioc_set.contains(summary.host.as_str());
            HostOverview {
                summary,
                category_counts,
                sources,
                has_ioc,
            }
        })
        .collect())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum LaneMode {
    #[default]
    Source,
    Category,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneHistogramRow {
    pub label: String,
    pub total: u64,
    pub category_hint: Category,
    pub buckets: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaneHistogram {
    pub mode: LaneMode,
    pub window: TimeWindow,
    pub bucket_width_us: u128,
    pub bucket_bounds: Vec<(i64, i64)>,
    pub lanes: Vec<LaneHistogramRow>,
}

/// Aggregates timed events in `window` into per-source or per-category density lanes.
pub fn lane_histogram(
    store: &Store,
    filter: &TimelineFilter,
    window: TimeWindow,
    width_px: usize,
    mode: LaneMode,
) -> Result<LaneHistogram, OverviewError> {
    let (bucket_width_us, bucket_bounds) = prepare_buckets(window, width_px)?;
    let bucket_len = bucket_bounds.len();

    struct Accum {
        total: u64,
        cat_counts: [u64; Category::ALL.len()],
        buckets: Vec<u64>,
    }
    let mut by_lane: HashMap<String, Accum> = HashMap::new();
    let mut outside_window = None;
    let mut overflow = false;

    store.visit_timed_events_with_source(
        &EventFilter::from(filter),
        window.start_utc,
        window.end_utc,
        |ts, kind, src| {
            let Some(idx) = bucket_index(ts, window.start_utc, bucket_width_us, bucket_len) else {
                outside_window = Some(ts);
                return;
            };
            let cat = category(kind);
            let lane_key = match mode {
                LaneMode::Source => src,
                LaneMode::Category => cat.label(),
            };
            let acc = by_lane
                .entry(lane_key.to_string())
                .or_insert_with(|| Accum {
                    total: 0,
                    cat_counts: [0; Category::ALL.len()],
                    buckets: vec![0; bucket_len],
                });
            if let (Some(nt), Some(nb)) =
                (acc.total.checked_add(1), acc.buckets[idx].checked_add(1))
            {
                acc.total = nt;
                acc.buckets[idx] = nb;
                acc.cat_counts[cat as usize] += 1;
            } else {
                overflow = true;
            }
        },
    )?;

    if let Some(ts) = outside_window {
        return Err(OverviewError::EventOutsideWindow(ts));
    }
    if overflow {
        return Err(OverviewError::CountOverflow);
    }

    let mut lanes: Vec<LaneHistogramRow> = by_lane
        .into_iter()
        .map(|(label, acc)| {
            let dominant_idx = acc
                .cat_counts
                .iter()
                .enumerate()
                .max_by_key(|&(_, c)| *c)
                .map_or(Category::Other as usize, |(i, _)| i);
            LaneHistogramRow {
                label,
                total: acc.total,
                category_hint: Category::ALL[dominant_idx],
                buckets: acc.buckets,
            }
        })
        .collect();
    lanes.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.label.cmp(&b.label)));

    Ok(LaneHistogram {
        mode,
        window,
        bucket_width_us,
        bucket_bounds,
        lanes,
    })
}
