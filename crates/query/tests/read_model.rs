use fp_fixtures::{SCENARIO_TZ, fake_zoom, local};

struct Case {
    _dir: tempfile::TempDir,
    db: std::path::PathBuf,
}

fn setup() -> Case {
    let dir = tempfile::tempdir().unwrap();
    fake_zoom(dir.path()).unwrap();
    let db = dir.path().join("case.db");
    let summary = fp_query::run_collect(
        &db,
        &fp_query::CollectParams {
            since: Some(format!("@{}", local(0, 0, 0))),
            until: Some(format!("@{}", local(23, 59, 59))),
            assume_tz: Some(SCENARIO_TZ.into()),
            homes: vec![dir.path().join("home/u")],
            root: dir.path().join("root"),
            ..Default::default()
        },
        None,
    )
    .unwrap();
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    Case { _dir: dir, db }
}

#[test]
fn timeline_filters_and_rows() {
    let case = setup();
    let store = fp_store::Store::open(&case.db).unwrap();

    let rows = fp_query::timeline(
        &store,
        &fp_query::TimelineFilter {
            kinds: vec!["dl.*".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, "dl.manager_download");
    assert_eq!(rows[0].source_label(), "xdm");
    assert!(rows[0].summary.as_ref().unwrap().contains("zoom_amd64.deb"));

    let zoom = fp_query::timeline(
        &store,
        &fp_query::TimelineFilter {
            hosts: vec!["zoom.us".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        zoom.iter()
            .any(|r| r.subject_host.as_deref() == Some("cdn.zoom.us"))
    );

    let web = fp_query::timeline(
        &store,
        &fp_query::TimelineFilter {
            sources: vec!["Brave".into()],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!web.is_empty());
    assert!(web.iter().all(|r| r.app.as_deref() == Some("Brave")));
}

#[test]
fn timeline_forwards_extended_filters() {
    let case = setup();
    let store = fp_store::Store::open_or_create(&case.db).unwrap();
    let rows = fp_query::timeline(&store, &fp_query::TimelineFilter::default()).unwrap();
    let row = rows.first().expect("fixture contains events");
    let host = rows
        .iter()
        .find_map(|row| row.subject_host.clone())
        .expect("fixture contains hosts");

    for filter in [
        fp_query::TimelineFilter {
            kinds: vec![row.kind.clone()],
            kinds_not: vec![row.kind.clone()],
            ..Default::default()
        },
        fp_query::TimelineFilter {
            hosts: vec![host.clone()],
            hosts_not: vec![host],
            ..Default::default()
        },
        fp_query::TimelineFilter {
            sources: vec![row.source_kind.clone()],
            sources_not: vec![row.source_kind.clone()],
            ..Default::default()
        },
    ] {
        assert!(fp_query::timeline(&store, &filter).unwrap().is_empty());
    }

    store
        .conn()
        .execute(
            "UPDATE events SET ts_precision = 'order_only', confidence = 'derived',
             summary = 'unique synthetic filter test' WHERE id = ?1",
            [row.id],
        )
        .unwrap();
    let filter = fp_query::TimelineFilter {
        timing: fp_query::EventTiming::Untimed,
        confidence: vec![fp_core::Confidence::Derived],
        run_id: Some(row.run_id),
        match_text: Some("unique synthetic filter test".into()),
        limit: Some(1),
        reverse: true,
        ..Default::default()
    };
    let filtered = fp_query::timeline(&store, &filter).unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].id, row.id);
    assert_eq!(
        store
            .count_events(&fp_store::EventFilter::from(&filter))
            .unwrap(),
        1
    );
    assert!(
        fp_query::timeline(
            &store,
            &fp_query::TimelineFilter {
                run_id: Some(u64::MAX),
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn histogram_matches_scoped_fake_zoom_timeline_without_paging() {
    use fp_query::category::Category;
    use fp_query::overview::{TimeWindow, histogram};

    let case = setup();
    let store = fp_store::Store::open(&case.db).unwrap();
    let (start_utc, end_utc) = store.time_bounds().unwrap().unwrap();
    let window = TimeWindow { start_utc, end_utc };
    for mut filter in [
        fp_query::TimelineFilter::default(),
        fp_query::TimelineFilter {
            hosts: vec!["zoom.us".into()],
            sources_not: vec!["xdm".into()],
            ..Default::default()
        },
        fp_query::TimelineFilter {
            kinds_not: vec!["web.*".into()],
            run_id: Some(1),
            confidence: vec![fp_core::Confidence::Direct],
            ..Default::default()
        },
    ] {
        let rows = fp_query::timeline(&store, &filter).unwrap();
        filter.limit = Some(1);
        filter.offset = Some(50);
        filter.reverse = true;
        let result = histogram(&store, &filter, window, 200).unwrap();
        let mut expected = vec![[0u64; Category::ALL.len()]; result.buckets.len()];
        for row in rows.iter().filter(|row| row.ts_precision != "order_only") {
            let category = match row.kind.as_str() {
                "web.download" | "dl.manager_download" => Category::Downloads,
                "web.visit"
                | "web.redirect"
                | "web.permission_change"
                | "web.permission_used"
                | "web.search" => Category::Web,
                "pkg.install" | "pkg.upgrade" | "pkg.remove" => Category::Packages,
                "priv.sudo" | "priv.su" | "auth.login" | "auth.ssh" | "auth.session" => {
                    Category::Access
                }
                "shell.command" => Category::Shell,
                "file.opened" | "file.created" | "file.modified" => Category::Files,
                "persist.unit_added" | "persist.autostart" => Category::Persistence,
                "ext.installed" => Category::Extensions,
                "net.listen" | "proc.exec" | "other" => Category::Other,
                other => panic!("unexpected fixture kind: {other}"),
            };
            let index = result
                .buckets
                .iter()
                .position(|bucket| (bucket.start_utc..=bucket.end_utc).contains(&row.ts_utc))
                .expect("fixture timestamp in window");
            expected[index][category as usize] += 1;
        }
        assert_eq!(
            result.buckets.iter().map(|b| b.counts).collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn facets_match_timeline_groupings_on_fake_zoom() {
    use fp_query::overview::{Facet, facets};
    use std::collections::BTreeMap;

    let case = setup();
    let store = fp_store::Store::open(&case.db).unwrap();
    let scope = fp_query::TimelineFilter {
        hosts: vec!["zoom.us".into()],
        kinds: vec!["web.visit".into()],
        ..Default::default()
    };
    let rows = fp_query::timeline(&store, &fp_query::TimelineFilter::default()).unwrap();
    for (facet, key) in [
        (
            Facet::Kind,
            (|r: &fp_query::TimelineRow| Some(r.kind.clone())) as fn(&_) -> _,
        ),
        (Facet::Source, |r| Some(r.source_label())),
        (Facet::Host, |r| r.subject_host.clone()),
        (Facet::Confidence, |r| Some(r.confidence.clone())),
    ] {
        // the facet's own selection is dropped, the remaining selection must still apply
        let mut remaining = facet.without(&scope);
        remaining.limit = None;
        let expected_rows = fp_query::timeline(&store, &remaining).unwrap();
        let mut expected: BTreeMap<Option<String>, u64> = BTreeMap::new();
        for row in &expected_rows {
            *expected.entry(key(row)).or_default() += 1;
        }
        let page = facets(&store, &scope, facet, 500).unwrap();
        let actual: BTreeMap<_, _> = page
            .values
            .iter()
            .map(|v| (v.key.clone(), v.count))
            .collect();
        assert_eq!(actual, expected, "{facet:?}");
        assert_eq!(page.distinct_values as usize, expected.len());
    }
    assert!(!rows.is_empty());
}

#[test]
fn detail_resolves_links() {
    let case = setup();
    let store = fp_store::Store::open(&case.db).unwrap();
    let rows = fp_query::timeline(
        &store,
        &fp_query::TimelineFilter {
            kinds: vec!["dl.manager_download".into()],
            ..Default::default()
        },
    )
    .unwrap();
    let detail = fp_query::event_detail(&store, rows[0].id).unwrap().unwrap();
    assert_eq!(detail.kind, "dl.manager_download");
    assert!(detail.prov_sha256.is_some());
    let link = detail
        .links
        .iter()
        .find(|l| l.rel == "downloaded_as")
        .expect("download links to its artifact");
    assert!(link.target_label.ends_with("zoom_amd64.deb"));
}

#[test]
fn trace_tree_matches_scenario() {
    let case = setup();
    let store = fp_store::Store::open(&case.db).unwrap();
    let deb = case
        ._dir
        .path()
        .join("home/u/Downloads/Programs/zoom_amd64.deb");
    let tree = fp_query::trace_tree(&store, deb.to_str().unwrap()).unwrap();
    let fp_correlate::TraceHead::Artifact { path, .. } = &tree.head else {
        panic!("expected an artifact head");
    };
    assert!(path.ends_with("zoom_amd64.deb"));
    let rels: Vec<&str> = tree.rows.iter().map(|r| r.rel.as_str()).collect();
    assert!(rels.contains(&"downloaded_as"));
    assert!(rels.contains(&"installed_from"));
    assert!(rels.contains(&"pkg.install"));
    // every row is backed by an event the UI can jump to, except notes
    for row in &tree.rows {
        if row.arrow != fp_correlate::TraceArrow::Note {
            assert!(row.event_id.is_some(), "{row:?}");
        }
    }
}

#[test]
fn state_and_sources_and_coverage() {
    let case = setup();
    let store = fp_store::Store::open(&case.db).unwrap();

    let perms = fp_query::state_records(&store, None, Some("web.site_permission"), None).unwrap();
    assert_eq!(perms.len(), 3, "camera, mic, notifications");
    assert!(perms.iter().all(|p| p.parsed.is_some()));

    let sources = fp_query::sources(&store, None).unwrap();
    assert!(sources.iter().any(|s| s.kind == "chromium.history"));
    let cov = fp_query::coverage(&sources);
    let browser = cov
        .iter()
        .find(|c| c.category == "Browser history")
        .unwrap();
    assert_eq!(browser.status, fp_query::CoverageStatus::Covered);
}

#[test]
fn diff_against_itself_is_empty() {
    let case = setup();
    let store = fp_store::Store::open(&case.db).unwrap();
    let entries = fp_query::diff_runs(&store, 1, 1).unwrap();
    assert!(entries.is_empty(), "a run equals itself");
}

#[test]
fn time_formatting_utc_is_zone_independent() {
    let ts = local(21, 6, 20) * 1_000_000;
    let utc = fp_query::format_time(ts, "second", fp_query::Tz::Utc, "%Y-%m-%d %H:%M");
    assert_eq!(utc, "2026-10-16 17:36");
    assert_eq!(
        fp_query::format_time(0, "order_only", fp_query::Tz::Utc, "%H:%M"),
        "(untimed)"
    );
}

#[test]
fn group_counts_reproduce_the_old_cli_grouping_on_fake_zoom() {
    use fp_query::overview::{GroupBy, group};
    use std::collections::HashMap;

    let case = setup();
    let store = fp_store::Store::open(&case.db).unwrap();
    let tz = fp_query::Tz::Utc;

    // the exact algorithm the CLI used before G1.5: fold timeline rows into string keys,
    // then sort by count desc, key asc, then truncate to the limit.
    let old = |by: GroupBy, filter: &fp_query::TimelineFilter, limit: Option<usize>| {
        let rows = fp_query::timeline(&store, filter).unwrap();
        let mut counts: HashMap<String, u64> = HashMap::new();
        for r in &rows {
            let key = match by {
                GroupBy::Dir => {
                    let subj = r.subject.as_deref().unwrap_or("");
                    if subj.contains('/') {
                        std::path::Path::new(subj)
                            .parent()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "/".into())
                    } else {
                        "-".into()
                    }
                }
                GroupBy::Host => r.subject_host.clone().unwrap_or_else(|| "-".into()),
                GroupBy::Source => r.source_label(),
                GroupBy::Kind => r.kind.clone(),
                GroupBy::Minute => {
                    fp_query::format_time(r.ts_utc, &r.ts_precision, tz, "%Y-%m-%d %H:%M")
                }
            };
            *counts.entry(key).or_default() += 1;
        }
        let mut sorted: Vec<(String, u64)> = counts.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        if let Some(limit) = limit {
            sorted.truncate(limit);
        }
        sorted
    };

    let scopes = [
        fp_query::TimelineFilter::default(),
        fp_query::TimelineFilter {
            hosts: vec!["zoom.us".into()],
            ..Default::default()
        },
        fp_query::TimelineFilter {
            kinds: vec!["web.*".into(), "dl.*".into()],
            ..Default::default()
        },
    ];
    for by in [
        GroupBy::Dir,
        GroupBy::Host,
        GroupBy::Source,
        GroupBy::Kind,
        GroupBy::Minute,
    ] {
        for scope in &scopes {
            for limit in [None, Some(2)] {
                let expected = old(by, scope, limit);
                let actual: Vec<(String, u64)> = group(&store, scope, by, limit, tz)
                    .unwrap()
                    .into_iter()
                    .map(|g| (g.key, g.count))
                    .collect();
                assert_eq!(actual, expected, "{by:?} {scope:?} limit={limit:?}");
            }
        }
    }
}

#[test]
fn host_summaries_rank_the_lure_domains_on_fake_zoom() {
    use fp_query::overview::{HostSort, hosts};
    use std::collections::HashMap;

    let case = setup();
    let store = fp_store::Store::open(&case.db).unwrap();
    let scope = fp_query::TimelineFilter::default();

    let summaries = hosts(&store, &scope, HostSort::EventsDesc, 500).unwrap();
    let by_host: HashMap<&str, &fp_query::overview::HostSummary> =
        summaries.iter().map(|s| (s.host.as_str(), s)).collect();

    // the fake page is the busiest host in the scenario
    let lure = by_host["us09-web-zoom.com"];
    assert_eq!(lure.events, 4);
    assert!(lure.first_in_case.is_some());
    // every host in a single-day case is first-seen that day, so a day-window flags them NEW
    let (lo, hi) = store.time_bounds().unwrap().unwrap();
    assert!(
        summaries
            .iter()
            .all(|s| s.first_in_case.is_none_or(|t| (lo..=hi).contains(&t)))
    );

    // event counts agree with grouping the unpaged timeline by host (hosts without a value drop)
    let rows = fp_query::timeline(&store, &scope).unwrap();
    let mut expected: HashMap<String, u64> = HashMap::new();
    for row in &rows {
        if let Some(host) = &row.subject_host {
            *expected.entry(host.clone()).or_default() += 1;
        }
    }
    let actual: HashMap<String, u64> = summaries
        .iter()
        .map(|s| (s.host.clone(), s.events))
        .collect();
    assert_eq!(actual, expected);

    // ranking is non-increasing by events
    assert!(summaries.windows(2).all(|w| w[0].events >= w[1].events));

    // scoping to the lure host keeps only it
    let scoped = hosts(
        &store,
        &fp_query::TimelineFilter {
            hosts: vec!["us09-web-zoom.com".into()],
            ..Default::default()
        },
        HostSort::EventsDesc,
        500,
    )
    .unwrap();
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].host, "us09-web-zoom.com");
}
