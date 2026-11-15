//! UI + worker integration tests for the G3 exploration loop via `egui_kittest`.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use fp_core::Confidence;
use fp_gui::app::Shell;
use fp_gui::model::{Action, ScopeAction, Selection, View};
use fp_gui::settings::Settings;
use fp_query::Tz;
use fp_store::Store;

fn build_fake_zoom_case(path: &Path) -> u64 {
    let mut store = Store::open_or_create(path).expect("create case db");
    let run_id = store
        .insert_run(&fp_store::NewRun {
            host: "workstation",
            user: "paya",
            uid: 1000,
            euid: 1000,
            version: "0.1.0",
            tz: "UTC",
            args: "collect --since -1d",
        })
        .expect("insert run");

    let conn = store.conn();
    conn.execute(
        "INSERT INTO sources (run_id, kind, app, profile, status)
         VALUES (?1, 'chromium', 'brave', 'Default', 'ok'),
                (?1, 'shell.history', 'bash', NULL, 'ok'),
                (?1, 'pkg.dpkg', 'dpkg', NULL, 'ok')",
        [run_id],
    )
    .expect("insert sources");

    let base_ts: i64 = 1_760_647_493_000_000;
    conn.execute(
        "INSERT INTO events
            (run_id, source_id, ts_utc, ts_precision, ts_raw, ts_raw_format, ts_tz_assumed,
             kind, subject, subject_host, summary, confidence, attrs_json, prov_file,
             prov_sha256, locator, dedup_key)
         VALUES
            (?1, 1, ?2, 'exact', '13436646517', 'WebKitMicros', 'UTC',
             'web.redirect', 'https://page-redirect.com/abc', 'page-redirect.com',
             'page-redirect.com -> us09-web-zoom.com', 'direct',
             '{\"status\":302,\"tab\":4}', '/home/paya/.config/Brave/History',
             'deadbeef', 'urls:42', 'k1'),
            (?1, 1, ?3, 'exact', '13436646520', 'WebKitMicros', 'UTC',
             'web.visit', 'https://us09-web-zoom.com/j/4139766', 'us09-web-zoom.com',
             'visited us09-web-zoom.com meeting page', 'direct',
             '{\"title\":\"Zoom Meeting\",\"nested\":{\"flag\":true}}',
             '/home/paya/.config/Brave/History', 'deadbeef', 'urls:43', 'k2'),
            (?1, 1, ?4, 'exact', '13436646800', 'WebKitMicros', NULL,
             'web.download', 'https://cdn.zoom.us/zoom_amd64.deb', 'cdn.zoom.us',
             'downloaded zoom_amd64.deb', 'direct',
             '{\"bytes\":288800000}', '/home/paya/.config/Brave/History',
             'deadbeef', 'downloads:7', 'k3'),
            (?1, 3, ?5, 'second', '2026-10-16 21:08:00', 'DpkgLog', 'local',
             'pkg.install', 'zoom:amd64', NULL,
             'installed zoom package', 'derived',
             NULL, '/var/log/dpkg.log', 'cafe01', 'line:19', 'k4'),
            (?1, 2, 0, 'order_only', NULL, NULL, NULL,
             'shell.command', 'sudo apt install ./zoom_amd64.deb', NULL,
             'sudo apt install ./zoom_amd64.deb', 'heuristic',
             NULL, '/home/paya/.bash_history', 'bash01', 'line:88', 'k5'),
            (?1, 2, ?6, 'exact', '1760648000', 'UnixSeconds', NULL,
             'file.created', '/tmp/report_\u{202e}fdp.exe', NULL,
             'created spoofed filename', 'direct',
             NULL, '/var/log/audit.log', 'aud01', 'line:3', 'k6')",
        rusqlite::params![
            run_id,
            base_ts,
            base_ts + 2_000_000,
            base_ts + 120_000_000,
            base_ts + 240_000_000,
            base_ts + 300_000_000,
        ],
    )
    .expect("insert events");

    let e2_id: u64 = conn
        .query_row("SELECT id FROM events WHERE dedup_key = 'k2'", [], |r| {
            r.get(0)
        })
        .expect("e2 id");
    let e3_id: u64 = conn
        .query_row("SELECT id FROM events WHERE dedup_key = 'k3'", [], |r| {
            r.get(0)
        })
        .expect("e3 id");

    conn.execute(
        "INSERT INTO links (src_type, src_id, dst_type, dst_id, rel, confidence, why)
         VALUES ('event', ?1, 'event', ?2, 'downloaded_as', 'direct', 'same browser session')",
        rusqlite::params![e2_id, e3_id],
    )
    .expect("insert link");

    conn.execute(
        "INSERT INTO iocs (type, value, note, added_utc) VALUES ('domain', 'us09-web-zoom.com', 'zoom-lookalike', 1760647000)",
        [],
    )
    .expect("insert ioc rule");
    let rule_id: i64 = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO links (src_type, src_id, dst_type, dst_id, rel, confidence, why)
         VALUES ('event', ?1, 'ioc', ?2, 'ioc_match', 'direct', 'zoom-lookalike')",
        rusqlite::params![e2_id, rule_id],
    )
    .expect("insert ioc link");

    e2_id
}

fn new_shell(case_path: &Path) -> Shell {
    let ctx = egui::Context::default();
    let settings = Settings {
        last_view: Some("timeline".to_string()),
        ..Settings::default()
    };
    let mut shell = Shell::new(case_path.to_path_buf(), Tz::Utc, settings, None, &ctx);
    shell.apply(Action::Go(View::Timeline));
    shell
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

#[test]
fn core_loop_facet_filter_undo_and_chip_removal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_fake_zoom_case(&db);

    let shell = new_shell(&db);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        );

    pump_until(&mut harness, |s| {
        s.model().counts.value().is_some()
            && s.model().category.value().is_some()
            && s.model().histogram.value().is_some()
    });

    assert_eq!(
        harness
            .state()
            .model()
            .counts
            .value()
            .map(|c| (c.in_scope, c.in_case)),
        Some((6, 6))
    );
    harness.get_by_label("6 / 6");
    harness.get_by_label("visited us09-web-zoom.com meeting page");

    // Click Category -> Downloads in the facet panel
    harness.get_by_label("Downloads").click();
    harness.step();
    pump_until(&mut harness, |s| {
        s.model().counts.generation() == s.model().generation
            && s.model().counts.value().is_some_and(|c| c.in_scope == 1)
    });

    assert_eq!(
        harness.state().model().counts.value().map(|c| c.in_scope),
        Some(1)
    );
    assert!(harness.state().model().scope.to_cli().contains("--kind"));
    assert!(
        harness
            .state()
            .model()
            .scope
            .to_cli()
            .contains("web.download")
    );

    // Undo restores all 6 events
    harness.get_by_label("undo").click();
    pump_until(&mut harness, |s| {
        s.model().counts.generation() == s.model().generation
            && s.model().counts.value().is_some_and(|c| c.in_scope == 6)
    });
    assert!(harness.state().model().scope.is_empty());
}

#[test]
fn window_untimed_notice_and_show_untimed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_fake_zoom_case(&db);

    let mut shell = new_shell(&db);
    let base_ts: i64 = 1_760_647_493_000_000;
    shell.apply(Action::Scope(ScopeAction::SetWindow {
        since: Some(base_ts),
        until: Some(base_ts + 130_000_000),
    }));

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        );

    pump_until(&mut harness, |s| {
        s.model().counts.generation() == s.model().generation
            && s.model().untimed_hidden() == Some(1)
    });

    // Status bar shows the untimed notice; clicking "show" switches to untimed view in 1 step
    harness.get_by_label("show").click();
    pump_until(&mut harness, |s| {
        s.model().counts.generation() == s.model().generation
            && s.model().counts.value().is_some_and(|c| c.in_scope == 1)
    });
    harness.get_by_label("sudo apt install ./zoom_amd64.deb");

    // Undo brings back the window and notice
    harness.get_by_label("undo").click();
    pump_until(&mut harness, |s| {
        s.model().counts.generation() == s.model().generation
            && s.model().untimed_hidden() == Some(1)
    });
}

#[test]
fn inspector_selection_links_pin_and_rtl_safeguard() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    let e2_id = build_fake_zoom_case(&db);

    let mut shell = new_shell(&db);
    shell.apply(Action::Select(Selection::Event(e2_id)));

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        );

    pump_until(&mut harness, |s| {
        s.model()
            .detail
            .value()
            .and_then(|d| d.as_ref())
            .is_some_and(|d| d.id == e2_id)
    });

    assert!(harness.query_all_by_label("web.visit").count() >= 2);
    harness.get_by_label("https://us09-web-zoom.com/j/4139766");
    // Pin inspector, select another event, verify inspector stays on pinned e2_id
    harness.get_by_label("pin").click();
    harness.step();
    assert_eq!(
        harness.state().model().inspector_pin,
        Some(Selection::Event(e2_id))
    );

    harness
        .state_mut()
        .apply(Action::Select(Selection::Event(1)));
    harness.step();
    assert_eq!(harness.state().model().inspected(), Selection::Event(e2_id));

    // Unpin and click outgoing link -> jumps to downloaded event (web.download)
    harness.get_by_label("pinned").click();
    harness.step();
    harness
        .state_mut()
        .apply(Action::Select(Selection::Event(e2_id)));
    pump_until(&mut harness, |s| {
        s.model()
            .detail
            .value()
            .and_then(|d| d.as_ref())
            .is_some_and(|d| d.id == e2_id)
    });
    harness
        .get_by_label("> downloaded_as · downloaded zoom_amd64.deb")
        .click();
    pump_until(&mut harness, |s| {
        s.model()
            .detail
            .value()
            .and_then(|d| d.as_ref())
            .is_some_and(|d| d.kind.as_str() == "web.download")
    });

    // Back button returns to e2_id
    harness.get_by_label("‹").click();
    pump_until(&mut harness, |s| {
        s.model()
            .detail
            .value()
            .and_then(|d| d.as_ref())
            .is_some_and(|d| d.id == e2_id)
    });

    // Selecting hostile RTL event triggers non-dismissible RTL notice + escaped code point
    harness
        .state_mut()
        .apply(Action::Select(Selection::Event(6)));
    pump_until(&mut harness, |s| {
        s.model()
            .detail
            .value()
            .and_then(|d| d.as_ref())
            .is_some_and(|d| !d.escaped_warnings.is_empty())
    });
    harness.get_by_label(
        "RTL override or control characters present in raw evidence (stripped in display).",
    );
    harness.get_by_label("subject: /tmp/report_\\u{202E}fdp.exe");
}

#[test]
fn group_by_host_and_drill_down_with_confidence_and_ioc_filter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_fake_zoom_case(&db);

    let mut shell = new_shell(&db);
    shell.apply(Action::SetGroupBy(Some(fp_query::overview::GroupBy::Host)));

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        );

    pump_until(&mut harness, |s| {
        s.model().groups.value().is_some_and(|g| !g.is_empty())
    });

    harness.get_by_label("us09-web-zoom.com");
    harness.get_by_label("page-redirect.com");
    // Double-click us09-web-zoom.com row in pivot -> drills down into host filter & returns to rows
    harness.get_by_label("us09-web-zoom.com").click();
    harness.step();
    harness.get_by_label("us09-web-zoom.com").click();
    pump_until(&mut harness, |s| {
        s.model().group_by.is_none()
            && s.model().counts.generation() == s.model().generation
            && s.model().counts.value().is_some_and(|c| c.in_scope == 1)
    });
    assert_eq!(
        harness.state().model().scope.filter().hosts,
        vec!["us09-web-zoom.com"]
    );

    // Clear scope via button
    harness.get_by_label("clear").click();
    pump_until(&mut harness, |s| {
        s.model().counts.generation() == s.model().generation
            && s.model().counts.value().is_some_and(|c| c.in_scope == 6)
    });

    // IOC hits only toggle narrows to 1 event and matches --confidence / --ioc-only CLI parity
    harness.get_by_label("IOC hits only (1)").click();
    pump_until(&mut harness, |s| {
        s.model().counts.generation() == s.model().generation
            && s.model().counts.value().is_some_and(|c| c.in_scope == 1)
    });
    assert!(
        harness
            .state()
            .model()
            .scope
            .cli_args()
            .contains(&"--ioc-only".to_string())
    );
    harness
        .state_mut()
        .apply(Action::Scope(ScopeAction::SetConfidence(vec![
            Confidence::Direct,
        ])));
    assert!(
        harness
            .state()
            .model()
            .scope
            .cli_args()
            .contains(&"--confidence".to_string())
    );
}

#[test]
fn keyboard_navigation_and_panel_toggles_work_without_mouse() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_fake_zoom_case(&db);

    let shell = new_shell(&db);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        );

    pump_until(&mut harness, |s| s.model().counts.value().is_some());
    //Wait for first page block so ArrowDown selects row 1 cleanly
    harness.step();
    std::thread::sleep(Duration::from_millis(80));
    harness.step();

    // Keyboard down moves selection onto event 1 then 2
    harness.key_press(egui::Key::ArrowDown);
    harness.step();
    assert_ne!(harness.state().model().selection(), Selection::None);

    // Clear shortcut works after scoping
    harness
        .state_mut()
        .apply(Action::Scope(ScopeAction::SetText(Some("zoom".into()))));
    pump_until(&mut harness, |s| {
        s.model().counts.generation() == s.model().generation
    });
    assert!(!harness.state().model().scope.is_empty());

    harness.key_press_modifiers(
        egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
        egui::Key::Backspace,
    );
    harness.step();
    assert!(harness.state().model().scope.is_empty());
}
