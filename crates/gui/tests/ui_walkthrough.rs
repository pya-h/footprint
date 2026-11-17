//! Scripted end-to-end walkthrough of the fake-Zoom scenario through the GUI
//! (`Dashboard -> Scope -> Host facet -> Inspector -> Trace -> State -> IOC -> Export/CLI`).

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use fp_fixtures::{SCENARIO_TZ, fake_zoom, local};
use fp_gui::app::Shell;
use fp_gui::model::{Action, ScopeAction, Selection, View};
use fp_gui::settings::Settings;
use fp_query::category::Category;
use fp_query::overview::Facet;

static COLLECT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn setup_fake_zoom() -> (tempfile::TempDir, std::path::PathBuf) {
    let _guard = COLLECT_LOCK.lock().expect("collect lock");
    let dir = tempfile::tempdir().expect("tempdir");
    fake_zoom(dir.path()).expect("fake_zoom");
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
    .expect("collect fake_zoom");
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);
    (dir, db)
}

fn new_shell(case_path: &Path, view: View) -> Shell {
    let ctx = egui::Context::default();
    let tz = fp_query::parse_tz(SCENARIO_TZ).expect("scenario tz");
    // reduced motion makes entrance/hover animations instant, so `Harness::run` settles
    // within the default step budget instead of racing the fade timers
    let settings = Settings {
        reduced_motion: true,
        ..Settings::default()
    };
    let mut shell = Shell::new(case_path.to_path_buf(), tz, settings, None, &ctx);
    shell.apply(Action::Go(view));
    shell
}

fn pump_until(harness: &mut Harness<'_, Shell>, pred: impl Fn(&Shell) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        harness.step();
        if pred(harness.state()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    panic!("timed out waiting for worker condition in harness");
}

#[test]
fn fake_zoom_end_to_end_walkthrough() {
    let (dir, db) = setup_fake_zoom();

    let shell = new_shell(&db, View::Dashboard);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .with_step_dt(1.0 / 60.0)
        .allow_missing_glyphs()
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        );

    // 1. Dashboard loads hosts, NEW badges, category breakdown, and coverage
    pump_until(&mut harness, |s| {
        s.model().counts.value().is_some()
            && s.model().dashboard_hosts.value().is_some()
            && s.model().dashboard_summary.value().is_some()
    });
    harness.step();
    harness.step();
    let total_in_case = harness
        .state()
        .model()
        .counts
        .value()
        .expect("counts")
        .in_case;
    assert!(total_in_case > 0);
    assert!(
        harness
            .query_all_by_label_contains("us09-web-zoom.com")
            .next()
            .is_some(),
        "expected us09-web-zoom.com on dashboard"
    );

    // 2. Apply the incident window (20:40 -> 21:30 scenario local time) on Dashboard to verify [NEW], then inspect Timeline counts
    let win_start = local(20, 40, 0) * 1_000_000;
    let win_end = local(21, 30, 0) * 1_000_000;
    harness
        .state_mut()
        .apply(Action::Scope(ScopeAction::SetWindow {
            since: Some(win_start),
            until: Some(win_end),
        }));
    pump_until(&mut harness, |s| {
        s.model().counts.value().is_some()
            && s.model().dashboard_hosts.value().is_some_and(|hosts| {
                hosts
                    .iter()
                    .any(|h| h.is_new_in_window(Some((win_start, win_end))))
            })
    });
    harness.step();
    harness.step();
    assert!(
        harness.query_all_by_label_contains("NEW").next().is_some(),
        "expected NEW badge for hosts first seen in incident window"
    );

    harness.state_mut().apply(Action::Go(View::Timeline));
    pump_until(&mut harness, |s| {
        s.model().counts.value().is_some() && s.model().category.value().is_some()
    });
    harness.step();
    harness.step();
    let windowed_count = harness
        .state()
        .model()
        .counts
        .value()
        .expect("window counts")
        .in_scope;
    assert!(
        windowed_count > 0 && windowed_count < total_in_case,
        "window {windowed_count} should narrow case total {total_in_case}"
    );
    let cats = harness
        .state()
        .model()
        .category
        .value()
        .expect("categories");
    assert!(cats[Category::Web as usize] > 0);
    assert_eq!(cats[Category::Downloads as usize], 2);
    assert!(cats[Category::Packages as usize] > 0);
    assert!(cats[Category::Shell as usize] > 0);

    // 3. Expand the HOST facet, filter to `us09-web-zoom.com`, and verify narrowed timeline rows
    harness
        .query_all_by_label("HOST")
        .next()
        .expect("HOST collapsing header")
        .click();
    pump_until(&mut harness, |s| {
        s.model()
            .facets
            .get(&Facet::Host)
            .and_then(|slot| slot.value())
            .is_some()
    });
    harness.step();
    harness
        .query_all_by_label_contains("us09-web-zoom.com")
        .next()
        .expect("host facet us09-web-zoom.com")
        .click();
    pump_until(&mut harness, |s| {
        s.model().scope.filter().hosts == vec!["us09-web-zoom.com".to_string()]
            && s.model().counts.value().is_some()
    });
    harness.step();
    harness.step();

    // 4. Pick the `web.redirect` event and inspect confidence, detail, links, and around rows
    harness
        .query_all_by_label_contains("web.redirect")
        .next()
        .expect("web.redirect row")
        .click();
    pump_until(&mut harness, |s| {
        matches!(s.model().selection(), Selection::Event(_))
            && s.model().detail.value().is_some_and(Option::is_some)
            && s.model().around.value().is_some()
    });
    harness.step();
    harness.step();
    let redirect_id = match harness.state().model().selection() {
        Selection::Event(id) => id,
        other => panic!("expected event selection, got {other:?}"),
    };
    let detail = harness
        .state()
        .model()
        .detail
        .value()
        .and_then(|d| d.as_deref())
        .expect("redirect detail");
    assert_eq!(detail.id, redirect_id);
    assert_eq!(detail.kind.as_str(), "web.redirect");
    harness.get_by_label("Trace");

    // 5. Open Trace from the selected redirect and check the chain lands
    harness.get_by_label("Trace").scroll_to_me();
    harness.run();
    harness.get_by_label("Trace").click();
    harness.run();
    assert_eq!(
        harness.state().model().view(),
        View::Trace,
        "clicking inspector Trace did not navigate (pending_trace={:?})",
        harness.state().model().pending_trace
    );
    pump_until(&mut harness, |s| {
        s.model().view() == View::Trace && s.model().trace.value().is_some()
    });
    harness.step();
    harness.step();
    let trace_vm = harness.state().model().trace.value().expect("trace vm");
    assert!(!trace_vm.rows.is_empty(), "trace should link redirect hops");

    // 6. State view shows `web.site_permission` rows for `us09-web-zoom.com`
    harness.state_mut().apply(Action::Go(View::State));
    pump_until(&mut harness, |s| s.model().states.value().is_some());
    harness.step();
    harness.step();
    harness.get_by_label_contains("web.site_permission");

    // 7. IOC view adds `us09-web-zoom.com` through the writer thread and links its hits
    harness.state_mut().apply(Action::Go(View::Ioc));
    pump_until(&mut harness, |s| s.model().iocs.value().is_some());
    harness.state_mut().apply(Action::AddIoc {
        ioc_type: "domain".to_string(),
        value: "us09-web-zoom.com".to_string(),
        note: None,
    });
    pump_until(&mut harness, |s| {
        s.model().iocs.value().is_some_and(|rows| {
            rows.iter()
                .any(|r| r.value.as_str() == "us09-web-zoom.com" && r.hits > 0)
        })
    });
    let zoom_hits = harness
        .state()
        .model()
        .iocs
        .value()
        .expect("iocs")
        .iter()
        .find(|r| r.value.as_str() == "us09-web-zoom.com")
        .expect("added domain")
        .hits;
    assert!(zoom_hits > 0);

    // Pivot to IOC-only timeline and verify the CLI scope string & redacted HTML export
    harness
        .state_mut()
        .apply(Action::Scope(ScopeAction::SetIocOnly(true)));
    harness.state_mut().apply(Action::Go(View::Timeline));
    pump_until(&mut harness, |s| s.model().counts.value().is_some());
    let cli = harness.state().model().scope.to_cli();
    assert!(cli.contains("--ioc-only"));
    assert!(cli.contains("--host us09-web-zoom.com"));
    assert!(cli.contains("--since"));
    assert!(cli.contains("--until"));

    // 8. Export the scoped investigation to redacted HTML
    let html_out = dir.path().join("walkthrough.html");
    harness.state_mut().apply(Action::ExportScope {
        path: html_out.clone(),
        format: "html".to_string(),
        redact: true,
    });
    pump_until(&mut harness, |_| html_out.exists());
    let html = std::fs::read_to_string(&html_out).expect("read exported html");
    assert!(html.contains("us09-web-zoom.com"));
}
