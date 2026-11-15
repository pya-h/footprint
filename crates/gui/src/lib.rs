//! The desktop GUI (egui/eframe). The CLI builds it only with `--features gui`; this
//! crate is a leaf like `fp-tui` and holds no SQL — everything goes through `fp-query`.

pub mod app;
pub mod model;
pub mod settings;
pub mod theme;
pub mod vm;
pub mod widgets;
pub mod worker;

pub use theme::Theme;

use std::path::Path;

/// Opens the case in a window. Blocks until the window closes.
pub fn run(case: &Path, tz: fp_query::Tz) -> anyhow::Result<()> {
    if unsafe { libc::geteuid() } == 0 {
        anyhow::bail!(
            "refusing to run the GUI as root: run `sudo footprint collect` in a terminal \
             (the case file is handed to SUDO_UID), then open the GUI as your user"
        );
    }
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        anyhow::bail!(
            "no display: DISPLAY and WAYLAND_DISPLAY are both unset; the GUI needs a \
             desktop session — use `footprint tui` on this terminal instead"
        );
    }

    let (settings, settings_warning) = settings::Settings::load();
    let title = format!("footprint — {}", case.display());
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(&title)
            .with_inner_size([1100.0, 700.0])
            .with_min_inner_size([1100.0, 700.0]),
        ..Default::default()
    };
    let case = case.to_path_buf();
    eframe::run_native(
        "footprint",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            Ok(Box::new(app::Shell::new(
                case,
                tz,
                settings,
                settings_warning,
                &ctx,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}
