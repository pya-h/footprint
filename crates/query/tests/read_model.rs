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
