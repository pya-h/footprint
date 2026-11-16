//! Hostile-input, error-handling, worker-panic, concurrency, and responsive
//! layout tests for the GUI shell.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use fp_gui::app::Shell;
use fp_gui::model::{Action, View};
use fp_gui::settings::Settings;
use fp_gui::worker::{QueryError, ReqKind, Worker};
use fp_query::Tz;
use fp_store::Store;

fn new_shell(case_path: &Path, view: View) -> Shell {
    let ctx = egui::Context::default();
    let mut shell = Shell::new(
        case_path.to_path_buf(),
        Tz::Utc,
        Settings::default(),
        None,
        &ctx,
    );
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
fn corrupt_case_renders_error_screen_without_panicking() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bad = dir.path().join("corrupt.db");
    std::fs::write(&bad, b"this is definitely not sqlite").expect("write junk");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            new_shell(&bad, View::Dashboard),
        );
    harness.step();
    harness.step();
    harness.get_by_label("cannot open the case");
}

#[test]
fn old_schema_case_renders_error_screen_without_panicking() {
    let dir = tempfile::tempdir().expect("tempdir");
    let old = dir.path().join("legacy_v1.db");
    {
        let conn = rusqlite::Connection::open(&old).expect("open sqlite");
        conn.execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta (key, value) VALUES ('schema_version', '1');",
        )
        .expect("seed v1 meta");
    }

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            new_shell(&old, View::Dashboard),
        );
    harness.step();
    harness.step();
    harness.get_by_label("cannot open the case");
}

#[test]
fn read_only_0400_case_reads_cleanly_and_reports_write_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("readonly.db");
    {
        let mut store = Store::open_or_create(&db).expect("create case");
        let run_id = store
            .insert_run(&fp_store::NewRun {
                host: "workstation",
                user: "paya",
                uid: 1000,
                euid: 1000,
                version: "0.1.0",
                tz: "UTC",
                args: "collect",
            })
            .expect("insert run");
        store
            .conn()
            .execute(
                "INSERT INTO sources (run_id, kind, app, profile, status) VALUES (?1, 'shell.history', 'bash', NULL, 'ok')",
                rusqlite::params![run_id],
            )
            .expect("insert source");
        store
            .conn()
            .execute(
                "INSERT INTO events
                    (run_id, source_id, ts_utc, ts_precision, kind, subject, confidence, dedup_key)
                 VALUES (?1, 1, 1_760_647_493_000_000, 'exact', 'shell.command', 'ls -la', 'direct', 'e1')",
                rusqlite::params![run_id],
            )
            .expect("insert event");
    }
    let perms = std::fs::Permissions::from_mode(0o400);
    std::fs::set_permissions(&db, perms).expect("chmod 0400");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            new_shell(&db, View::Timeline),
        );
    pump_until(&mut harness, |s| s.model().counts.value().is_some());
    assert_eq!(
        harness
            .state()
            .model()
            .counts
            .value()
            .expect("counts")
            .in_case,
        1
    );

    // Trying to add an IOC on a 0400 database reports an error without panicking.
    harness.state_mut().apply(Action::AddIoc {
        ioc_type: "domain".to_string(),
        value: "example.com".to_string(),
        note: None,
    });
    pump_until(&mut harness, |s| s.model().ioc_status.is_some());
    assert!(
        harness
            .state()
            .model()
            .ioc_status
            .as_ref()
            .is_some_and(Result::is_err)
    );

    let restore = std::fs::Permissions::from_mode(0o600);
    let _ = std::fs::set_permissions(&db, restore);
}

#[test]
fn empty_case_and_deleted_case_handle_gracefully() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("empty.db");
    let _store = Store::open_or_create(&db).expect("create empty case");

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1280.0, 820.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            new_shell(&db, View::Dashboard),
        );
    pump_until(&mut harness, |s| s.model().counts.value().is_some());
    harness.step();
    harness.step();
    harness.get_by_label_contains("this case has no events yet");
    harness.get_by_label("Open Collect");

    // Removing the case file on disk does not crash subsequent frames or watcher cycles.
    std::fs::remove_file(&db).expect("remove db");
    harness.state_mut().apply(Action::RefreshCase);
    for _ in 0..10 {
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn worker_survives_panic_injection_and_serves_subsequent_reads() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    {
        let mut store = Store::open_or_create(&db).expect("create");
        let run_id = store
            .insert_run(&fp_store::NewRun {
                host: "h",
                user: "u",
                uid: 1000,
                euid: 1000,
                version: "0.1.0",
                tz: "UTC",
                args: "collect",
            })
            .expect("run");
        store
            .conn()
            .execute(
                "INSERT INTO sources (run_id, kind, app, profile, status) VALUES (?1, 'shell.history', 'bash', NULL, 'ok')",
                rusqlite::params![run_id],
            )
            .expect("source");
        store
            .conn()
            .execute(
                "INSERT INTO events
                    (run_id, source_id, ts_utc, ts_precision, kind, subject, confidence, dedup_key)
                 VALUES (?1, 1, 1_760_647_493_000_000, 'exact', 'shell.command', 'echo ok', 'direct', 'e1')",
                rusqlite::params![run_id],
            )
            .expect("event");
    }

    let worker = Worker::spawn(&db, 1, || {}).expect("spawn worker");
    let panic_id = worker.submit(ReqKind::Detail, None, 1, |_| panic!("boom"));
    let ok_id = worker.submit(ReqKind::Count, None, 1, |store| {
        Ok(Box::new(store.count_events(&Default::default())?))
    });

    let mut saw_panic = false;
    let mut saw_count = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(saw_panic && saw_count) && Instant::now() < deadline {
        for resp in worker.poll() {
            if resp.id == panic_id {
                assert!(
                    matches!(resp.result, Err(QueryError::Message(ref m)) if m.contains("boom"))
                );
                saw_panic = true;
            } else if resp.id == ok_id {
                let count = *resp
                    .result
                    .expect("count result")
                    .downcast::<u64>()
                    .expect("u64");
                assert_eq!(count, 1);
                saw_count = true;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(saw_panic && saw_count);
}

#[test]
fn narrow_viewport_uses_drawers_at_1100x700() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    {
        let mut store = Store::open_or_create(&db).expect("create");
        let run_id = store
            .insert_run(&fp_store::NewRun {
                host: "h",
                user: "u",
                uid: 1000,
                euid: 1000,
                version: "0.1.0",
                tz: "UTC",
                args: "collect",
            })
            .expect("run");
        store
            .conn()
            .execute(
                "INSERT INTO sources (run_id, kind, app, profile, status) VALUES (?1, 'shell.history', 'bash', NULL, 'ok')",
                rusqlite::params![run_id],
            )
            .expect("source");
        store
            .conn()
            .execute(
                "INSERT INTO events
                    (run_id, source_id, ts_utc, ts_precision, kind, subject, confidence, dedup_key)
                 VALUES (?1, 1, 1_760_647_493_000_000, 'exact', 'shell.command', 'echo narrow', 'direct', 'e1')",
                rusqlite::params![run_id],
            )
            .expect("event");
    }

    let mut harness = Harness::builder()
        .with_size(egui::vec2(1100.0, 700.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            new_shell(&db, View::Timeline),
        );
    pump_until(&mut harness, |s| s.model().counts.value().is_some());
    harness.step();
    harness.step();

    // Below 1280 px, open sidebars render as dismissible drawers instead of squeezing the timeline.
    harness.get_by_label("close facets");
    harness.get_by_label("close inspector").click();
    harness.step();
    harness.step();
    assert_eq!(harness.query_all_by_label("close inspector").count(), 0);
}

#[test]
fn concurrent_collect_while_gui_reads_stays_consistent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let evidence = dir.path().join("evidence");
    fp_fixtures::fake_zoom(&evidence).expect("fake_zoom");
    let db = dir.path().join("case.db");
    {
        let mut store = Store::open_or_create(&db).expect("create initial case");
        let run_id = store
            .insert_run(&fp_store::NewRun {
                host: "workstation",
                user: "paya",
                uid: 1000,
                euid: 1000,
                version: "0.1.0",
                tz: "UTC",
                args: "seed",
            })
            .expect("insert run");
        store
            .conn()
            .execute(
                "INSERT INTO sources (run_id, kind, app, profile, status) VALUES (?1, 'shell.history', 'bash', NULL, 'ok')",
                rusqlite::params![run_id],
            )
            .expect("insert source");
        store
            .conn()
            .execute(
                "INSERT INTO events
                    (run_id, source_id, ts_utc, ts_precision, kind, subject, confidence, dedup_key)
                 VALUES (?1, 1, 1_760_647_493_000_000, 'exact', 'shell.command', 'echo seed', 'direct', 'seed-1')",
                rusqlite::params![run_id],
            )
            .expect("insert event");
    }

    let worker = Worker::spawn(&db, 2, || {}).expect("spawn worker");

    let collect_db = db.clone();
    let collect_home = evidence.join("home/u");
    let collect_root = evidence.join("root");
    let collect_handle = std::thread::spawn(move || {
        fp_query::run_collect(
            &collect_db,
            &fp_query::CollectParams {
                since: Some(format!("@{}", fp_fixtures::local(0, 0, 0))),
                until: Some(format!("@{}", fp_fixtures::local(23, 59, 59))),
                assume_tz: Some(fp_fixtures::SCENARIO_TZ.into()),
                homes: vec![collect_home],
                root: collect_root,
                ..Default::default()
            },
            None,
        )
    });

    // Interleave active reads while run_collect ingests in parallel.
    let mut generation = 1u64;
    while !collect_handle.is_finished() {
        worker.submit(ReqKind::Count, None, generation, |store| {
            Ok(Box::new(store.count_events(&Default::default())?))
        });
        worker.submit(ReqKind::Page, None, generation, |store| {
            Ok(Box::new(store.list_events(&fp_store::EventFilter {
                limit: Some(50),
                ..Default::default()
            })?))
        });
        for resp in worker.poll() {
            if let Err(err) = resp.result {
                assert!(
                    matches!(err, QueryError::Interrupted),
                    "unexpected concurrent error: {err:?}"
                );
            }
        }
        generation += 1;
        std::thread::sleep(Duration::from_millis(5));
    }

    let summary = collect_handle
        .join()
        .expect("collect thread joined")
        .expect("run_collect succeeded");
    assert!(summary.errors.is_empty(), "{:?}", summary.errors);

    // Drain any in-flight work, then verify a clean post-collect read sees all events.
    let _ = worker.poll();
    let final_id = worker.submit(ReqKind::Count, None, generation + 1, |store| {
        Ok(Box::new(store.count_events(&Default::default())?))
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut final_count = None;
    while Instant::now() < deadline {
        for resp in worker.poll() {
            if resp.id == final_id {
                let count = *resp
                    .result
                    .expect("final count ok")
                    .downcast::<u64>()
                    .expect("u64");
                final_count = Some(count);
                break;
            }
        }
        if final_count.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let count = final_count.expect("final count response arrived");
    assert!(
        count > 1,
        "expected collected events beyond initial seed, got {count}"
    );
}
