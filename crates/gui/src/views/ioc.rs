//! IOC view: indicators with link-based hit counts, the hits for the selected
//! IOC (or all), and add/import/remove dialogs that write on the worker's
//! writer thread.

use eframe::egui;

use crate::model::{Action, LoadStatus, Selection, View};
use crate::theme::Theme;
use crate::vm::{IocHitVm, IocSummaryVm};
use crate::widgets::ViewCtx;
use crate::widgets::chip::thousands;

#[derive(Default)]
pub struct IocViewState {
    pub dialog: Option<IocDialog>,
    /// the remove button arms itself once, then fires on the second click
    pub pending_remove: Option<u64>,
}

pub enum IocDialog {
    Add {
        value: String,
        note: String,
        error: Option<String>,
    },
    Import {
        content: String,
        error: Option<String>,
    },
}

pub fn show_iocs(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    state: &mut IocViewState,
    actions: &mut Vec<Action>,
) {
    render_toolbar(ui, state, actions);
    if let Some(status) = &ctx.model.ioc_status {
        ui.horizontal(|ui| {
            match status {
                Ok(msg) => ui.colored_label(ctx.theme.ok, msg),
                Err(err) => ui.colored_label(ctx.theme.danger, format!("IOC error: {err}")),
            };
            if ui.small_button("dismiss").clicked() {
                actions.push(Action::DismissIocStatus);
            }
        });
    }
    ui.add_space(4.0);

    let model = ctx.model;
    let status = model.iocs.status(model.generation);
    let Some(iocs) = model.iocs.value() else {
        match status {
            LoadStatus::Error(err) => {
                ui.colored_label(ctx.theme.danger, format!("Failed to load IOCs: {err}"));
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("reading indicators...");
                });
            }
        }
        return;
    };

    let selected_id = match ctx.model.selection() {
        Selection::Ioc(id) => Some(id),
        _ => None,
    };
    ui.horizontal(|ui| {
        render_ioc_list(ui, ctx.theme, iocs, selected_id, state, actions);
        ui.separator();
        render_hits(ui, ctx, actions);
    });

    if let Some(dialog) = state.dialog.as_mut() {
        let mut keep = true;
        render_dialog(ui.ctx(), ctx.theme, dialog, &mut keep, actions);
        if !keep {
            state.dialog = None;
        }
    }
}

fn render_toolbar(ui: &mut egui::Ui, state: &mut IocViewState, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Indicators").strong());
        ui.add_space(8.0);
        if ui.button("+ Add").clicked() {
            state.dialog = Some(IocDialog::Add {
                value: String::new(),
                note: String::new(),
                error: None,
            });
        }
        if ui.button("Import…").clicked() {
            state.dialog = Some(IocDialog::Import {
                content: String::new(),
                error: None,
            });
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("show all hits as scope")
                .on_hover_text("set the scope to ioc-only and open the timeline")
                .clicked()
            {
                actions.push(Action::Scope(crate::model::ScopeAction::SetIocOnly(true)));
                actions.push(Action::Go(View::Timeline));
            }
        });
    });
}

fn render_ioc_list(
    ui: &mut egui::Ui,
    theme: &Theme,
    iocs: &[IocSummaryVm],
    selected_id: Option<u64>,
    state: &mut IocViewState,
    actions: &mut Vec<Action>,
) {
    ui.vertical(|ui| {
        ui.set_width(340.0);
        if iocs.is_empty() {
            ui.label(egui::RichText::new("no indicators yet — add or import some").weak());
            return;
        }
        for ioc in iocs {
            let selected = selected_id == Some(ioc.id);
            ui.horizontal(|ui| {
                let label = format!(
                    "{}  {}   {} hits",
                    ioc.ioc_type.as_str(),
                    ioc.value.as_str(),
                    thousands(ioc.hits)
                );
                let resp = ui.selectable_label(
                    selected,
                    egui::RichText::new(label)
                        .monospace()
                        .color(if ioc.hits > 0 { theme.ioc } else { theme.text }),
                );
                if resp.clicked() {
                    actions.push(Action::Select(Selection::Ioc(ioc.id)));
                }
                resp.on_hover_text(ioc.note.as_ref().map(|n| n.as_str()).unwrap_or(""));

                let armed = state.pending_remove == Some(ioc.id);
                let button = egui::Button::new(if armed { "sure?" } else { "×" }).small();
                if ui
                    .add(button)
                    .on_hover_text("remove this indicator and its hits")
                    .clicked()
                {
                    if armed {
                        state.pending_remove = None;
                        actions.push(Action::RemoveIoc(ioc.id));
                    } else {
                        state.pending_remove = Some(ioc.id);
                    }
                }
            });
        }
    });
}

fn render_hits(ui: &mut egui::Ui, ctx: &ViewCtx<'_>, actions: &mut Vec<Action>) {
    let model = ctx.model;
    ui.vertical(|ui| {
        ui.set_min_width(420.0);
        let selected = matches!(model.selection(), Selection::Ioc(_));
        let title = if selected { "HITS" } else { "HITS (all IOCs)" };
        ui.label(
            egui::RichText::new(title)
                .small()
                .strong()
                .color(ctx.theme.text_dim),
        );
        let Some(hits) = model.ioc_hits.value() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("reading hits...");
            });
            return;
        };
        if hits.is_empty() {
            ui.label(egui::RichText::new("no hits — the case matches none of these").weak());
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for hit in hits {
                    render_hit_row(ui, ctx.theme, hit, actions);
                }
            });
    });
}

fn render_hit_row(ui: &mut egui::Ui, theme: &Theme, hit: &IocHitVm, actions: &mut Vec<Action>) {
    let label = format!(
        "{} · {} · {}   (matched: {})",
        hit.target_kind.as_str(),
        hit.target_id,
        shorten(hit.target_label.as_str(), 60),
        hit.ioc_value.as_str()
    );
    let resp = ui
        .selectable_label(
            false,
            egui::RichText::new(label).monospace().color(theme.text),
        )
        .on_hover_text(
            hit.why
                .as_ref()
                .map(|w| w.as_str())
                .unwrap_or("click to inspect the matched event"),
        );
    if resp.clicked() && hit.target_type.as_str() == "event" {
        actions.push(Action::Select(Selection::Event(hit.target_id)));
    }
}

fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn render_dialog(
    ctx: &egui::Context,
    theme: &Theme,
    dialog: &mut IocDialog,
    open: &mut bool,
    actions: &mut Vec<Action>,
) {
    let title = match dialog {
        IocDialog::Add { .. } => "Add indicator",
        IocDialog::Import { .. } => "Import indicators",
    };
    // the window chrome (X button) and the dialog's own buttons close separately
    let mut chrome_open = true;
    let mut close = false;
    egui::Window::new(title)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .open(&mut chrome_open)
        .show(ctx, |ui| {
            match dialog {
                IocDialog::Add { value, note, error } => {
                    ui.label(
                        "Value (domain, ip, cidr, sha256, path or url — the type is detected):",
                    );
                    ui.add(egui::TextEdit::singleline(value).desired_width(360.0));
                    ui.label("Note (optional):");
                    ui.add(egui::TextEdit::singleline(note).desired_width(360.0));
                    if let Some(err) = error {
                        ui.colored_label(theme.danger, err);
                    }
                    let detected = fp_query::iocs::detect_ioc_type(value.trim());
                    ui.label(
                        egui::RichText::new(format!("detected type: {detected}"))
                            .small()
                            .weak(),
                    );
                    if ui.button("add").clicked() && !value.trim().is_empty() {
                        actions.push(Action::AddIoc {
                            ioc_type: detected.to_string(),
                            value: value.trim().to_string(),
                        });
                        close = true;
                    }
                }
                IocDialog::Import { content, error } => {
                    ui.label("One per line: `type value [note]`, csv, or a bare value:");
                    ui.add(egui::TextEdit::multiline(content).desired_width(420.0));
                    if let Some(err) = error {
                        ui.colored_label(theme.danger, err);
                    }
                    if ui.button("import").clicked() && !content.trim().is_empty() {
                        actions.push(Action::ImportIocs(content.clone()));
                        close = true;
                    }
                }
            }
            if ui.button("cancel").clicked() {
                close = true;
            }
        });
    *open = chrome_open && !close;
}
