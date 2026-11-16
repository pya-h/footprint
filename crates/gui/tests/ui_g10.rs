//! Headless UI + worker integration tests for the session story board (G10.1)
//! and session saved views (G10.2).

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use fp_gui::app::Shell;
use fp_gui::model::{Action, BoardExportFormat, ScopeAction, Selection, View};
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
                 'https://us09-web-zoom.com/evil\u{202e}fdp.exe', 'us09-web-zoom.com', 'direct', 'e1')",
        rusqlite::params![run_id],
    )
    .expect("insert event");
    conn.execute(
        "INSERT INTO state (run_id, kind, key, value_json)
         VALUES (?1, 'persist.systemd', 'zoom-helper.service',
                 '{\"exec\":\"/tmp/helper\",\"integrity\":\"unowned\",\"flags\":[\"unowned\"]}')",
        rusqlite::params![run_id],
    )
    .expect("insert state");
    conn.execute(
        "INSERT INTO iocs (type, value, note, added_utc)
         VALUES ('domain', 'us09-web-zoom.com', 'zoom-lookalike', 1760647000)",
        [],
    )
    .expect("insert ioc");
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
fn board_pins_reorders_annotates_inspects_and_exports() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_case(&db);

    let shell = new_shell(&db);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        );
    pump_until(&mut harness, |s| s.model().counts.value().is_some());

    // 1. Select event #1 and pin it from the Inspector.
    harness
        .state_mut()
        .apply(Action::Select(Selection::Event(1)));
    pump_until(&mut harness, |s| {
        s.model().detail.value().and_then(|d| d.as_ref()).is_some()
    });
    harness.step();
    harness.step();
    harness.get_by_label("Add to Board").click();
    harness.step();
    harness.step();
    assert_eq!(harness.state().model().board.len(), 1);
    harness.get_by_label("On Board");

    // 2. Pin the state record from State view's Inspector.
    harness.state_mut().apply(Action::Go(View::State));
    pump_until(&mut harness, |s| {
        s.model()
            .states
            .value()
            .is_some_and(|rows| !rows.is_empty())
    });
    let state_id = harness.state().model().states.value().expect("states")[0].rowid;
    harness
        .state_mut()
        .apply(Action::Select(Selection::State(state_id)));
    harness.step();
    harness.step();
    harness.get_by_label("Add to Board").click();
    harness.step();
    harness.step();
    assert_eq!(harness.state().model().board.len(), 2);

    // 3. Open the Board via shortcut `b`, move item #2 up, annotate item #1, and verify order.
    harness.key_press(egui::Key::B);
    harness.step();
    harness.step();
    harness.get_by_label("Board");
    harness.get_by_label("board (2)");
    harness.get_by_label("move up board item #2").click();
    harness.step();
    harness.step();
    assert_eq!(
        harness.state().model().board.items()[0].target,
        Selection::State(state_id)
    );
    let second_id = harness.state().model().board.items()[1].id;
    harness.state_mut().apply(Action::SetBoardNote {
        id: second_id,
        note: "initial lure <script>alert(1)</script>".to_string(),
    });
    harness.step();
    harness.step();

    // 4. Export as Markdown and HTML inside the tempdir, verifying 0600 + sanitization + CSP.
    harness.get_by_label("Export Board").click();
    harness.step();
    harness.step();
    harness.get_by_label_contains("exported 2 items to");

    harness.get_by_label("html").click();
    harness.step();
    harness.step();
    harness.get_by_label("Export Board").click();
    harness.step();
    harness.step();
    harness.get_by_label_contains("html)");

    let md_files: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext == BoardExportFormat::Markdown.ext())
        })
        .collect();
    assert_eq!(md_files.len(), 1);
    let md_body = std::fs::read_to_string(md_files[0].path()).expect("read md");
    assert!(!md_body.contains('\u{202e}'));
    assert!(md_body.contains("evilfdp.exe"));
    assert!(md_body.contains("initial lure"));

    let html_files: Vec<_> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext == BoardExportFormat::Html.ext())
        })
        .collect();
    assert_eq!(html_files.len(), 1);
    let html_path = html_files[0].path();
    let mode = std::fs::metadata(&html_path)
        .expect("stat html")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    let html_body = std::fs::read_to_string(&html_path).expect("read html");
    assert!(html_body.contains("default-src 'none'"));
    assert!(!html_body.contains("<script>"));
    assert!(html_body.contains("&lt;script&gt;"));

    // 5. Esc closes Board cleanly.
    harness.key_press(egui::Key::Escape);
    harness.step();
    harness.step();
    assert_eq!(harness.query_all_by_label("Board").count(), 0);
}

#[test]
fn saved_views_save_apply_undo_and_appear_in_palette() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("case.db");
    build_case(&db);

    let shell = new_shell(&db);
    let mut harness = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            |ui, state| {
                state.show_ui(ui);
            },
            shell,
        );
    pump_until(&mut harness, |s| s.model().counts.value().is_some());

    // Set a filter and open Timeline, then save the view via the scope bar popover.
    harness.state_mut().apply(Action::Go(View::Timeline));
    harness
        .state_mut()
        .apply(Action::Scope(ScopeAction::SetText(Some(
            "zoom".to_string(),
        ))));
    harness.step();
    harness.step();

    harness.get_by_label("save view").click();
    harness.step();
    harness.step();
    harness.get_by_label("Save").click();
    harness.step();
    harness.step();
    assert_eq!(harness.state().model().saved_views.len(), 1);
    let saved_name = harness.state().model().saved_views.list()[0].name.clone();
    harness.get_by_label("views (1)");

    // Clear scope and leave Timeline, then restore via the views menu.
    harness.state_mut().apply(Action::Scope(ScopeAction::Clear));
    harness.state_mut().apply(Action::Go(View::Dashboard));
    harness.step();
    harness.step();
    assert_eq!(harness.state().model().view(), View::Dashboard);
    assert!(harness.state().model().scope.is_empty());

    harness.get_by_label("views (1)").click();
    harness.step();
    harness.step();
    harness
        .get_by_label(&format!("{saved_name} (Timeline)"))
        .click();
    harness.step();
    harness.step();
    assert_eq!(harness.state().model().view(), View::Timeline);
    assert_eq!(
        harness.state().model().scope.filter().match_text.as_deref(),
        Some("zoom")
    );

    // Restoring a saved view pushes cleanly onto Scope's undo stack.
    harness.state_mut().apply(Action::UndoScope);
    harness.step();
    assert!(harness.state().model().scope.is_empty());

    // Ctrl+K palette exposes the saved view and restores it again (use Enter after filtering or accesskit click).
    harness.state_mut().apply(Action::Go(View::Hosts));
    harness.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::K);
    harness.step();
    harness.step();
    harness
        .get_by_label(&format!("view: {saved_name}"))
        .click_accesskit();
    harness.step();
    harness.step();
    assert_eq!(harness.state().model().view(), View::Timeline);
    assert_eq!(
        harness.state().model().scope.filter().match_text.as_deref(),
        Some("zoom")
    );
}
