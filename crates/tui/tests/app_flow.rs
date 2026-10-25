use fp_fixtures::{SCENARIO_TZ, fake_zoom, local};
use fp_tui::{App, Key, View};

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

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.on_key(Key::Char(c)).unwrap();
    }
}

#[test]
fn timeline_navigation_and_paging() {
    let case = setup();
    let mut app = App::new(&case.db).unwrap();
    assert_eq!(app.view, View::Timeline);
    assert!(!app.rows.is_empty());

    app.on_key(Key::Down).unwrap();
    assert_eq!(app.selected, 1);
    app.on_key(Key::Up).unwrap();
    assert_eq!(app.selected, 0);
    app.on_key(Key::PageDown).unwrap();
    assert!(app.selected > 0);
    app.on_key(Key::PageUp).unwrap();
    assert_eq!(app.selected, 0);
}

#[test]
fn filter_bar_narrows_the_timeline() {
    let case = setup();
    let mut app = App::new(&case.db).unwrap();
    let all = app.rows.len();

    app.on_key(Key::Char('i')).unwrap();
    assert!(app.editing);
    // kinds is the third field: Tab twice from `since`
    app.on_key(Key::Tab).unwrap();
    app.on_key(Key::Tab).unwrap();
    type_text(&mut app, "dl.*");
    app.on_key(Key::Enter).unwrap();
    assert!(!app.editing);
    assert_eq!(app.rows.len(), 1, "only the XDM row matches dl.*");
    assert!(app.rows.len() < all);
}

#[test]
fn enter_opens_detail_and_links_jump() {
    let case = setup();
    let mut app = App::new(&case.db).unwrap();

    // find the XDM row and open its detail
    let idx = app
        .rows
        .iter()
        .position(|r| r.kind == "dl.manager_download")
        .unwrap();
    app.selected = idx;
    app.on_key(Key::Enter).unwrap();
    assert_eq!(app.view, View::Detail);
    let detail = app.detail.as_ref().unwrap();
    let link_idx = detail
        .links
        .iter()
        .position(|l| l.rel == "downloaded_as")
        .unwrap();
    assert_eq!(detail.links[link_idx].target_type, "artifact");

    // follow event links until the chain ends
    let mut jumps = 0;
    for _ in 0..8 {
        let Some(detail) = &app.detail else { break };
        if let Some(pos) = detail.links.iter().position(|l| l.target_type == "event") {
            app.detail_link = pos;
            app.on_key(Key::Enter).unwrap();
            if app.view != View::Detail {
                break;
            }
            jumps += 1;
        } else {
            break;
        }
    }
    assert!(jumps > 0, "at least one event link should be followable");
}

#[test]
fn trace_view_opens_from_timeline() {
    let case = setup();
    let mut app = App::new(&case.db).unwrap();
    let idx = app
        .rows
        .iter()
        .position(|r| r.kind == "dl.manager_download")
        .unwrap();
    app.selected = idx;
    app.on_key(Key::Char('r')).unwrap();
    assert_eq!(app.view, View::Trace);
    let trace = app.trace.as_ref().unwrap();
    assert!(
        trace
            .rows
            .iter()
            .any(|r| r.confidence.as_deref() == Some("heuristic"))
    );
}

#[test]
fn state_diff_ioc_and_sources_views() {
    let case = setup();
    let mut app = App::new(&case.db).unwrap();

    // state view with a kind filter
    app.view = View::State;
    app.on_key(Key::Char('k')).unwrap();
    type_text(&mut app, "web.site_permission");
    app.on_key(Key::Enter).unwrap();
    assert_eq!(app.state_rows.len(), 3);

    // diff prev/last with a single run diffs the run against itself: empty
    app.view = View::Diff;
    app.on_key(Key::Enter).unwrap();
    assert!(app.diff_entries.is_empty());

    // ioc view: no IOCs configured yet
    app.view = View::Ioc;
    app.reload_ioc().unwrap();
    assert!(app.ioc_hits.is_empty());

    // sources + coverage
    assert_eq!(
        app.coverage
            .iter()
            .find(|c| c.category == "Browser history")
            .unwrap()
            .status,
        fp_query::CoverageStatus::Covered
    );
}

#[test]
fn collect_runs_and_refreshes() {
    let case = setup();
    let mut app = App::new(&case.db).unwrap();
    let before = app.rows.len();

    app.view = View::Collect;
    // narrow the in-TUI collect to one cheap source so the test stays hermetic
    app.collect.sources = "auth.wtmp".into();
    app.on_key(Key::Enter).unwrap();
    assert!(app.collect.running);
    for _ in 0..200 {
        app.poll_collect().unwrap();
        if !app.collect.running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!app.collect.running, "collect should finish");
    let text = app.collect.summary.as_ref().unwrap();
    assert!(text.contains("0 new events"), "{text}");
    assert_eq!(app.rows.len(), before, "idempotent re-collect adds nothing");
}

#[test]
fn a_collect_that_cannot_start_reports_and_unblocks() {
    let case = setup();
    let mut app = App::new(&case.db).unwrap();
    app.view = View::Collect;
    app.collect.since = "not a time".into();
    app.on_key(Key::Enter).unwrap();
    for _ in 0..200 {
        app.poll_collect().unwrap();
        if !app.collect.running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        !app.collect.running,
        "a failed collect must not stay running"
    );
    let text = app.collect.summary.as_ref().unwrap();
    assert!(text.contains("collect failed"), "{text}");
}

#[test]
fn help_overlay_and_quit() {
    let case = setup();
    let mut app = App::new(&case.db).unwrap();
    app.on_key(Key::Char('?')).unwrap();
    assert!(app.help);
    app.on_key(Key::Esc).unwrap();
    assert!(!app.help);
    assert!(app.on_key(Key::Char('q')).unwrap(), "q should quit");
}
