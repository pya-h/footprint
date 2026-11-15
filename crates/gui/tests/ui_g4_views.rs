//! UI + worker integration tests for G4 views (Dashboard, Hosts, Lanes) via `egui_kittest`.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use fp_gui::app::Shell;
use fp_gui::model::{Action, ScopeAction, Selection, View};
use fp_gui::settings::Settings;
use fp_query::Tz;
use fp_query::overview::{HostSort, LaneMode};
use fp_store::{EventTiming, Store};

const BASE_TS: i64 = 1_760_647_000_000_000;

fn build_g4_case(path: &Path) -> u64 {
    let mut store = Store::open_or_create(path).expect("create case db");
    let run1 = store
        .insert_run(&fp_store::NewRun {
            host: "ws01",
            user: "analyst",
            uid: 1000,
            euid: 1000,
            version: "0.1.0",
            tz: "UTC",
            args: "collect --run 1",
        })
        .expect("insert run 1");

    let run2 = store
        .insert_run(&fp_store::NewRun {
            host: "ws01",
            user: "analyst",
            uid: 1000,
            euid: 1000,
            version: "0.1.0",
            tz: "UTC",
            args: "collect --run 2",
        })
        .expect("insert run 2");

    let conn = store.conn();
    conn.execute(
        "INSERT INTO sources (run_id, kind, app, profile, status)
         VALUES (?1, 'chromium', 'brave', 'Default', '\"present\"'),
                (?1, 'shell.history', 'bash', NULL, '{\"unreadable\":{\"needs_root\":true}}'),
                (?2, 'sshd', NULL, NULL, '\"partial\"')",
        rusqlite::params![run1, run2],
    )
    .expect("insert sources");

    conn.execute(
        "INSERT INTO state (run_id, kind, key, value_json)
         VALUES (?1, 'ufw.status', 'firewall', '{\"active\":false,\"flags\":[\"firewall_disabled\"]}'),
                (?2, 'ufw.status', 'firewall', '{\"active\":true}')",
        rusqlite::params![run1, run2],
    )
    .expect("insert state");

    conn.execute(
        "INSERT INTO events
            (run_id, source_id, ts_utc, ts_precision, kind, subject, subject_host, summary, confidence, dedup_key)
         VALUES
            (?1, 1, ?3, 'exact', 'web.visit', 'https://old-host.example/login', 'old-host.example',
             'early login visit', 'direct', 'g4_k1'),
            (?1, 2, 0, 'order_only', 'shell.command', 'cat /etc/passwd', NULL,
             'cat /etc/passwd', 'heuristic', 'g4_k2'),
            (?2, 1, ?4, 'exact', 'web.visit', 'https://old-host.example/ping', 'old-host.example',
             'late ping on old host', 'direct', 'g4_k3'),
            (?2, 1, ?5, 'exact', 'web.visit', 'https://us09-web-zoom.com/j/42', 'us09-web-zoom.com',
             'visited zoom lookalike', 'direct', 'g4_k4'),
            (?2, 1, ?6, 'exact', 'web.download', 'https://cdn.zoom.us/zoom.deb', 'cdn.zoom.us',
             'downloaded zoom.deb', 'derived', 'g4_k5'),
            (?2, 3, ?7, 'exact', 'web.visit', 'https://evil-drop.org/payload', 'evil-drop.org',
             'fetched payload from evil-drop.org', 'direct', 'g4_k6')",
        rusqlite::params![
            run1,
            run2,
            BASE_TS,
            BASE_TS + 520_000_000,
            BASE_TS + 550_000_000,
            BASE_TS + 600_000_000,
            BASE_TS + 650_000_000,
        ],
    )
    .expect("insert events");

    let zoom_ev_id: u64 = conn
        .query_row("SELECT id FROM events WHERE dedup_key = 'g4_k4'", [], |r| {
            r.get(0)
        })
        .expect("zoom event id");

    conn.execute(
        "INSERT INTO iocs (type, value, note, added_utc)
         VALUES ('domain', 'us09-web-zoom.com', 'zoom-lookalike', 1760647000)",
        [],
    )
    .expect("insert ioc");
    let ioc_id: i64 = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO links (src_type, src_id, dst_type, dst_id, rel, confidence, why)
         VALUES ('event', ?1, 'ioc', ?2, 'ioc_match', 'direct', 'domain match')",
        rusqlite::params![zoom_ev_id, ioc_id],
    )
    .expect("insert ioc link");

    zoom_ev_id
}

fn new_shell(case_path: &Path, initial_view: View) -> Shell {
    let ctx = egui::Context::default();
    let settings = Settings {
        last_view: Some(initial_view.label().to_lowercase()),
        ..Settings::default()
    };
    let mut shell = Shell::new(case_path.to_path_buf(), Tz::Utc, settings, None, &ctx);
    shell.apply(Action::Go(initial_view));
    shell
}

fn build_harness(shell: Shell) -> Harness<'static, Shell> {
    Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        )
}

fn pump_until(harness: &mut Harness<'_, Shell>, pred: impl Fn(&Shell) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        harness.step();
        if pred(harness.state()) {
            return;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    panic!("timed out waiting for worker condition in harness");
}

fn click_at(harness: &mut Harness<'_, Shell>, pos: egui::Pos2) {
    harness.event(egui::Event::PointerMoved(pos));
    harness.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    harness.event(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    harness.step();
}

#[test]
fn dashboard_kpis_category_bars_hosts_and_worth_a_look() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_g4_case(&db);

    let mut harness = build_harness(new_shell(&db, View::Dashboard));
    pump_until(&mut harness, |s| {
        s.model().counts.value().is_some()
            && s.model().category.value().is_some()
            && s.model().histogram.value().is_some()
            && s.model().dashboard_summary.value().is_some()
            && s.model().dashboard_hosts.value().is_some()
    });

    harness.get_by_label_contains("analyst@ws01");
    harness.get_by_label_contains("2 runs");
    harness.get_by_label("EVENTS");
    harness.get_by_label("WORTH A LOOK");
    harness.get_by_label_contains("1 IOC-matched events");
    harness.get_by_label_contains("1 flagged state records");
    harness.get_by_label_contains("bash: unreadable (needs root)");
    harness.get_by_label_contains("sshd: partial");
    harness.get_by_label_contains("Run #1 -> #2:");

    // Category bar click filters Timeline to Downloads.
    harness.get_by_label("Downloads").click();
    harness.step();
    assert_eq!(harness.state().model().view(), View::Timeline);
    assert_eq!(
        harness.state().model().scope.filter().kinds,
        vec!["web.download".to_string(), "dl.*".to_string()]
    );

    // Return to Dashboard and click Top Host link.
    harness.state_mut().apply(Action::Scope(ScopeAction::Clear));
    harness.state_mut().apply(Action::Go(View::Dashboard));
    pump_until(&mut harness, |s| {
        s.model().view() == View::Dashboard && s.model().dashboard_hosts.value().is_some()
    });
    harness.get_by_label("us09-web-zoom.com").click();
    harness.step();
    assert_eq!(harness.state().model().view(), View::Timeline);
    assert_eq!(
        harness.state().model().scope.filter().hosts,
        vec!["us09-web-zoom.com".to_string()]
    );

    // Worth-a-look IOC item -> Timeline with ioc_only=true.
    harness.state_mut().apply(Action::Scope(ScopeAction::Clear));
    harness.state_mut().apply(Action::Go(View::Dashboard));
    pump_until(&mut harness, |s| {
        s.model().dashboard_summary.value().is_some()
    });
    harness
        .get_by_label_contains("1 IOC-matched events")
        .click();
    harness.step();
    assert_eq!(harness.state().model().view(), View::Timeline);
    assert!(harness.state().model().scope.filter().ioc_only);

    // Worth-a-look Run diff item -> Diff view.
    harness.state_mut().apply(Action::Scope(ScopeAction::Clear));
    harness.state_mut().apply(Action::Go(View::Dashboard));
    pump_until(&mut harness, |s| {
        s.model().dashboard_summary.value().is_some()
    });
    harness.get_by_label_contains("Run #1 -> #2:").click();
    harness.step();
    assert_eq!(harness.state().model().view(), View::Diff);
}

#[test]
fn hosts_view_search_sort_new_badge_drilldown_and_add_ioc() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_g4_case(&db);

    let mut harness = build_harness(new_shell(&db, View::Hosts));
    // Scope to the late window so old-host.example has first_in_case BEFORE window (no [NEW])
    // while us09-web-zoom.com / cdn.zoom.us / evil-drop.org have first_in_case INSIDE window ([NEW]).
    harness
        .state_mut()
        .apply(Action::Scope(ScopeAction::SetWindow {
            since: Some(BASE_TS + 500_000_000),
            until: Some(BASE_TS + 700_000_000),
        }));
    pump_until(&mut harness, |s| {
        s.model().hosts.value().is_some() && s.model().bounds.value().copied().flatten().is_some()
    });

    assert_eq!(harness.get_all_by_label("[NEW]").count(), 3);
    harness.get_by_label("[IOC]");
    let rows = harness.state().model().hosts.value().expect("hosts loaded");
    assert_eq!(rows.len(), 4);
    let win = (BASE_TS + 500_000_000, BASE_TS + 700_000_000);
    let old_row = rows
        .iter()
        .find(|r| r.host_key() == "old-host.example")
        .expect("old-host present");
    assert!(!old_row.is_new_in_window(Some(win)));
    let evil_row = rows
        .iter()
        .find(|r| r.host_key() == "evil-drop.org")
        .expect("evil-drop present");
    assert!(evil_row.is_new_in_window(Some(win)));
    assert!(!evil_row.has_ioc);

    // Search substring filters in SQL.
    harness.get_by_label("Host").click();
    pump_until(&mut harness, |s| {
        s.model().host_sort == HostSort::HostAsc
            && s.model()
                .hosts
                .value()
                .is_some_and(|r| r.first().is_some_and(|f| f.host_key() == "cdn.zoom.us"))
    });

    let inputs: Vec<_> = harness
        .get_all_by_role(egui::accesskit::Role::TextInput)
        .collect();
    let search = inputs.last().expect("hosts search input");
    search.focus();
    search.type_text("evil");
    harness.step();
    pump_until(&mut harness, |s| {
        s.model().hosts.generation() == s.model().hosts_gen
            && s.model()
                .hosts
                .value()
                .is_some_and(|r| r.len() == 1 && r[0].host_key() == "evil-drop.org")
    });
    assert_eq!(
        harness.state().model().hosts.value().expect("filtered")[0].host_key(),
        "evil-drop.org"
    );

    // Context-menu Add as IOC persists to DB and updates host IOC badge.
    let link = harness.get_by_label("evil-drop.org");
    link.click_secondary();
    harness.run_steps(2);
    harness.get_by_label("Add as IOC").click();
    pump_until(&mut harness, |s| {
        s.model().ioc_status.is_some()
            && s.model().hosts.value().is_some_and(|r| {
                r.iter()
                    .any(|h| h.host_key() == "evil-drop.org" && h.has_ioc)
            })
    });

    // Let egui's 0.5s multi-click window reset after closing the context menu, then double-click the row.
    harness.run_steps(35);
    harness.get_by_label("[NEW]").click();
    harness.step();
    harness.get_by_label("[NEW]").click();
    harness.step();
    assert_eq!(harness.state().model().view(), View::Timeline);
    assert_eq!(
        harness.state().model().scope.filter().hosts,
        vec!["evil-drop.org".to_string()]
    );
}

#[test]
fn lanes_view_grouping_cursor_selection_untimed_lane_and_5000_event_density_cap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_g4_case(&db);

    let mut harness = build_harness(new_shell(&db, View::Lanes));
    pump_until(&mut harness, |s| s.model().lanes.value().is_some());

    {
        let data = harness.state().model().lanes.value().expect("lanes loaded");
        assert!(!data.truncated_to_density);
        assert_eq!(data.untimed_total, 1);
    }
    harness.get_by_label_contains("Untimed lane (order_only): 1 events");
    let lane_rect = harness.get_by_label("brave/Default").rect();

    // Hovering a track paints the cursor readout; clicking the first event at track_rect.left() selects it.
    let track_left_click = egui::pos2(lane_rect.left() + 178.0, lane_rect.center().y);
    click_at(&mut harness, track_left_click);
    harness.get_by_label_contains("Cursor: 2025-");
    assert!(matches!(
        harness.state().model().inspected(),
        Selection::Event(_)
    ));

    // Toggle Source -> Category.
    harness.get_by_label("Category").click();
    pump_until(&mut harness, |s| {
        s.model().lanes_mode == LaneMode::Category
            && s.model()
                .lanes
                .value()
                .is_some_and(|d| d.mode == LaneMode::Category)
    });
    harness.get_by_label("Downloads");

    // Untimed lane "[show as list]" drops window, switches to Untimed timing, and opens Timeline.
    harness.get_by_label("show as list").click();
    harness.step();
    assert_eq!(harness.state().model().view(), View::Timeline);
    assert_eq!(
        harness.state().model().scope.filter().timing,
        EventTiming::Untimed
    );

    // Add 5,005 timed events directly in the DB to verify > 5,000 density fallback banner.
    {
        let mut store = Store::open_or_create(&db).expect("open db");
        let tx = store.conn_mut().transaction().expect("tx");
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO events (run_id, source_id, ts_utc, ts_precision, kind, subject, summary, confidence, dedup_key)
                     VALUES (1, 1, ?1, 'exact', 'web.visit', 'https://bulk.example/', 'bulk event', 'direct', ?2)",
                )
                .expect("prep");
            for idx in 0..5_005i64 {
                let ts = BASE_TS + idx * 1_000;
                let key = format!("bulk_{idx}");
                stmt.execute(rusqlite::params![ts, key]).expect("ins");
            }
        }
        tx.commit().expect("commit");
    }

    let mut bulk_harness = build_harness(new_shell(&db, View::Lanes));
    pump_until(&mut bulk_harness, |s| s.model().lanes.value().is_some());
    let data = bulk_harness
        .state()
        .model()
        .lanes
        .value()
        .expect("bulk lanes");
    assert!(data.truncated_to_density);
    assert!(data.total_timed > 5_000);
    bulk_harness.get_by_label_contains("zoom in (<= 5,000 events) to see individual events");
}
