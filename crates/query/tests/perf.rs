//! Ignored performance tests asserting the read model's budgets (true count, keyset
//! page, host facet, histogram) at 100k and 1m events. Run with
//! `cargo test -p fp-query --release --test perf -- --ignored`. Timings are warm-cache
//! medians over repeated runs on one host, not cold-cache or end-to-end GUI guarantees.

use fp_fixtures::bulk_case;
use fp_query::overview::{self, Facet};
use fp_query::{TimelineFilter, timeline_after};
use fp_store::{EventFilter, Store};
use std::time::{Duration, Instant};

fn timed(runs: usize, f: impl Fn() -> usize) -> Duration {
    let mut samples: Vec<Duration> = (0..runs)
        .map(|_| {
            let start = Instant::now();
            std::hint::black_box(f());
            start.elapsed()
        })
        .collect();
    samples.sort();
    samples[runs / 2]
}

fn check_budgets(db: &std::path::Path, budgets: [(&str, u64); 4]) {
    let store = Store::open(db).unwrap();
    let scope = TimelineFilter::default();
    let bounds = store.time_bounds().unwrap().unwrap();
    let window = overview::TimeWindow {
        start_utc: bounds.0,
        end_utc: bounds.1,
    };

    let measured = [
        timed(5, || {
            store.count_events(&EventFilter::default()).unwrap() as usize
        }),
        timed(5, || {
            timeline_after(&store, &scope, None, 500)
                .unwrap()
                .rows
                .len()
        }),
        timed(3, || {
            overview::facets(&store, &scope, Facet::Host, 500)
                .unwrap()
                .values
                .len()
        }),
        timed(3, || {
            overview::histogram(&store, &scope, window, 1200)
                .unwrap()
                .buckets
                .len()
        }),
    ];

    let violations: Vec<String> = budgets
        .iter()
        .zip(measured)
        .filter(|((_, budget), taken)| taken > &Duration::from_millis(*budget))
        .map(|((name, budget), taken)| format!("{name}: {taken:?} over {budget} ms"))
        .collect();
    for ((name, budget), taken) in budgets.iter().zip(measured) {
        println!(
            "{name}: {:>6.1} ms (budget {budget} ms)",
            taken.as_secs_f64() * 1e3
        );
    }
    assert!(violations.is_empty(), "budget violations: {violations:?}");
}

#[test]
#[ignore = "perf budgets: run explicitly in release"]
fn budgets_at_100k_events() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("case.db");
    bulk_case(&db, 100_000, 10, 7).unwrap();
    check_budgets(
        &db,
        [
            ("count", 100),
            ("page", 30),
            ("facet", 150),
            ("histogram", 150),
        ],
    );
}

#[test]
#[ignore = "perf budgets: run explicitly in release"]
fn budgets_at_1m_events() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("case.db");
    bulk_case(&db, 1_000_000, 30, 7).unwrap();
    check_budgets(
        &db,
        [
            ("count", 400),
            ("page", 60),
            ("facet", 700),
            ("histogram", 600),
        ],
    );
}
