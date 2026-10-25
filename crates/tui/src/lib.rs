//! The interactive terminal UI. `footprint tui [-c case]`.

mod app;
mod ui;

pub use app::{App, Key, View};
pub use ui::{Theme, draw};

use anyhow::{Context, Result, bail};
use std::io::{IsTerminal, stdout};
use std::path::Path;
use std::time::Duration;

/// Runs the TUI until the user quits.
pub fn run(case: &Path) -> Result<()> {
    if !case.exists() {
        bail!(
            "case file {} not found; run `footprint collect` first",
            case.display()
        );
    }
    if !stdout().is_terminal() {
        bail!("`footprint tui` needs an interactive terminal");
    }

    let mut app = App::new(case)?;
    // NO_COLOR (https://no-color.org): any value disables styling
    let theme = ui::Theme {
        colors: std::env::var_os("NO_COLOR").is_none(),
    };

    crossterm::terminal::enable_raw_mode().context("enable raw mode")?;
    let mut stdout = stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)
        .context("enter alternate screen")?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    let mut terminal = ratatui::Terminal::new(backend).context("create terminal")?;

    let result = event_loop(&mut terminal, &mut app, theme);

    let restore = (|| -> Result<()> {
        crossterm::terminal::disable_raw_mode().context("disable raw mode")?;
        crossterm::execute!(
            terminal.backend_mut(),
            crossterm::terminal::LeaveAlternateScreen
        )
        .context("leave alternate screen")?;
        terminal.show_cursor().context("show cursor")?;
        Ok(())
    })();
    result.and(restore)
}

fn event_loop(
    terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
    theme: ui::Theme,
) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app, theme))?;
        // a timeout lets collect progress stream in while the UI idles
        if crossterm::event::poll(Duration::from_millis(100))? {
            let crossterm::event::Event::Key(key) = crossterm::event::read()? else {
                continue;
            };
            if let Some(key) = convert_key(key) {
                match app.on_key(key) {
                    Ok(true) => return Ok(()),
                    Ok(false) => {}
                    Err(e) => app.status = format!("error: {e:#}"),
                }
            }
        }
        if let Err(e) = app.poll_collect() {
            app.status = format!("error: {e:#}");
        }
    }
}

fn convert_key(key: crossterm::event::KeyEvent) -> Option<Key> {
    use crossterm::event::{KeyCode, KeyModifiers};
    match key.code {
        KeyCode::Char(c) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            Some(Key::Char(c))
        }
        KeyCode::Enter => Some(Key::Enter),
        KeyCode::Esc => Some(Key::Esc),
        KeyCode::Tab => Some(Key::Tab),
        KeyCode::BackTab => Some(Key::BackTab),
        KeyCode::Up => Some(Key::Up),
        KeyCode::Down => Some(Key::Down),
        KeyCode::PageUp => Some(Key::PageUp),
        KeyCode::PageDown => Some(Key::PageDown),
        KeyCode::Backspace => Some(Key::Backspace),
        _ => None,
    }
}
