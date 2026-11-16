//! Session-only story board: review pinned events/state/IOCs in narrative order,
//! annotate entries, jump back to the underlying record, and export as Markdown or HTML.

use std::path::{Path, PathBuf};

use eframe::egui;

use crate::model::{Action, BoardExportFormat, Model, target_label, view_for_selection};
use crate::theme::Theme;

#[derive(Default)]
pub struct BoardWindowState {
    pub open: bool,
    pub format: BoardExportFormat,
    pub path: String,
    pub status: Option<Result<String, String>>,
    /// Per-item buffered note text synced cleanly with `Model::board`.
    notes: std::collections::HashMap<u64, String>,
}

impl BoardWindowState {
    pub fn toggle(&mut self, case: &Path) {
        if self.open {
            self.open = false;
        } else {
            self.open_default(case);
        }
    }

    pub fn open_default(&mut self, case: &Path) {
        self.open = true;
        if self.path.is_empty() {
            self.path = default_path(case, self.format);
        }
        self.status = None;
    }

    pub fn close(&mut self) {
        self.open = false;
    }
}

fn default_path(case: &Path, format: BoardExportFormat) -> String {
    let stamp = crate::views::export::chrono_now_stamp();
    let dir = case
        .parent()
        .map(|p| p.display().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ".".to_string());
    format!("{dir}/footprint-board-{stamp}.{}", format.ext())
}

fn case_display_name(case: &Path) -> String {
    case.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| case.display().to_string())
}

pub fn show_board_window(
    ctx: &egui::Context,
    theme: &Theme,
    case: &Path,
    model: &Model,
    state: &mut BoardWindowState,
    actions: &mut Vec<Action>,
) {
    if !state.open {
        return;
    }
    let items = model.board.items();
    state
        .notes
        .retain(|id, _| items.iter().any(|item| item.id == *id));

    let mut keep_open = true;
    let mut close = false;
    let case_name = case_display_name(case);

    egui::Window::new("Board")
        .collapsible(false)
        .resizable(true)
        .default_size([640.0, 580.0])
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .open(&mut keep_open)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("{} pinned items (session only)", items.len()))
                        .small()
                        .color(theme.text_dim),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .add_enabled(!items.is_empty(), egui::Button::new("Clear board"))
                        .clicked()
                    {
                        actions.push(Action::ClearBoard);
                        state.notes.clear();
                    }
                });
            });
            ui.separator();

            if items.is_empty() {
                ui.add_space(18.0);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("No items pinned yet.").strong());
                    ui.label(
                        egui::RichText::new(
                            "Use 'Add to Board' in the Inspector or right-click a Timeline row / IOC hit to build a story.",
                        )
                        .small()
                        .weak(),
                    );
                });
                ui.add_space(18.0);
            } else {
                egui::ScrollArea::vertical()
                    .id_salt("board_items_scroll")
                    .max_height(280.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let count = items.len();
                        for (idx, item) in items.iter().enumerate() {
                            egui::Frame::group(ui.style())
                                .fill(theme.bg2)
                                .corner_radius(egui::CornerRadius::same(6))
                                .show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        let when = item.when.as_deref().unwrap_or("untimed");
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "#{} · {} · {when} · {}",
                                                idx + 1,
                                                item.kind.as_str(),
                                                target_label(&item.target)
                                            ))
                                            .monospace()
                                            .small()
                                            .color(theme.accent),
                                        );
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                let rm = ui.small_button("remove");
                                                rm.widget_info(|| {
                                                    egui::WidgetInfo::labeled(
                                                        egui::Role::Button,
                                                        true,
                                                        format!("remove board item #{}", idx + 1),
                                                    )
                                                });
                                                if rm.clicked() {
                                                    actions.push(Action::UnpinBoardItem(item.id));
                                                }

                                                let down = ui.add_enabled(
                                                    idx + 1 < count,
                                                    egui::Button::new("down").small(),
                                                );
                                                down.widget_info(|| {
                                                    egui::WidgetInfo::labeled(
                                                        egui::Role::Button,
                                                        idx + 1 < count,
                                                        format!("move down board item #{}", idx + 1),
                                                    )
                                                });
                                                if down.clicked() {
                                                    actions.push(Action::MoveBoardItem {
                                                        index: idx,
                                                        delta: 1,
                                                    });
                                                }

                                                let up = ui.add_enabled(
                                                    idx > 0,
                                                    egui::Button::new("up").small(),
                                                );
                                                up.widget_info(|| {
                                                    egui::WidgetInfo::labeled(
                                                        egui::Role::Button,
                                                        idx > 0,
                                                        format!("move up board item #{}", idx + 1),
                                                    )
                                                });
                                                if up.clicked() {
                                                    actions.push(Action::MoveBoardItem {
                                                        index: idx,
                                                        delta: -1,
                                                    });
                                                }

                                                if ui
                                                    .small_button("inspect")
                                                    .on_hover_text("jump to this item")
                                                    .clicked()
                                                {
                                                    actions.push(Action::Go(view_for_selection(
                                                        &item.target,
                                                    )));
                                                    actions.push(Action::Select(item.target.clone()));
                                                }
                                            },
                                        );
                                    });
                                    ui.label(egui::RichText::new(item.summary.as_str()).strong());
                                    if !item.detail.is_empty() {
                                        ui.label(
                                            egui::RichText::new(item.detail.as_str())
                                                .monospace()
                                                .small()
                                                .color(theme.text_dim),
                                        );
                                    }
                                    let note = state
                                        .notes
                                        .entry(item.id)
                                        .or_insert_with(|| item.note.clone());
                                    let note_resp = ui.add(
                                        egui::TextEdit::multiline(note)
                                            .desired_rows(2)
                                            .desired_width(f32::INFINITY)
                                            .hint_text("analyst note for this step…"),
                                    );
                                    if note_resp.changed() {
                                        actions.push(Action::SetBoardNote {
                                            id: item.id,
                                            note: note.clone(),
                                        });
                                    }
                                });
                            ui.add_space(4.0);
                        }
                    });
            }

            ui.separator();
            ui.horizontal(|ui| {
                ui.label("format:");
                for format in [BoardExportFormat::Markdown, BoardExportFormat::Html] {
                    if ui
                        .selectable_label(state.format == format, format.label())
                        .clicked()
                    {
                        state.format = format;
                        let base = state.path.rsplit_once('.').map(|(b, _)| b).unwrap_or("");
                        if !base.is_empty() {
                            state.path = format!("{base}.{}", format.ext());
                        } else {
                            state.path = default_path(case, format);
                        }
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("path:");
                ui.add(egui::TextEdit::singleline(&mut state.path).desired_width(440.0));
            });
            ui.horizontal(|ui| {
                if ui.button("Copy Markdown").clicked() {
                    let md = model.board.render_markdown(&case_name);
                    ui.ctx().copy_text(md);
                    state.status = Some(Ok("copied Markdown to clipboard".to_string()));
                }
                if ui.button("Export Board").clicked() {
                    let path = PathBuf::from(state.path.trim());
                    match model.board.write_export(&case_name, state.format, &path) {
                        Ok(count) => {
                            state.status = Some(Ok(format!(
                                "exported {count} items to {} ({})",
                                path.display(),
                                state.format.label()
                            )));
                        }
                        Err(err) => {
                            state.status = Some(Err(err));
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.set_min_height(20.0);
                if let Some(status) = &state.status {
                    match status {
                        Ok(msg) => {
                            ui.colored_label(theme.ok, msg);
                            if ui.small_button("copy path").clicked() {
                                ui.ctx().copy_text(state.path.clone());
                            }
                        }
                        Err(err) => {
                            ui.colored_label(theme.danger, err);
                        }
                    }
                }
            });
        });

    if !keep_open || close {
        state.close();
    }
}
