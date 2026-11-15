//! The desktop GUI (egui/eframe). The CLI builds it only with `--features gui`; this
//! crate is a leaf like `fp-tui` and holds no SQL — everything goes through `fp-query`.

pub mod theme;
pub mod widgets;
pub mod worker;

pub use theme::Theme;

use std::path::Path;

/// Opens the case in a window. Blocks until the window closes.
pub fn run(case: &Path, _tz: fp_query::Tz) -> anyhow::Result<()> {
    let store = fp_store::Store::open(case)?;
    let totals = format!(
        "{} events · {} sources",
        store.event_count()?,
        store.sources(None)?.len()
    );
    let title = format!("footprint — {}", case.display());

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(&title)
            .with_inner_size([900.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "footprint",
        options,
        Box::new(move |_cc| Ok(Box::new(Shell { title, totals }))),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(())
}

struct Shell {
    title: String,
    totals: String,
}

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut eframe::egui::Ui, _frame: &mut eframe::Frame) {
        eframe::egui::CentralPanel::default().show(ui, |ui| {
            ui.heading(&self.title);
            ui.label(&self.totals);
            ui.separator();
            ui.label("the interface is built in later milestones");
        });
    }
}
