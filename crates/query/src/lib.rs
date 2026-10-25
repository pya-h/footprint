//! The read model and the collect runner. Frontends (CLI, TUI, a future GUI) call this
//! crate; no SQL lives outside it.

pub mod time;

mod collect;
mod detail;
mod diff;
mod explain;
mod sources;
mod state;
mod timeline;

pub use collect::{CollectEvent, CollectParams, CollectSummary, run_collect, selected};
pub use detail::{EventDetailView, LinkDirection, LinkView, event_detail};
pub use diff::{DiffChange, DiffEntry, diff_runs};
pub use explain::{EXPLANATIONS, Explanation, explain};
pub use sources::{CoverageRow, CoverageStatus, SourceView, coverage, sources};
pub use state::{StateView, state_details, state_records};
pub use timeline::{
    PAGE_SIZE, TimelineFilter, TimelinePage, TimelineRow, search, timeline, timeline_page,
};

pub use fp_correlate::{IocHit, TraceArrow, TraceHead, TraceTree, check_iocs, trace_tree};

use chrono::{DateTime, Local, Utc};

/// Which zone to render instants in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tz {
    Local,
    Utc,
}

/// Renders an event timestamp; `order_only` events have no instant to show.
pub fn format_time(ts_utc: i64, precision: &str, tz: Tz, pattern: &str) -> String {
    if precision == "order_only" {
        return "(untimed)".to_string();
    }
    DateTime::from_timestamp_micros(ts_utc)
        .map(|t| match tz {
            Tz::Local => t.with_timezone(&Local).format(pattern).to_string(),
            Tz::Utc => t.with_timezone(&Utc).format(pattern).to_string(),
        })
        .unwrap_or_else(|| format!("@{ts_utc}µs"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untimed_and_fallback() {
        assert_eq!(format_time(0, "order_only", Tz::Utc, "%H:%M"), "(untimed)");
        assert!(format_time(i64::MAX, "exact", Tz::Utc, "%H:%M").starts_with('@'));
    }
}
