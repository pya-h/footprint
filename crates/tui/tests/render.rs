use fp_fixtures::{SCENARIO_TZ, fake_zoom, local};
use fp_tui::{App, Theme};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn case_with_data() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    fake_zoom(dir.path()).unwrap();
    let db = dir.path().join("case.db");
    fp_query::run_collect(
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
    .unwrap();
    (dir, db)
}

fn screen(app: &App, theme: Theme, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| fp_tui::draw(frame, app, theme))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            out.push_str(buffer[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[test]
fn timeline_renders_at_80x24() {
    let (_dir, db) = case_with_data();
    let app = App::new(&db).unwrap();
    // the smallest useful terminal: tabs, filter bar and rows all fit
    let text = screen(&app, Theme { colors: false }, 80, 24);
    assert!(text.contains("timeline"), "{text}");
    assert!(text.contains("filter"), "{text}");
    assert!(text.contains("shell.command"), "{text}");
}

#[test]
fn timeline_row_text_renders_wide() {
    let (_dir, db) = case_with_data();
    let app = App::new(&db).unwrap();
    let text = screen(&app, Theme { colors: false }, 160, 40);
    // the XDM download row carries the source URL
    assert!(text.contains("cdn.zoom.us"), "{text}");
}

#[test]
fn no_color_disables_styling() {
    let (_dir, db) = case_with_data();
    let app = App::new(&db).unwrap();
    // flag every zoom.us row so the timeline shows the IOC marker
    {
        use fp_store::Store;
        let mut store = Store::open(&db).unwrap();
        store.insert_ioc("domain", "zoom.us", Some("flag")).unwrap();
    }
    drop(app);
    let app = App::new(&db).unwrap();
    // rows loaded after the IOC carry the hit markers
    assert!(app.rows.iter().any(|r| !r.ioc_hits.is_empty()));

    fn flagged_cell_fg(app: &App, theme: Theme) -> Option<ratatui::style::Color> {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| fp_tui::draw(frame, app, theme))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        buffer
            .content()
            .iter()
            .find(|cell| cell.symbol() == "\u{2691}")
            .map(|cell| cell.fg)
    }

    use ratatui::style::Color;
    // with colors on the marker is red; NO_COLOR renders everything unstyled
    assert_eq!(
        flagged_cell_fg(&app, Theme { colors: true }),
        Some(Color::Red)
    );
    assert_eq!(
        flagged_cell_fg(&app, Theme { colors: false }),
        Some(Color::Reset)
    );
}

#[test]
fn help_overlay_renders() {
    let (_dir, db) = case_with_data();
    let mut app = App::new(&db).unwrap();
    app.on_key(fp_tui::Key::Char('?')).unwrap();
    assert!(app.help);
    let text = screen(&app, Theme { colors: false }, 80, 24);
    assert!(text.contains("PgUp/PgDn"), "{text}");
    assert!(text.contains("filter bar"), "{text}");
}

#[test]
fn rows_render_in_the_zone_given_by_tz() {
    let (_dir, db) = case_with_data();
    let mut app = App::new(&db).unwrap();
    // what `footprint --tz +03:30 tui` sets; the dpkg line was written at 21:08:00 +03:30
    app.tz = fp_query::parse_tz("+03:30").unwrap();
    let text = screen(&app, Theme { colors: false }, 160, 60);
    assert!(text.contains("t:+03:30"), "{text}");
    assert!(text.contains("21:08:00"), "{text}");
}
