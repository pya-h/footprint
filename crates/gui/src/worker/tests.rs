#![cfg(test)]
//! Worker pool tests; the module is gated in `mod.rs`, and the file-level gate
//! keeps the CI no-unwrap scan from treating test code as product code.

use super::{ReqKind, Worker};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;

fn wait_for(mut cond: impl FnMut() -> bool) {
    for _ in 0..3000 {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!("condition not reached");
}

fn tiny_case() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("case.db");
    seed_case(&db);
    (dir, db)
}

fn seed_case(db: &std::path::Path) {
    let mut store = fp_store::Store::open_or_create(db).unwrap();
    let run = store
        .insert_run(&fp_store::NewRun {
            host: "h",
            user: "u",
            uid: 1,
            euid: 1,
            version: "0",
            tz: "UTC",
            args: "",
        })
        .unwrap();
    let source = store
        .insert_source(
            run,
            &fp_core::SourceInstance::new("shell.history", "bash", None, "/h".into()),
        )
        .unwrap();
    store
        .ingest(
            run,
            &fp_store::Batch {
                source_identity: "shell.history|bash|/h",
                events: &[fp_core::Event {
                    run_id: run,
                    source_kind: "shell.history".into(),
                    ts_utc: 100,
                    ts_precision: fp_core::TimePrecision::Second,
                    ts_raw: "100".into(),
                    ts_raw_format: fp_core::time::TimeFormat::UnixSeconds,
                    ts_tz_assumed: None,
                    source_inst: source,
                    kind: fp_core::EventKind::ShellCommand,
                    actor: None,
                    subject: "ls".into(),
                    subject_host: None,
                    summary: "ls".into(),
                    attrs: serde_json::json!({}),
                    provenance: fp_core::Provenance {
                        file: "/h/.bash_history".into(),
                        file_sha256: None,
                        locator: "1".into(),
                    },
                    confidence: fp_core::Confidence::Direct,
                }],
                states: &[],
                artifacts: &[],
            },
        )
        .unwrap();
}

#[test]
fn responses_carry_id_kind_gen_and_payload() {
    let (_dir, db) = tiny_case();
    let paints = Arc::new(AtomicUsize::new(0));
    let painted = Arc::clone(&paints);
    let worker = Worker::spawn(&db, 2, move || {
        painted.fetch_add(1, Ordering::SeqCst);
    })
    .unwrap();

    let id = worker.submit(ReqKind::Count, None, 7, |store| {
        Ok(Box::new(store.event_count()?))
    });
    let mut found = None;
    wait_for(|| {
        for resp in worker.poll() {
            if resp.kind == ReqKind::Count {
                found = Some((
                    resp.id,
                    resp.generation,
                    resp.result
                        .as_ref()
                        .unwrap()
                        .as_ref()
                        .downcast_ref::<u64>()
                        .copied()
                        .unwrap(),
                ));
            }
        }
        found.is_some()
    });
    assert_eq!(found, Some((id, 7, 1)));
    wait_for(|| paints.load(Ordering::SeqCst) > 0);
    worker.shutdown();
}

#[test]
fn pending_request_of_a_kind_is_superseded_by_the_latest() {
    let (_dir, db) = tiny_case();
    let worker = Worker::spawn(&db, 1, || {}).unwrap();

    // hold the single reader with a slow request of another kind
    let release = Arc::new(Barrier::new(2));
    let gate = Arc::clone(&release);
    worker.submit(ReqKind::Trace, None, 1, move |_store| {
        gate.wait();
        Ok(Box::new(()))
    });

    let first = worker.submit(ReqKind::Count, None, 1, |_store| Ok(Box::new("first")));
    let latest = worker.submit(ReqKind::Count, None, 2, |_store| Ok(Box::new("latest")));
    assert_ne!(first, latest);
    release.wait();

    let mut saw_count = None;
    wait_for(|| {
        for resp in worker.poll() {
            if resp.kind == ReqKind::Count {
                saw_count = Some(resp.id);
            }
        }
        saw_count.is_some()
    });
    assert_eq!(saw_count, Some(latest));
    worker.shutdown();
}

#[test]
fn subs_of_one_kind_run_in_parallel_and_supersede_per_sub() {
    let (_dir, db) = tiny_case();
    let worker = Worker::spawn(&db, 2, || {}).unwrap();

    // hold both readers with a slow request each, so pending requests queue up
    // (two different kinds: same-kind requests share a sub key and supersede)
    let release = Arc::new(Barrier::new(3));
    let gate = Arc::clone(&release);
    worker.submit(ReqKind::Trace, None, 1, move |_store| {
        gate.wait();
        Ok(Box::new(()))
    });
    let gate = Arc::clone(&release);
    worker.submit(ReqKind::Detail, None, 1, move |_store| {
        gate.wait();
        Ok(Box::new(()))
    });
    let kind_a = worker.submit(ReqKind::Facets, Some("kind".into()), 1, |_store| {
        Ok(Box::new("kind"))
    });
    let host_a = worker.submit(ReqKind::Facets, Some("host".into()), 1, |_store| {
        Ok(Box::new("host"))
    });
    // a newer request of the same sub supersedes only that sub
    let kind_b = worker.submit(ReqKind::Facets, Some("kind".into()), 1, |_store| {
        Ok(Box::new("kind2"))
    });
    release.wait();

    let mut seen = Vec::new();
    wait_for(|| {
        for resp in worker.poll() {
            if resp.kind == ReqKind::Facets {
                seen.push((
                    resp.id,
                    resp.result
                        .as_ref()
                        .unwrap()
                        .as_ref()
                        .downcast_ref::<&str>()
                        .copied(),
                ));
            }
        }
        seen.len() == 2
    });
    let mut ids: Vec<u64> = seen.iter().map(|(id, _)| *id).collect();
    ids.sort();
    assert_eq!(ids, vec![host_a, kind_b]);
    assert!(!ids.contains(&kind_a), "the superseded sub must not answer");
    worker.shutdown();
}

#[test]
fn cancel_interrupts_the_running_query_quietly() {
    let (_dir, db) = tiny_case();
    let worker = Worker::spawn(&db, 1, || {}).unwrap();

    worker.submit(ReqKind::Count, None, 1, |store| {
        // long enough to be interrupted; failure maps to Interrupted via From
        let total: rusqlite::Result<i64> = store.conn().query_row(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 100000000)
             SELECT SUM(i) FROM n",
            [],
            |row| row.get(0),
        );
        total.map_err(fp_store::StoreError::from)?;
        Ok(Box::new(()))
    });
    // let the query start, then supersede it
    std::thread::sleep(Duration::from_millis(50));
    worker.cancel(ReqKind::Count);

    // the pool stays alive and answers the next request of that kind
    let id = worker.submit(ReqKind::Count, None, 2, |store| {
        Ok(Box::new(store.event_count()?))
    });
    let mut answered = None;
    wait_for(|| {
        for resp in worker.poll() {
            if resp.kind == ReqKind::Count && resp.id == id {
                answered = resp
                    .result
                    .as_ref()
                    .unwrap()
                    .as_ref()
                    .downcast_ref::<u64>()
                    .copied();
            }
        }
        answered.is_some()
    });
    assert_eq!(answered, Some(1));
    worker.shutdown();
}

#[test]
fn a_panicking_query_becomes_an_error_and_the_reader_survives() {
    let (_dir, db) = tiny_case();
    let worker = Worker::spawn(&db, 1, || {}).unwrap();

    worker.submit(ReqKind::Detail, None, 1, |_store| panic!("boom"));
    worker.submit(ReqKind::Count, None, 1, |store| {
        Ok(Box::new(store.event_count()?))
    });

    let mut error = None;
    let mut count: Option<u64> = None;
    wait_for(|| {
        for resp in worker.poll() {
            match resp.kind {
                ReqKind::Detail => error = Some(format!("{:?}", resp.result.err().unwrap())),
                ReqKind::Count => {
                    count = resp
                        .result
                        .as_ref()
                        .unwrap()
                        .as_ref()
                        .downcast_ref::<u64>()
                        .copied()
                }
                _ => {}
            }
        }
        error.is_some() && count.is_some()
    });
    assert!(error.unwrap().contains("worker panic: boom"));
    assert_eq!(count, Some(1));
    worker.shutdown();
}

#[test]
fn the_writer_creates_the_case_and_applies_writes_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("new-case.db");

    let worker = Worker::spawn(&db, 1, || {}).unwrap();
    let first = worker.submit_writer(ReqKind::IocWrite, 1, |store| {
        Ok(Box::new(store.insert_ioc("domain", "zoom.us", None)?))
    });
    let second = worker.submit_writer(ReqKind::IocWrite, 1, |store| {
        Ok(Box::new(store.list_iocs()?.len() as u64))
    });

    let mut got = Vec::new();
    wait_for(|| {
        for resp in worker.poll() {
            let value: Option<u64> = resp
                .result
                .as_ref()
                .ok()
                .and_then(|payload| payload.as_ref().downcast_ref::<u64>().copied());
            if resp.id == first {
                got.push(("first", value));
            }
            if resp.id == second {
                got.push(("second", value));
            }
        }
        got.len() == 2
    });
    assert!(db.exists(), "the writer's open_or_create made the case");
    // writes ran in submission order: the listing saw the insert
    assert!(
        got.iter()
            .any(|(name, value)| *name == "second" && matches!(value, Some(1)))
    );
    worker.shutdown();
}

#[test]
fn spawning_on_a_missing_case_allows_the_writer_to_create_it() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nope.db");
    let worker = Worker::spawn(&missing, 1, || {}).unwrap();
    let id = worker.submit_writer(ReqKind::IocWrite, 1, |store| {
        Ok(Box::new(store.insert_ioc("domain", "zoom.us", None)?))
    });
    let mut created = None;
    wait_for(|| {
        for resp in worker.poll() {
            if resp.id == id {
                created = resp
                    .result
                    .as_ref()
                    .ok()
                    .and_then(|p| p.as_ref().downcast_ref::<u64>().copied());
            }
        }
        created.is_some()
    });
    assert_eq!(created, Some(1));
    worker.shutdown();
}

#[test]
fn spawning_on_a_corrupt_case_errors() {
    let dir = tempfile::tempdir().unwrap();
    let bad = dir.path().join("bad.db");
    std::fs::write(&bad, b"not a sqlite file").unwrap();
    let result = Worker::spawn(&bad, 1, || {});
    assert!(result.is_err());
}

#[test]
fn the_watcher_reports_the_first_sample_and_external_writes() {
    let (_dir, db) = tiny_case();
    let worker = Worker::spawn(&db, 1, || {}).unwrap();

    let mut samples: Vec<i64> = Vec::new();
    wait_for(|| {
        for resp in worker.poll() {
            if resp.kind == ReqKind::Watch
                && let Some(v) = resp
                    .result
                    .as_ref()
                    .ok()
                    .and_then(|p| p.as_ref().downcast_ref::<i64>().copied())
            {
                samples.push(v);
            }
        }
        !samples.is_empty()
    });

    // a commit from another connection (what `footprint collect` does) shows up
    let mut writer = fp_store::Store::open_or_create(&db).unwrap();
    writer
        .insert_run(&fp_store::NewRun {
            host: "h",
            user: "u",
            uid: 1,
            euid: 1,
            version: "0",
            tz: "UTC",
            args: "collect",
        })
        .unwrap();
    drop(writer);

    wait_for(|| {
        for resp in worker.poll() {
            if resp.kind == ReqKind::Watch
                && let Some(v) = resp
                    .result
                    .as_ref()
                    .ok()
                    .and_then(|p| p.as_ref().downcast_ref::<i64>().copied())
            {
                samples.push(v);
            }
        }
        samples.len() >= 2
    });
    assert!(
        samples.windows(2).any(|w| w[1] > w[0]),
        "an external commit must raise the reported version: {samples:?}"
    );
    worker.shutdown();
}

#[test]
fn a_case_appearing_after_spawn_is_opened_and_watched() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("later.db");
    let worker = Worker::spawn(&db, 1, || {}).unwrap();
    // let the reader and watcher hit the missing file a few times first
    std::thread::sleep(Duration::from_millis(700));
    seed_case(&db);

    let id = worker.submit(ReqKind::Count, None, 1, |store| {
        Ok(Box::new(store.event_count()?))
    });
    let mut count = None;
    let mut watched = false;
    wait_for(|| {
        for resp in worker.poll() {
            match resp.kind {
                ReqKind::Count if resp.id == id => {
                    count = resp
                        .result
                        .as_ref()
                        .ok()
                        .and_then(|p| p.as_ref().downcast_ref::<u64>().copied());
                }
                ReqKind::Watch => watched = true,
                _ => {}
            }
        }
        count == Some(1) && watched
    });
    worker.shutdown();
}
