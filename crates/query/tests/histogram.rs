use fp_query::TimelineFilter;
use fp_query::category::Category;
use fp_query::overview::{MAX_HISTOGRAM_BUCKETS, OverviewError, TimeWindow, bucket_for, histogram};
use fp_store::Store;

fn case() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_or_create(dir.path().join("case.db")).unwrap();
    store
        .conn()
        .execute_batch(
            "INSERT INTO runs(id, started_utc) VALUES (1, 0);
             INSERT INTO sources(id, run_id, kind, app) VALUES (1, 1, 'test', 'test');",
        )
        .unwrap();
    (dir, store)
}

#[test]
fn ladder_and_bucket_limit_cover_large_spans() {
    assert!(matches!(
        bucket_for(0, 100),
        Err(OverviewError::InvalidSpan)
    ));
    assert!(matches!(bucket_for(1, 0), Err(OverviewError::InvalidWidth)));
    assert!(matches!(
        bucket_for(u128::MAX, 4),
        Err(OverviewError::BucketOverflow)
    ));
    assert_eq!(bucket_for(1, 4).unwrap(), 1_000_000);
    assert_eq!(bucket_for(5_000_000, 4).unwrap(), 5_000_000);
    assert_eq!(bucket_for(31_000_000, 4).unwrap(), 60_000_000);
    assert_eq!(
        bucket_for(1_000 * 86_400_000_000, 4).unwrap(),
        1_000 * 86_400_000_000
    );
    let span = 3_600_000_000_001;
    let width = bucket_for(span, usize::MAX).unwrap();
    assert!(span.div_ceil(width) <= MAX_HISTOGRAM_BUCKETS as u128);
}

#[test]
fn inclusive_utc_boundaries_categories_and_untimed() {
    let (dir, store) = case();
    store
        .conn()
        .execute_batch(
            "INSERT INTO events(run_id, source_id, ts_utc, ts_precision, kind, dedup_key) VALUES
             (1, 1, -1000001, 'exact', 'web.visit', 'before'),
             (1, 1, -1000000, 'exact', 'web.visit', 'first'),
             (1, 1, -1, 'derived', 'web.download', 'negative'),
             (1, 1, 0, 'exact', 'dl.manager_download', 'zero'),
             (1, 1, 0, 'order_only', 'web.visit', 'untimed'),
             (1, 1, 999999, 'exact', 'pkg.install', 'middle'),
             (1, 1, 1000000, 'exact', 'auth.login', 'boundary'),
             (1, 1, 1999999, 'exact', 'unrecognized', 'unknown'),
             (1, 1, 2000000, 'exact', 'shell.command', 'last'),
             (1, 1, 2000001, 'exact', 'web.visit', 'after');",
        )
        .unwrap();
    let window = TimeWindow {
        start_utc: -1_000_000,
        end_utc: 2_000_000,
    };
    let result = histogram(&store, &TimelineFilter::default(), window, 16).unwrap();
    assert_eq!(result.window, window);
    assert_eq!(result.bucket_width_us, 1_000_000);
    assert_eq!(result.buckets.len(), 4);
    assert_eq!(
        result
            .buckets
            .iter()
            .map(|bucket| (bucket.start_utc, bucket.end_utc))
            .collect::<Vec<_>>(),
        [
            (-1_000_000, -1),
            (0, 999_999),
            (1_000_000, 1_999_999),
            (2_000_000, 2_000_000)
        ]
    );
    assert_eq!(result.buckets[0].count(Category::Web), 1);
    assert_eq!(result.buckets[0].count(Category::Downloads), 1);
    assert_eq!(result.buckets[1].count(Category::Downloads), 1);
    assert_eq!(result.buckets[1].count(Category::Packages), 1);
    assert_eq!(result.buckets[2].count(Category::Access), 1);
    assert_eq!(result.buckets[2].count(Category::Other), 1);
    assert_eq!(result.buckets[3].count(Category::Shell), 1);
    assert_eq!(
        result
            .buckets
            .iter()
            .flat_map(|bucket| bucket.counts)
            .sum::<u64>(),
        7
    );

    store
        .conn()
        .execute(
            "UPDATE events SET subject = 'literal_100%' WHERE dedup_key = 'zero'",
            [],
        )
        .unwrap();
    let read_only = Store::open(dir.path().join("case.db")).unwrap();
    let scoped = histogram(
        &read_only,
        &TimelineFilter {
            since: Some(0),
            until: Some(0),
            match_text: Some("literal_100%".into()),
            limit: Some(0),
            offset: Some(100),
            ..Default::default()
        },
        window,
        16,
    )
    .unwrap();
    assert_eq!(scoped.buckets[1].count(Category::Downloads), 1);
    assert_eq!(
        scoped
            .buckets
            .iter()
            .flat_map(|bucket| bucket.counts)
            .sum::<u64>(),
        1
    );
    let untimed_scope = histogram(
        &read_only,
        &TimelineFilter {
            timing: fp_query::EventTiming::Untimed,
            ..Default::default()
        },
        window,
        16,
    )
    .unwrap();
    assert!(
        untimed_scope
            .buckets
            .iter()
            .flat_map(|bucket| bucket.counts)
            .all(|count| count == 0)
    );
}

#[test]
fn extremes_empty_windows_and_utc_daylight_saving_transition() {
    let (_dir, store) = case();
    store
        .conn()
        .execute_batch(
            "INSERT INTO events(run_id, source_id, ts_utc, ts_precision, kind, dedup_key) VALUES
             (1, 1, -9223372036854775808, 'exact', 'web.visit', 'min'),
             (1, 1, 9223372036854775807, 'exact', 'web.download', 'max'),
             (1, 1, 1710053940000000, 'exact', 'web.visit', 'dst-before'),
             (1, 1, 1710054000000000, 'exact', 'web.visit', 'dst-after');",
        )
        .unwrap();
    let full = histogram(
        &store,
        &TimelineFilter::default(),
        TimeWindow {
            start_utc: i64::MIN,
            end_utc: i64::MAX,
        },
        4,
    )
    .unwrap();
    assert_eq!(full.buckets.len(), 1);
    assert_eq!(full.buckets[0].start_utc, i64::MIN);
    assert_eq!(full.buckets[0].end_utc, i64::MAX);
    assert_eq!(full.buckets[0].count(Category::Web), 3);
    assert_eq!(full.buckets[0].count(Category::Downloads), 1);
    let empty = histogram(
        &store,
        &TimelineFilter::default(),
        TimeWindow {
            start_utc: 0,
            end_utc: 0,
        },
        4,
    )
    .unwrap();
    assert_eq!(empty.buckets.len(), 1);
    assert_eq!(empty.buckets[0].counts, [0; Category::ALL.len()]);
    assert!(
        histogram(
            &store,
            &TimelineFilter::default(),
            TimeWindow {
                start_utc: 1,
                end_utc: 0
            },
            4
        )
        .is_err()
    );
    assert!(
        histogram(
            &store,
            &TimelineFilter::default(),
            TimeWindow {
                start_utc: 0,
                end_utc: 0
            },
            0
        )
        .is_err()
    );

    let dst = histogram(
        &store,
        &TimelineFilter::default(),
        TimeWindow {
            start_utc: 1_710_053_940_000_000,
            end_utc: 1_710_054_059_999_999,
        },
        8,
    )
    .unwrap();
    assert_eq!(dst.bucket_width_us, 60_000_000);
    assert_eq!(dst.buckets.len(), 2);
    assert_eq!(dst.buckets[0].count(Category::Web), 1);
    assert_eq!(dst.buckets[1].count(Category::Web), 1);
}
