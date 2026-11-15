//! UI + worker integration tests for the Trace view via `egui_kittest`:
//! the chain the GUI builds must equal the CLI `trace` order and confidences.

use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use fp_core::Confidence;
use fp_gui::app::Shell;
use fp_gui::model::{Action, View};
use fp_gui::settings::Settings;
use fp_gui::views::trace::render_trace_text;
use fp_gui::vm::TraceHeadVm;
use fp_query::Tz;
use fp_store::Store;

const ARTIFACT_PATH: &str = "/home/paya/Downloads/Programs/zoom_amd64.deb";
const BASE_TS: i64 = 1_760_647_493_000_000;

/// fake-Zoom style chain: artifact <- downloaded_as <- navigated_from <-
/// redirected_from <- redirected_from, plus installed_from -> pkg.install.
fn build_trace_case(path: &Path) {
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

    conn.execute(
        "INSERT INTO artifacts (path, sha256, size)
         VALUES (?1, 'e9a522c7deadbeef', 302788858)",
        [ARTIFACT_PATH],
    )
    .expect("insert artifact");
    let artifact_id: u64 = conn.last_insert_rowid() as u64;

    // ids 1..6 in insert order: download, nav1, nav2, nav3, install, pkg
    conn.execute(
        "INSERT INTO events
            (run_id, source_id, ts_utc, ts_precision, kind, subject, summary, confidence, dedup_key)
         VALUES
            (?1, 1, ?2, 'exact', 'web.download', 'https://cdn.zoom.us/zoom_amd64.deb',
             'downloaded zoom_amd64.deb', 'direct', 't1'),
            (?1, 1, ?3, 'exact', 'web.redirect', 'https://page-redirect.com/abc',
             'page-redirect.com -> cdn.zoom.us', 'heuristic', 't2'),
            (?1, 1, ?4, 'exact', 'web.redirect', 'https://ad.net/track',
             'ad.net -> page-redirect.com', 'heuristic', 't3'),
            (?1, 1, ?5, 'exact', 'web.visit', 'https://us09-web-zoom.com/j/1',
             'visited zoom lookalike', 'direct', 't4'),
            (?1, 2, ?6, 'exact', 'shell.command', 'sudo apt install ./zoom_amd64.deb',
             'sudo apt install ./zoom_amd64.deb', 'direct', 't5'),
            (?1, 3, ?7, 'second', 'pkg.install', 'zoom:amd64',
             'installed zoom package', 'derived', 't6')",
        rusqlite::params![
            run_id,
            BASE_TS + 300_000_000,
            BASE_TS + 297_000_000,
            BASE_TS + 240_000_000,
            BASE_TS + 180_000_000,
            BASE_TS + 96_000_000,
            BASE_TS + 426_000_000,
        ],
    )
    .expect("insert events");

    let event_id = |key: &str| -> u64 {
        conn.query_row("SELECT id FROM events WHERE dedup_key = ?1", [key], |r| {
            r.get(0)
        })
        .expect("event id")
    };

    let link = |src: (&str, u64), rel: &str, dst: (&str, u64), conf: &str| {
        conn.execute(
            "INSERT INTO links (src_type, src_id, rel, dst_type, dst_id, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![src.0, src.1, rel, dst.0, dst.1, conf],
        )
        .expect("insert link");
    };

    link(
        ("artifact", artifact_id),
        "downloaded_as",
        ("event", event_id("t1")),
        "direct",
    );
    link(
        ("event", event_id("t1")),
        "navigated_from",
        ("event", event_id("t2")),
        "heuristic",
    );
    link(
        ("event", event_id("t2")),
        "redirected_from",
        ("event", event_id("t3")),
        "heuristic",
    );
    link(
        ("event", event_id("t3")),
        "redirected_from",
        ("event", event_id("t4")),
        "heuristic",
    );
    link(
        ("artifact", artifact_id),
        "installed_from",
        ("event", event_id("t5")),
        "direct",
    );
    link(
        ("event", event_id("t5")),
        "pkg.install",
        ("event", event_id("t6")),
        "derived",
    );
}

fn new_shell(case_path: &Path) -> Shell {
    let ctx = egui::Context::default();
    let settings = Settings {
        last_view: Some("trace".to_string()),
        ..Settings::default()
    };
    let mut shell = Shell::new(case_path.to_path_buf(), Tz::Utc, settings, None, &ctx);
    shell.apply(Action::Go(View::Trace));
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
fn trace_chain_matches_cli_order_and_confidences() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_trace_case(&db);

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

    harness
        .state_mut()
        .apply(Action::RequestTrace(ARTIFACT_PATH.to_string()));
    pump_until(&mut harness, |s| s.model().trace.value().is_some());
    // let the loading -> loaded layout transition settle so queried rects stay stable
    harness.step();
    harness.step();

    let vm = harness.state().model().trace.value().expect("trace loaded");
    assert!(
        matches!(&vm.head, TraceHeadVm::Artifact { path, .. } if path.as_str() == ARTIFACT_PATH)
    );

    // the CLI order is the tree's row order; the GUI must not reorder or drop rows
    let expected: Vec<(String, Confidence)> = {
        let store = Store::open(&db).expect("open case");
        let tree = fp_correlate::trace::trace_tree(&store, ARTIFACT_PATH).expect("cli tree");
        tree.rows
            .iter()
            .map(|r| {
                let conf = match r.confidence.as_deref() {
                    Some("direct") => Confidence::Direct,
                    Some("derived") => Confidence::Derived,
                    _ => Confidence::Heuristic,
                };
                (r.rel.clone(), conf)
            })
            .collect()
    };
    let actual: Vec<(String, Confidence)> = vm
        .rows
        .iter()
        .map(|r| {
            (
                r.rel.as_str().to_string(),
                r.confidence.unwrap_or(Confidence::Heuristic),
            )
        })
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(
        actual
            .iter()
            .map(|(rel, _)| rel.as_str())
            .collect::<Vec<_>>(),
        [
            "downloaded_as",
            "navigated_from",
            "redirected_from",
            "redirected_from",
            "installed_from",
            "pkg.install",
        ]
    );

    // the two consecutive redirect hops render collapsed into one header…
    harness.get_by_label("2 redirects").click();
    // the toggle lands mid-frame; the items were already computed, so the
    // expanded nodes only exist in the next frame's tree
    harness.step();
    harness.step();
    // …and expanding them reveals the hidden hop (node text is the event subject)
    harness.get_by_label("https://ad.net/track");
    harness.get_by_label("installed zoom package");
}

#[test]
fn trace_text_view_equals_cli_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_trace_case(&db);

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

    harness
        .state_mut()
        .apply(Action::RequestTrace(ARTIFACT_PATH.to_string()));
    pump_until(&mut harness, |s| s.model().trace.value().is_some());
    harness.step();
    harness.step();

    harness.get_by_label("view: chain").click();
    harness.step();
    // reaching "copy text" proves the text view rendered; a non-ASCII arrow in the
    // display text would have panicked the headless font layout on the step above
    harness.get_by_label("copy text");

    let store = Store::open(&db).expect("open case");
    let time = |ts| fp_query::format_time(ts, "second", Tz::Utc, "%H:%M:%S");
    let cli = fp_correlate::render_trace(&store, ARTIFACT_PATH, &time).expect("cli trace");
    let vm = harness.state().model().trace.value().expect("trace loaded");
    let gui = render_trace_text(vm, &time);
    assert_eq!(gui, cli);
}
