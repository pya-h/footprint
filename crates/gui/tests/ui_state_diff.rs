//! UI + worker integration tests for the State and Diff views via `egui_kittest`.

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

/// Two runs whose state drifts: a permission flip, a firewall toggle, an added
/// unowned service, and a removed cron entry.
fn build_state_case(path: &Path) {
    let mut store = Store::open_or_create(path).expect("create case db");
    let run1 = store
        .insert_run(&fp_store::NewRun {
            host: "workstation",
            user: "paya",
            uid: 1000,
            euid: 1000,
            version: "0.1.0",
            tz: "UTC",
            args: "collect --run 1",
        })
        .expect("insert run 1");
    let run2 = store
        .insert_run(&fp_store::NewRun {
            host: "workstation",
            user: "paya",
            uid: 1000,
            euid: 1000,
            version: "0.1.0",
            tz: "UTC",
            args: "collect --run 2",
        })
        .expect("insert run 2");

    let conn = store.conn();
    // the shell shows an empty-case state when the case has no events at all
    conn.execute(
        "INSERT INTO sources (run_id, kind, app, profile, status) VALUES (?1, 'shell.history', 'bash', NULL, 'ok')",
        rusqlite::params![run1],
    )
    .expect("insert source");
    conn.execute(
        "INSERT INTO events
            (run_id, source_id, ts_utc, ts_precision, kind, subject, confidence, dedup_key)
         VALUES (?1, 1, 1_760_647_493_000_000, 'exact', 'shell.command', 'sudo ufw enable', 'direct', 'e1')",
        rusqlite::params![run1],
    )
    .expect("insert event");

    let insert = |sql: &str, run: u64| {
        conn.execute(sql, rusqlite::params![run])
            .expect("insert state");
    };
    insert(
        "INSERT INTO state (run_id, kind, key, value_json) VALUES
         (?1, 'web.site_permission', 'brave/Default|https://us09-web-zoom.com:443|media_stream_camera',
          '{\"decision\":\"grant\",\"origin\":\"https://us09-web-zoom.com\"}'),
         (?1, 'web.site_permission', 'brave/Default|https://us09-web-zoom.com:443|media_stream_microphone',
          '{\"decision\":\"deny\"}'),
         (?1, 'persist.systemd', '/etc/systemd/system/updater.service',
          '{\"owner_pkg\":null,\"integrity\":\"unowned\",\"flags\":[\"unowned\"]}'),
         (?1, 'persist.cron', '/etc/cron.d/backup',
          '{\"owner_pkg\":\"cron\",\"integrity\":\"ok\",\"flags\":[]}'),
         (?1, 'ufw.status', 'firewall', '{\"active\":false,\"flags\":[\"firewall_disabled\"]}')",
        run1,
    );
    insert(
        "INSERT INTO state (run_id, kind, key, value_json) VALUES
         (?1, 'web.site_permission', 'brave/Default|https://us09-web-zoom.com:443|media_stream_camera',
          '{\"decision\":\"grant\",\"origin\":\"https://us09-web-zoom.com\"}'),
         (?1, 'web.site_permission', 'brave/Default|https://us09-web-zoom.com:443|media_stream_microphone',
          '{\"decision\":\"grant\"}'),
         (?1, 'persist.systemd', '/etc/systemd/system/updater.service',
          '{\"owner_pkg\":null,\"integrity\":\"unowned\",\"flags\":[\"unowned\"]}'),
         (?1, 'persist.systemd', '/etc/systemd/system/new.service',
          '{\"owner_pkg\":null,\"integrity\":\"unowned\",\"flags\":[\"unowned\"]}'),
         (?1, 'ufw.status', 'firewall', '{\"active\":true}')",
        run2,
    );
}

fn new_shell(case_path: &Path, view: View) -> Shell {
    let ctx = egui::Context::default();
    let settings = Settings::default();
    let mut shell = Shell::new(case_path.to_path_buf(), Tz::Utc, settings, None, &ctx);
    shell.apply(Action::Go(view));
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
fn state_view_filters_kinds_and_explains_flags() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_state_case(&db);

    let shell = new_shell(&db, View::State);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        );

    // the view defaults to the last run (run 2): five rows across three kinds
    pump_until(&mut harness, |s| s.model().states.value().is_some());
    harness.step();
    harness.step();
    harness.get_by_label("all (5)");
    harness.get_by_label("web.site_permission  2");
    harness.get_by_label("persist.systemd  2  !2");

    // flagged-only narrows client-side to the two unowned services
    harness.get_by_label("flagged only").click();
    harness.step();
    harness.step();
    harness.get_by_label("/etc/systemd/system/new.service");
    assert_eq!(
        harness
            .query_all_by_label_contains("media_stream_camera")
            .count(),
        0
    );

    // back to everything, pick the permissions kind, and inspect the camera row
    harness.get_by_label("flagged only").click();
    harness.step();
    harness.step();
    harness.get_by_label("web.site_permission  2").click();
    harness.step();
    harness.get_by_label_contains("media_stream_camera").click();
    // the accesskit tree lags the model by one frame: settle before asserting
    harness.step();
    harness.step();
    // the key now appears twice: once in the table, once as the inspector header
    assert!(
        harness
            .get_all_by_label_contains("us09-web-zoom.com:443|media_stream_camera")
            .count()
            >= 2
    );
    harness.get_by_label("decision:");

    // the unowned service explains itself in the inspector
    harness.get_by_label("persist.systemd  2  !2").click();
    harness.step();
    harness.get_by_label_contains("new.service").click();
    harness.step();
    harness.step();
    harness.get_by_label_contains("no installed package claims this file");
}

#[test]
fn diff_view_groups_entries_and_compares_values() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_state_case(&db);

    let shell = new_shell(&db, View::Diff);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        );

    // the pickers default to the last two runs and the diff loads on its own
    pump_until(&mut harness, |s| s.model().diff.value().is_some());
    harness.step();
    harness.step();
    harness.get_by_label("+ added (1)");
    harness.get_by_label("- removed (1)");
    harness.get_by_label("~ changed (2)");
    harness.get_by_label_contains("persist.cron");
    harness.get_by_label_contains("persist.systemd");
    harness.get_by_label_contains("decision: deny -> grant");

    // selecting the changed permission shows both values with the flip marked
    harness
        .get_by_label_contains("media_stream_microphone")
        .click();
    harness.step();
    harness.step();
    harness.get_by_label("\"grant\"");

    // the chips double as filters: hiding added removes the new.service row
    harness.get_by_label("+ added (1)").click();
    harness.step();
    harness.step();
    assert_eq!(
        harness.query_all_by_label_contains("new.service").count(),
        0
    );
}
