use fp_query::category::Category;
use fp_query::overview::{
    Facet, MAX_FACET_ROWS, OverviewError, category_counts, facets, timing_counts,
};
use fp_query::{EventTiming, TimelineFilter};
use fp_store::{EventFilter, Store};

fn case() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_or_create(dir.path().join("case.db")).unwrap();
    store
        .conn()
        .execute_batch(
            "INSERT INTO runs(id, started_utc) VALUES (1, 0), (2, 0);
             INSERT INTO sources(id, run_id, kind, app, profile) VALUES
               (1, 1, 'chromium.history', 'Brave', 'Default'),
               (2, 1, 'chromium.history', 'Brave', 'Work'),
               (3, 1, 'xdm.downloads', 'xdm', NULL),
               (4, 1, 'dpkg.log', NULL, NULL);
             INSERT INTO events(run_id, source_id, ts_utc, ts_precision, kind, subject_host, confidence, dedup_key) VALUES
               (1, 1, 10, 'exact', 'web.visit', 'zoom.us', 'direct', 'a'),
               (1, 1, 11, 'exact', 'web.visit', 'cdn.zoom.us', 'direct', 'b'),
               (1, 2, 12, 'exact', 'web.download', 'cdn.zoom.us', 'derived', 'c'),
               (1, 3, 13, 'exact', 'dl.manager_download', NULL, 'direct', 'd'),
               (2, 4, 14, 'exact', 'pkg.install', NULL, 'heuristic', 'e'),
               (2, 4, 0, 'order_only', 'pkg.upgrade', NULL, 'direct', 'f'),
               (2, 4, 15, 'exact', 'mystery.kind', 'a.test', 'direct', 'g');",
        )
        .unwrap();
    (dir, store)
}

fn pairs(page: &fp_store::FacetPage) -> Vec<(Option<&str>, u64)> {
    page.values
        .iter()
        .map(|value| (value.key.as_deref(), value.count))
        .collect()
}

#[test]
fn facet_values_counts_and_limits() {
    let (_dir, store) = case();
    let all = TimelineFilter::default();

    let kinds = facets(&store, &all, Facet::Kind, 3).unwrap();
    assert_eq!(kinds.distinct_values, 6);
    assert_eq!(
        pairs(&kinds),
        [
            (Some("web.visit"), 2),
            (Some("dl.manager_download"), 1),
            (Some("mystery.kind"), 1)
        ]
    );

    let sources = facets(&store, &all, Facet::Source, 10).unwrap();
    assert_eq!(
        pairs(&sources),
        [
            (Some("dpkg.log"), 3),
            (Some("Brave/Default"), 2),
            (Some("Brave/Work"), 1),
            (Some("xdm"), 1)
        ]
    );
    let hosts = facets(&store, &all, Facet::Host, 10).unwrap();
    assert_eq!(hosts.distinct_values, 4);
    assert_eq!(
        hosts.values[0],
        fp_store::FacetCount {
            key: None,
            count: 3
        }
    );
    assert_eq!(
        pairs(&facets(&store, &all, Facet::Confidence, 10).unwrap()),
        [
            (Some("direct"), 5),
            (Some("derived"), 1),
            (Some("heuristic"), 1)
        ]
    );
    assert_eq!(
        pairs(&facets(&store, &all, Facet::Run, 10).unwrap()),
        [(Some("1"), 4), (Some("2"), 3)]
    );

    let none = TimelineFilter {
        match_text: Some("no such text".into()),
        ..Default::default()
    };
    let empty = facets(&store, &none, Facet::Kind, 5).unwrap();
    assert!(empty.values.is_empty());
    assert_eq!(empty.distinct_values, 0);

    for limit in [0, MAX_FACET_ROWS + 1] {
        assert!(matches!(
            facets(&store, &all, Facet::Kind, limit),
            Err(OverviewError::InvalidLimit(_))
        ));
    }
}

#[test]
fn a_facet_ignores_only_its_own_selection() {
    let (_dir, store) = case();
    let scope = TimelineFilter {
        kinds: vec!["web.*".into()],
        kinds_not: vec!["web.download".into()],
        hosts_not: vec!["cdn.zoom.us".into()],
        sources: vec!["Brave".into()],
        confidence: vec![fp_core::Confidence::Direct],
        run_id: Some(1),
        limit: Some(1),
        offset: Some(5),
        reverse: true,
        ..Default::default()
    };

    // kinds: own `web.*` and exclusion removed, every other selection kept
    assert_eq!(
        pairs(&facets(&store, &scope, Facet::Kind, 10).unwrap()),
        [(Some("web.visit"), 1)]
    );
    // hosts: the excluded host still shows what it would add back
    assert_eq!(
        pairs(&facets(&store, &scope, Facet::Host, 10).unwrap()),
        [(Some("cdn.zoom.us"), 1), (Some("zoom.us"), 1)]
    );
    assert_eq!(
        pairs(&facets(&store, &scope, Facet::Source, 10).unwrap()),
        [(Some("Brave/Default"), 1)]
    );
    assert_eq!(
        pairs(&facets(&store, &scope, Facet::Confidence, 10).unwrap()),
        [(Some("direct"), 1)]
    );
    assert_eq!(
        pairs(&facets(&store, &scope, Facet::Run, 10).unwrap()),
        [(Some("1"), 1)]
    );
}

#[test]
fn categories_and_timing_fold_without_paging() {
    let (_dir, store) = case();
    let counts = category_counts(&store, &TimelineFilter::default()).unwrap();
    let get = |category: Category| counts[category as usize];
    assert_eq!(get(Category::Web), 2);
    assert_eq!(get(Category::Downloads), 2);
    assert_eq!(get(Category::Packages), 2);
    assert_eq!(get(Category::Other), 1);
    assert_eq!(counts.iter().sum::<u64>(), 7);

    // a category selection is expressed as kinds, so it is removed from its own facet
    let selected = TimelineFilter {
        kinds: vec!["pkg.*".into()],
        sources_not: vec!["dpkg".into()],
        limit: Some(1),
        ..Default::default()
    };
    let counts = category_counts(&store, &selected).unwrap();
    assert_eq!(counts.iter().sum::<u64>(), 4);
    assert_eq!(counts[Category::Packages as usize], 0);

    let timing = timing_counts(
        &store,
        &TimelineFilter {
            timing: EventTiming::Timed,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((timing.timed, timing.untimed), (6, 1));
    let scoped = timing_counts(
        &store,
        &TimelineFilter {
            hosts: vec!["zoom.us".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!((scoped.timed, scoped.untimed), (3, 0));
}

#[test]
fn facet_counts_agree_with_the_timeline_total() {
    let (_dir, store) = case();
    let scope = TimelineFilter {
        hosts: vec!["zoom.us".into()],
        ..Default::default()
    };
    let total = store.count_events(&EventFilter::from(&scope)).unwrap();
    for facet in [Facet::Kind, Facet::Source, Facet::Confidence, Facet::Run] {
        let page = facets(&store, &scope, facet, 500).unwrap();
        assert_eq!(page.values.iter().map(|v| v.count).sum::<u64>(), total);
    }
}
