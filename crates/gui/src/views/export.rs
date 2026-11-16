//! Export dialog: write the current scope to html/csv/jsonl through
//! `fp_render::export_case`, on a background thread so big cases don't
//! freeze the UI.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use eframe::egui;

use crate::model::Action;
use crate::theme::Theme;

#[derive(Default)]
pub struct ExportDialog {
    pub open: bool,
    pub format: String,
    pub path: String,
    pub redact: bool,
    pub running: Option<Receiver<Result<String, String>>>,
    pub status: Option<Result<String, String>>,
}

impl ExportDialog {
    pub fn open_default(&mut self, case: &std::path::Path) {
        let stamp = chrono_now_stamp();
        let dir = case
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| ".".to_string());
        self.open = true;
        self.format = "html".to_string();
        self.path = format!("{dir}/footprint-export-{stamp}.html");
        self.redact = true;
        self.status = None;
    }
}

pub(crate) fn chrono_now_stamp() -> String {
    fp_query::format_time(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0),
        "exact",
        fp_query::Tz::Local,
        "%Y%m%d-%H%M%S",
    )
}

pub fn show_export_dialog(
    ui: &egui::Context,
    theme: &Theme,
    dialog: &mut ExportDialog,
    in_scope: Option<u64>,
    actions: &mut Vec<Action>,
) {
    if !dialog.open {
        return;
    }
    if let Some(rx) = &dialog.running {
        if let Ok(result) = rx.try_recv() {
            dialog.status = Some(result);
            dialog.running = None;
        } else {
            ui.request_repaint_after(std::time::Duration::from_millis(80));
        }
    }
    let mut keep_open = true;
    let mut close = false;
    egui::Window::new("Export scope")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .open(&mut keep_open)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("format:");
                for format in ["html", "csv", "jsonl"] {
                    if ui
                        .selectable_label(dialog.format == format, format)
                        .clicked()
                    {
                        dialog.format = format.to_string();
                        let base = dialog.path.rsplit_once('.').map(|(b, _)| b).unwrap_or("");
                        dialog.path = format!("{base}.{format}");
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("path:");
                ui.add(egui::TextEdit::singleline(&mut dialog.path).desired_width(420.0));
            });
            ui.checkbox(
                &mut dialog.redact,
                "redact secrets (commands keep their shape)",
            )
            .on_hover_text("passwords and keys inside commands become <redacted>");
            match in_scope {
                Some(count) => {
                    ui.label(
                        egui::RichText::new(format!("{} events in scope will be written", count))
                            .small()
                            .weak(),
                    );
                }
                None => {
                    ui.label(
                        egui::RichText::new("waiting for the in-scope count…")
                            .small()
                            .weak(),
                    );
                }
            }
            if let Some(status) = &dialog.status {
                match status {
                    Ok(msg) => {
                        ui.colored_label(theme.ok, msg);
                        if ui.small_button("copy path").clicked() {
                            ui.ctx().copy_text(dialog.path.clone());
                        }
                    }
                    Err(err) => {
                        ui.colored_label(theme.danger, err);
                    }
                }
            }
            ui.horizontal(|ui| {
                let busy = dialog.running.is_some();
                if ui.add_enabled(!busy, egui::Button::new("Export")).clicked() {
                    actions.push(Action::ExportScope {
                        path: PathBuf::from(dialog.path.trim()),
                        format: dialog.format.clone(),
                        redact: dialog.redact,
                    });
                }
                if busy {
                    ui.spinner();
                }
                if ui.button("close").clicked() {
                    close = true;
                }
            });
        });
    if !keep_open || close {
        dialog.open = false;
        dialog.running = None;
    }
}

/// Runs one export on a background thread; the dialog polls `rx`.
pub fn spawn_export(
    case: PathBuf,
    filter: fp_store::EventFilter,
    format: String,
    path: PathBuf,
    redact: bool,
    tz: fp_query::Tz,
) -> Receiver<Result<String, String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result =
            run_export(&case, &filter, &format, &path, redact, tz).map_err(|e| e.to_string());
        let _ = tx.send(result);
    });
    rx
}

fn run_export(
    case: &std::path::Path,
    filter: &fp_store::EventFilter,
    format: &str,
    path: &std::path::Path,
    redact: bool,
    tz: fp_query::Tz,
) -> anyhow::Result<String> {
    let store = fp_store::Store::open(case)?;
    let format: fp_render::ExportFormat = format.parse()?;
    let zone = match tz {
        fp_query::Tz::Local => "local".to_string(),
        fp_query::Tz::Utc => "UTC".to_string(),
        fp_query::Tz::Fixed(off) => off.to_string(),
    };
    let render =
        |ts: i64, precision: &str| fp_query::format_time(ts, precision, tz, "%Y-%m-%d %H:%M:%S");
    let time = fp_render::ExportTime {
        zone: &zone,
        format: &render,
    };
    let count = fp_render::export_case(&store, filter, format, path, redact, &time)?;
    Ok(format!(
        "exported {count} events to {} ({:?}{})",
        path.display(),
        format,
        if redact { ", redacted" } else { "" }
    ))
}
