//! UI + worker integration tests for the palette, help overlay, collect view
//! form, and the export dialog.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use fp_gui::app::Shell;
use fp_gui::model::{Action, View};
use fp_gui::settings::Settings;
use fp_query::Tz;
use fp_store::Store;

fn build_case(path: &Path) {
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
         VALUES (?1, 'chromium.history', 'brave', 'Default', 'ok')",
        rusqlite::params![run_id],
    )
    .expect("insert source");
    conn.execute(
        "INSERT INTO events
            (run_id, source_id, ts_utc, ts_precision, kind, subject, subject_host, confidence, dedup_key)
         VALUES (?1, 1, 1_760_647_493_000_000, 'exact', 'web.visit',
                 'https://us09-web-zoom.com/j/4139766', 'us09-web-zoom.com', 'direct', 'k1')",
        rusqlite::params![run_id],
    )
    .expect("insert event");
}

fn new_shell(case_path: &Path) -> Shell {
    let ctx = egui::Context::default();
    Shell::new(
        case_path.to_path_buf(),
        Tz::Utc,
        Settings::default(),
        None,
        &ctx,
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

#[test]
fn palette_help_and_collect_form() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_case(&db);

    let shell = new_shell(&db);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 900.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        );
    pump_until(&mut harness, |s| s.model().counts.value().is_some());

    // Ctrl+K opens the palette; Enter on the first hit runs it
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::K);
    harness.step();
    harness.step();
    harness.get_by_label("Command palette");
    harness.key_press(egui::Key::Enter);
    harness.step();
    harness.step();
    assert_eq!(harness.state().model().view(), View::Dashboard);

    // F1 opens the help overlay with the keyboard map and the legend
    harness.key_press(egui::Key::F1);
    harness.step();
    harness.step();
    harness.get_by_label_contains("Keyboard");
    harness.get_by_label_contains("Confidence discs");
    harness.get_by_label_contains("Colours");
    harness.key_press(egui::Key::F1);
    harness.step();
    harness.step();
    assert_eq!(
        harness
            .query_all_by_label_contains("Confidence discs")
            .count(),
        0
    );

    // the collect view form shows the equivalent command live
    harness.state_mut().apply(Action::Go(View::Collect));
    harness.step();
    harness.step();
    harness.get_by_label_contains("equivalent command:");
    harness.get_by_label_contains("footprint collect --since -7d");
    harness.get_by_label("Start");
}

#[test]
fn export_dialog_writes_the_scoped_events() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_case(&db);

    let shell = new_shell(&db);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 900.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        );
    pump_until(&mut harness, |s| s.model().counts.value().is_some());

    // the dialog opens from the top bar with a live count preview
    harness.state_mut().apply(Action::OpenExport);
    harness.step();
    harness.step();
    harness.get_by_label("Export scope");
    harness.get_by_label_contains("1 events in scope will be written");
    harness.get_by_label("close").click();
    harness.step();
    harness.step();

    // a real export on a background thread writes the file
    let out = dir.path().join("out.csv");
    harness.state_mut().apply(Action::ExportScope {
        path: out.clone(),
        format: "csv".to_string(),
        redact: true,
    });
    pump_until(&mut harness, |_| out.exists());
    let content = std::fs::read_to_string(&out).expect("read export");
    assert!(content.contains("us09-web-zoom.com"));
}
