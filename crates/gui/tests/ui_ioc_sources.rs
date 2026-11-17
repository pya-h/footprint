//! UI + worker integration tests for the IOC and Sources views via `egui_kittest`:
//! list with link-based counts, hit selection, remove on the writer thread,
//! coverage cards, the health matrix, and the runs table actions.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use fp_gui::app::Shell;
use fp_gui::model::{Action, Selection, View};
use fp_gui::settings::Settings;
use fp_query::Tz;
use fp_store::Store;

fn build_ioc_case(path: &Path) -> u64 {
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
         VALUES (?1, 'chromium.history', 'brave', 'Default', '\"present\"'),
                (?1, 'shell.history', 'bash', NULL, '\"partial\"')",
        rusqlite::params![run_id],
    )
    .expect("insert sources");
    conn.execute(
        "INSERT INTO events
            (run_id, source_id, ts_utc, ts_precision, kind, subject, subject_host, confidence, dedup_key)
         VALUES (?1, 1, 1_760_647_493_000_000, 'exact', 'web.visit',
                 'https://us09-web-zoom.com/j/4139766', 'us09-web-zoom.com', 'direct', 'k1')",
        rusqlite::params![run_id],
    )
    .expect("insert event");
    conn.execute(
        "INSERT INTO iocs (type, value, note, added_utc)
         VALUES ('domain', 'us09-web-zoom.com', 'zoom lookalike', 1760647000)",
        [],
    )
    .expect("insert ioc");
    let ioc_id: u64 = conn.last_insert_rowid() as u64;
    conn.execute(
        "INSERT INTO links (src_type, src_id, rel, dst_type, dst_id, confidence, why)
         VALUES ('event', 1, 'ioc_match', 'ioc', ?1, 'direct', 'zoom-lookalike')",
        rusqlite::params![ioc_id],
    )
    .expect("insert ioc link");
    ioc_id
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
fn ioc_view_lists_counts_selects_hits_and_removes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    let ioc_id = build_ioc_case(&db);

    let shell = new_shell(&db, View::Ioc);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        );

    pump_until(&mut harness, |s| s.model().iocs.value().is_some());
    harness.step();
    harness.step();
    harness.get_by_label_contains("1 hits");

    // select the IOC: its hits render on the right and its summary appears in the inspector
    harness
        .get_by_label_contains("domain  us09-web-zoom.com")
        .click();
    harness.step();
    harness.step();
    harness.get_by_label_contains("web.visit · 1 ·");
    harness.get_by_label_contains("matched: us09-web-zoom.com");
    harness.get_by_label("indicator · domain");
    harness.get_by_label("1 linked hits");

    // clicking a hit selects the event for the inspector while keeping the IOC filter active
    harness.get_by_label_contains("web.visit · 1 ·").click();
    harness.step();
    harness.step();
    assert_eq!(harness.state().model().selection(), Selection::Event(1));
    harness.get_by_label("HITS");
    harness.get_by_label("show all");

    // the add dialog opens and closes
    harness.get_by_label("+ Add").click();
    harness.step();
    harness.step();
    harness.get_by_label_contains("Add indicator");
    harness.get_by_label("cancel").click();
    harness.step();
    harness.step();
    assert_eq!(
        harness.query_all_by_label_contains("Add indicator").count(),
        0
    );

    // remove needs the second click; the writer drops the IOC and its hits
    harness.get_by_label("×").click();
    harness.step();
    harness.step();
    harness.get_by_label("sure?").click();
    pump_until(&mut harness, |s| {
        s.model()
            .iocs
            .value()
            .is_some_and(|iocs| iocs.iter().all(|i| i.id != ioc_id))
    });
    harness.step();
    harness.step();
    harness.get_by_label_contains("Removed IOC");
    harness.get_by_label_contains("no indicators yet");
    harness.get_by_label("dismiss").click();
    harness.step();
    assert!(harness.state().model().ioc_status.is_none());
}

#[test]
fn sources_view_shows_coverage_health_and_run_actions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_ioc_case(&db);

    let shell = new_shell(&db, View::Sources);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 1500.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        );

    pump_until(&mut harness, |s| s.model().sources.value().is_some());
    harness.step();
    harness.step();
    // coverage cards from the same coverage() the CLI uses
    harness.get_by_label_contains("Browser history");
    harness.get_by_label_contains("covered");
    harness.get_by_label_contains("Shell");
    // the card says "partial" and the answering line repeats it
    assert!(harness.get_all_by_label_contains("partial").count() >= 2);

    // health matrix rows carry the source identity
    harness.get_by_label_contains("brave/Default  (chromium.history)");
    harness.get_by_label_contains("bash  (shell.history)");

    // clicking a matrix cell selects the source for the inspector
    // (the cell's accessible name is its hover text)
    harness
        .get_by_label_contains("brave/Default — run 1: present")
        .click();
    harness.step();
    harness.step();
    assert!(matches!(
        harness.state().model().selection(),
        Selection::Source(_)
    ));
    harness.get_by_label("present");

    // "events first stored" scopes the timeline to run 1; the accesskit click
    // works without the button being on screen
    harness
        .get_by_label("events first stored")
        .click_accesskit();
    harness.step();
    harness.step();
    assert!(harness.state().model().scope.to_cli().contains("--run 1"));
    assert_eq!(harness.state().model().view(), View::Timeline);
}

#[test]
fn ioc_add_preserves_note_and_state_hit_click_selects_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_ioc_case(&db);
    let state_rowid: i64 = {
        let store = Store::open_or_create(&db).expect("open db");
        store
            .conn()
            .execute(
                "INSERT INTO state (run_id, kind, key, value_json)
                 VALUES (1, 'persist.cron', '/etc/cron.d/backdoor', '{\"origin\":\"https://us09-web-zoom.com\"}')",
                [],
            )
            .expect("insert state");
        let sid = store.conn().last_insert_rowid();
        store
            .conn()
            .execute(
                "INSERT INTO links (src_type, src_id, rel, dst_type, dst_id, confidence, why)
                 VALUES ('state', ?1, 'ioc_match', 'ioc', 1, 'direct', 'state-hit')",
                rusqlite::params![sid],
            )
            .expect("insert state ioc link");
        sid
    };

    let shell = new_shell(&db, View::Ioc);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 900.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        );

    pump_until(&mut harness, |s| s.model().iocs.value().is_some());
    harness.state_mut().apply(Action::AddIoc {
        ioc_type: "domain".into(),
        value: "evil.example".into(),
        note: Some("audit persistence check".into()),
    });
    pump_until(&mut harness, |s| {
        s.model()
            .iocs
            .value()
            .is_some_and(|rows| rows.iter().any(|r| r.value.as_str() == "evil.example"))
    });
    harness
        .get_by_label_contains("domain  evil.example")
        .click();
    harness.step();
    harness.step();
    harness.get_by_label_contains("audit persistence check");

    harness
        .get_by_label_contains("domain  us09-web-zoom.com")
        .click();
    pump_until(&mut harness, |s| {
        s.model()
            .ioc_hits
            .value()
            .is_some_and(|rows| rows.iter().any(|h| h.target_type.as_str() == "state"))
    });
    harness.step();
    harness.get_by_label_contains("persist.cron · ").click();
    harness.step();
    assert_eq!(
        harness.state().model().selection(),
        Selection::State(state_rowid)
    );
}
