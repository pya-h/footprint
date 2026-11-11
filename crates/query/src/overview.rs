use crate::TimelineFilter;
use crate::category::{Category, category};
use fp_store::{EventFilter, Store, StoreError};
use thiserror::Error;

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

/// Aggregates scoped timed events into UTC buckets anchored at the window start.
/// The scope's time bounds are intersected with the window; paging and order are ignored.
/// Empty windows of data still return zero-filled buckets, and untimed events are excluded.
pub fn histogram(
    store: &Store,
    filter: &TimelineFilter,
    window: TimeWindow,
    width_px: usize,
) -> Result<Histogram, OverviewError> {
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
    let mut buckets = Vec::with_capacity(count);
    for index in 0..count {
        let start = index as u128 * bucket_width_us;
        let end = (start + bucket_width_us).min(span) - 1;
        buckets.push(HistogramBucket {
            start_utc: timestamp_at(window.start_utc, start)?,
            end_utc: timestamp_at(window.start_utc, end)?,
            counts: [0; Category::ALL.len()],
        });
    }

    let mut outside_window = None;
    let mut overflow = false;
    store.visit_timed_events(
        &EventFilter::from(filter),
        window.start_utc,
        window.end_utc,
        |ts, kind| {
            let offset = i128::from(ts) - i128::from(window.start_utc);
            let index = u128::try_from(offset)
                .ok()
                .and_then(|offset| usize::try_from(offset / bucket_width_us).ok());
            if let Some(bucket) = index.and_then(|index| buckets.get_mut(index)) {
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
