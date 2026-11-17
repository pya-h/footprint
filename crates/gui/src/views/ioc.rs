//! IOC view: indicators with link-based hit counts, the hits for the selected
//! IOC (or all), and add/import/remove dialogs that write on the worker's
//! writer thread.

use eframe::egui;
use egui_extras::{Size, StripBuilder};

use crate::model::{Action, LoadStatus, Selection, View};
use crate::theme::Theme;
use crate::vm::{IocHitVm, IocSummaryVm};
use crate::widgets::chip::thousands;
use crate::widgets::{ViewCtx, kit};

#[derive(Default)]
pub struct IocViewState {
    pub dialog: Option<IocDialog>,
    /// the remove button arms itself once, then fires on the second click
    pub pending_remove: Option<u64>,
    /// which IOC filters the right-hand hits panel; survives clicking a hit row to inspect its event
    pub selected_ioc: Option<u64>,
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

    if let Selection::Ioc(id) = ctx.model.selection()
        && iocs.iter().any(|i| i.id == id)
    {
        state.selected_ioc = Some(id);
    }
    if let Some(id) = state.selected_ioc
        && !iocs.iter().any(|i| i.id == id)
    {
        state.selected_ioc = None;
    }
    let selected_id = state.selected_ioc;
    // a strip gives each pane the full remaining height; a scroll area inside `horizontal` is one row tall
    let list_width = (ui.available_width() * 0.4).clamp(260.0, 420.0);
    StripBuilder::new(ui)
        .size(Size::exact(list_width))
        .size(Size::remainder().at_least(160.0))
        .horizontal(|mut strip| {
            strip.cell(|ui| render_ioc_list(ui, ctx.theme, iocs, selected_id, state, actions));
            strip.cell(|ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: 12,
                        ..Default::default()
                    })
                    .show(ui, |ui| render_hits(ui, ctx, state, actions));
            });
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
        if iocs.is_empty() {
            ui.label(egui::RichText::new("no indicators yet — add or import some").weak());
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("ioc_list")
            .auto_shrink([false, false])
            .show(ui, |ui| {
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
                            if selected {
                                state.selected_ioc = None;
                                actions.push(Action::Select(Selection::None));
                            } else {
                                state.selected_ioc = Some(ioc.id);
                                actions.push(Action::Select(Selection::Ioc(ioc.id)));
                            }
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
                                if state.selected_ioc == Some(ioc.id) {
                                    state.selected_ioc = None;
                                }
                                actions.push(Action::RemoveIoc(ioc.id));
                            } else {
                                state.pending_remove = Some(ioc.id);
                            }
                        }
                    });
                }
            });
    });
}

fn render_hits(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    state: &mut IocViewState,
    actions: &mut Vec<Action>,
) {
    let model = ctx.model;
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            let title = if state.selected_ioc.is_some() {
                "HITS"
            } else {
                "HITS (all IOCs)"
            };
            ui.label(
                egui::RichText::new(title)
                    .small()
                    .strong()
                    .color(ctx.theme.text_dim),
            );
            if state.selected_ioc.is_some() && ui.small_button("show all").clicked() {
                state.selected_ioc = None;
            }
        });
        let Some(hits) = model.ioc_hits.value() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("reading hits...");
            });
            return;
        };
        if hits.is_empty() {
            kit::empty_state(
                ui,
                ctx.theme,
                if state.selected_ioc.is_some() {
                    "no hits for this IOC"
                } else {
                    "no IOC hits in this case"
                },
                "collect more evidence or add different indicators",
            );
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("ioc_hits_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (idx, hit) in hits.iter().enumerate() {
                    render_hit_row(ui, ctx.theme, model, (idx, hit), actions);
                    ui.add_space(4.0);
                }
            });
    });
}

fn render_hit_row(
    ui: &mut egui::Ui,
    theme: &Theme,
    model: &crate::model::Model,
    (idx, hit): (usize, &IocHitVm),
    actions: &mut Vec<Action>,
) {
    let label = format!(
        "{} · {} · {}   (matched: {})",
        hit.target_kind.as_str(),
        hit.target_id,
        hit.target_label.as_str(),
        hit.ioc_value.as_str()
    );
    let default_hint = match hit.target_type.as_str() {
        "event" => "click to inspect the matched event",
        "state" => "click to inspect the matched state record",
        "artifact" => "click to trace this matched artifact",
        _ => "matched IOC hit",
    };
    let card_id = ui.scope_id().with(("ioc_hit", idx));
    let card = kit::card_frame(theme, kit::hover_amount(ui, card_id, theme))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(hit.target_kind.as_str())
                        .small()
                        .strong()
                        .color(theme.accent),
                );
                ui.label(
                    egui::RichText::new(format!("#{}", hit.target_id))
                        .small()
                        .color(theme.text_dim),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(hit.ioc_value.as_str())
                                .small()
                                .color(theme.ioc),
                        )
                        .truncate(),
                    );
                    ui.label(egui::RichText::new("matched").small().color(theme.text_dim));
                });
            });
            let font = egui::FontId::monospace(theme.base_font_size - 0.5);
            let subject =
                kit::middle_ellipsis(ui, hit.target_label.as_str(), &font, ui.available_width());
            ui.add(
                egui::Label::new(egui::RichText::new(subject).font(font).color(theme.text))
                    .selectable(false),
            );
        });
    let resp = card
        .response
        .interact(egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(hit.why.as_ref().map(|w| w.as_str()).unwrap_or(default_hint));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &label));
    kit::remember_hover(ui, card_id, resp.hovered());
    let selection = match hit.target_type.as_str() {
        "event" => Some(Selection::Event(hit.target_id)),
        "state" => i64::try_from(hit.target_id).ok().map(Selection::State),
        _ => None,
    };
    if resp.clicked() {
        if let Some(sel) = &selection {
            actions.push(Action::Select(sel.clone()));
        } else if hit.target_type.as_str() == "artifact" && !hit.target_label.as_str().is_empty() {
            actions.push(Action::Go(View::Trace));
            actions.push(Action::RequestTrace(hit.target_label.as_str().to_string()));
        }
    }
    if let Some(target) = selection {
        resp.context_menu(|ui| {
            let on_board = model.board.contains(&target);
            let board_label = if on_board {
                "Remove from Board"
            } else {
                "Add to Board"
            };
            if ui.button(board_label).clicked() {
                if on_board {
                    actions.push(Action::UnpinBoardTarget(target));
                } else {
                    actions.push(Action::PinToBoard(crate::model::BoardDraft {
                        target,
                        kind: hit.target_kind.as_str().to_string(),
                        when: None,
                        summary: hit.target_label.as_str().to_string(),
                        detail: format!("matched IOC: {}", hit.ioc_value.as_str()),
                    }));
                }
                ui.close();
            }
        });
    }
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
                        let note_trimmed = note.trim();
                        actions.push(Action::AddIoc {
                            ioc_type: detected.to_string(),
                            value: value.trim().to_string(),
                            note: (!note_trimmed.is_empty()).then(|| note_trimmed.to_string()),
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
