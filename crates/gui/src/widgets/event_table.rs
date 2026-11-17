//! Virtualized timeline event table and Group-by pivot view.

use std::collections::{BTreeSet, HashSet};

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use fp_query::Tz;
use fp_query::overview::GroupBy;

use crate::model::{Action, LoadStatus, Model, ScopeAction, Selection, View};
use crate::settings::{Density, Settings};
use crate::theme::Theme;
use crate::vm::RowVm;
use crate::widgets::ViewCtx;
use crate::widgets::badge::{ConfidenceGlyph, StatusGlyph, confidence_disc, status_mark};
use crate::widgets::chip::thousands;
use crate::worker::BlockCache;
use crate::worker::cache::BLOCK_SIZE;

pub const ALL_COLUMNS: &[(&str, &str, f32)] = &[
    ("confidence", "", 22.0),
    ("time", "TIME", 122.0),
    ("source", "SOURCE", 120.0),
    ("kind", "KIND", 126.0),
    ("summary", "SUMMARY", 320.0),
    ("host", "HOST", 145.0),
    ("ioc", "IOC", 32.0),
];

#[derive(Default)]
pub struct TableState {
    pub needed_blocks: Vec<usize>,
    pub multi_selected: BTreeSet<u64>,
    pub focused_row: Option<usize>,
    pub scroll_to_focused: bool,
    pub select_after_load: Option<usize>,
    pub burst_folding: bool,
    pub expanded_bursts: HashSet<u64>,
}

pub fn show_timeline_table(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    cache: &mut BlockCache,
    settings: &mut Settings,
    state: &mut TableState,
    actions: &mut Vec<Action>,
) {
    let model = ctx.model;
    let theme = ctx.theme;
    let tz = ctx.tz;
    state.needed_blocks.clear();

    show_toolbar(ui, model, settings, theme, state, actions);
    ui.separator();

    if let Some(by) = model.group_by {
        show_group_pivot(ui, by, model, theme, tz, actions);
        return;
    }

    let Some(counts) = model.counts.value() else {
        ui.centered_and_justified(|ui| {
            ui.label(egui::RichText::new("loading timeline…").weak());
        });
        return;
    };
    let total_rows = usize::try_from(counts.in_scope).unwrap_or(usize::MAX);
    if total_rows == 0 {
        show_empty_scope(ui, model, theme, actions);
        return;
    }

    let row_h = match settings.density {
        Density::Compact => theme.row_compact,
        Density::Comfortable => theme.row_comfortable,
    };

    handle_keyboard_nav(ui, model, cache, total_rows, state, actions);

    let scope_hash = model.scope.hash();
    let reverse = model.reverse;
    if let Some(pending_idx) = state.select_after_load
        && let Some(row) = cache.row(scope_hash, reverse, pending_idx)
    {
        state.select_after_load = None;
        actions.push(Action::Select(Selection::Event(row.id)));
    }
    let quiet_threshold = quiet_gap_threshold(model);
    let cols = sanitized_columns(&settings.table.columns);
    let click_mods = ui.input(|i| i.modifiers);

    let mut visited_blocks = HashSet::new();
    let mut resized_widths: Vec<(String, f32)> = Vec::new();
    ui.scope(|ui| {
        ui.style_mut().interaction.selectable_labels = false;
        let mut builder = TableBuilder::new(ui)
            .striped(false)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center));

        if let Some(target_row) = model.scroll_to_row {
            builder = builder.scroll_to_row(
                target_row.min(total_rows.saturating_sub(1)),
                Some(egui::Align::Center),
            );
            state.focused_row = Some(target_row.min(total_rows.saturating_sub(1)));
            state.scroll_to_focused = false;
            actions.push(Action::ClearScrollTarget);
        } else if std::mem::take(&mut state.scroll_to_focused)
            && let Some(row_idx) = state.focused_row
        {
            builder = builder.scroll_to_row(row_idx.min(total_rows.saturating_sub(1)), None);
        }

        for col in &cols {
            let default_w = default_col_width(col, &settings.table.widths);
            let column = if col == "summary" {
                Column::remainder().at_least(160.0).clip(true)
            } else {
                Column::initial(default_w)
                    .at_least(20.0)
                    .resizable(true)
                    .clip(true)
            };
            builder = builder.column(column);
        }

        let primary_selected_id = match model.selection() {
            Selection::Event(id) => Some(id),
            _ => None,
        };

        builder
            .header(row_h, |mut header| {
                for col in &cols {
                    header.col(|ui| {
                        let actual_w = ui.max_rect().width();
                        if col != "summary"
                            && actual_w >= 20.0
                            && (actual_w - default_col_width(col, &settings.table.widths)).abs()
                                > 1.5
                        {
                            resized_widths.push((col.clone(), actual_w));
                        }
                        let title = col_header(col, tz);
                        ui.label(
                            egui::RichText::new(title)
                                .small()
                                .strong()
                                .color(theme.text_dim),
                        );
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, total_rows, |mut row_ui| {
                    let row_idx = row_ui.index();
                    let block_idx = row_idx / BLOCK_SIZE;
                    visited_blocks.insert(block_idx);
                    visited_blocks.insert(block_idx + 1);

                    let prev_row = row_idx
                        .checked_sub(1)
                        .and_then(|p| cache.row(scope_hash, reverse, p))
                        .cloned();
                    let cur_row = cache.row(scope_hash, reverse, row_idx).cloned();

                    let Some(row) = cur_row else {
                        for col in &cols {
                            row_ui.col(|ui| {
                                if col == "time" {
                                    ui.label(egui::RichText::new("…").weak());
                                }
                            });
                        }
                        return;
                    };

                    let is_selected = primary_selected_id == Some(row.id)
                        || state.multi_selected.contains(&row.id);
                    let has_ioc = !row.ioc_hits.is_empty();
                    row_ui.set_selected(is_selected);

                    let day_changed = day_boundary(prev_row.as_ref(), &row, tz);
                    let quiet_gap = quiet_gap_label(prev_row.as_ref(), &row, quiet_threshold);
                    let burst_count = if state.burst_folding {
                        contiguous_burst_len(cache, scope_hash, reverse, row_idx, &row)
                    } else {
                        0
                    };

                    for col in &cols {
                        row_ui.col(|ui| {
                            if has_ioc && !is_selected {
                                let r = ui.max_rect();
                                ui.painter()
                                    .rect_filled(r, 0.0, theme.ioc.linear_multiply(0.14));
                            }
                            if day_changed {
                                let r = ui.max_rect();
                                ui.painter().line_segment(
                                    [r.left_top(), r.right_top()],
                                    egui::Stroke::new(1.0, theme.border),
                                );
                            }
                            render_cell(
                                ui,
                                col,
                                &row,
                                CellFlags {
                                    day_changed,
                                    quiet_gap: quiet_gap.as_deref(),
                                    burst_count,
                                },
                                theme,
                                tz,
                            );
                        });
                    }

                    let row_resp = row_ui.response();
                    if row_resp.clicked() {
                        handle_row_click(
                            RowClickCtx {
                                row_idx,
                                mods: click_mods,
                                scope_hash,
                                reverse,
                            },
                            &row,
                            cache,
                            state,
                            actions,
                        );
                    }
                    row_resp.context_menu(|ui| {
                        show_row_context_menu(ui, &row, model, tz, actions);
                    });
                });
            });
    });

    for (col, w) in resized_widths {
        settings.table.widths.insert(col, w);
    }
    for block in visited_blocks {
        if !cache.has_block((scope_hash, reverse, block)) {
            state.needed_blocks.push(block);
        }
    }
    state.needed_blocks.sort_unstable();
}

fn show_empty_scope(ui: &mut egui::Ui, model: &Model, theme: &Theme, actions: &mut Vec<Action>) {
    ui.centered_and_justified(|ui| {
        ui.vertical_centered(|ui| {
            ui.label(
                egui::RichText::new("No events match this scope")
                    .strong()
                    .color(theme.text),
            );
            if let Some(hints) = model.empty_recovery.value()
                && !hints.is_empty()
            {
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new("Remove a filter chip to bring events back:")
                        .small()
                        .color(theme.text_dim),
                );
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    for hint in hints {
                        let text = format!(
                            "x {} (+{} events)",
                            hint.label,
                            thousands(hint.restored_count)
                        );
                        if ui.button(text).clicked() {
                            actions.push(Action::Scope(ScopeAction::Remove(hint.remove.clone())));
                        }
                    }
                });
            }
            if !model.scope.is_empty() {
                ui.add_space(6.0);
                if ui.small_button("Clear all filters").clicked() {
                    actions.push(Action::Scope(ScopeAction::Clear));
                }
            }
        });
    });
}

fn show_toolbar(
    ui: &mut egui::Ui,
    model: &Model,
    settings: &mut Settings,
    theme: &Theme,
    state: &mut TableState,
    actions: &mut Vec<Action>,
) {
    ui.horizontal_wrapped(|ui| {
        let sort_label = if model.reverse {
            "newest first"
        } else {
            "oldest first"
        };
        if ui
            .button(sort_label)
            .on_hover_text("toggle chronological sort order")
            .clicked()
        {
            actions.push(Action::SetReverse(!model.reverse));
        }

        let density_label = match settings.density {
            Density::Compact => "density: compact",
            Density::Comfortable => "density: comfortable",
        };
        if ui.button(density_label).clicked() {
            settings.density = match settings.density {
                Density::Compact => Density::Comfortable,
                Density::Comfortable => Density::Compact,
            };
        }

        ui.separator();
        let active_group = model.group_by;
        ui.label(egui::RichText::new("group by").color(theme.text_dim));
        let group_combo = egui::ComboBox::from_id_salt("timeline_group_by")
            .selected_text(group_by_label(active_group))
            .show_ui(ui, |ui| {
                for opt in [
                    None,
                    Some(GroupBy::Host),
                    Some(GroupBy::Source),
                    Some(GroupBy::Kind),
                    Some(GroupBy::Dir),
                    Some(GroupBy::Minute),
                ] {
                    if ui
                        .selectable_label(active_group == opt, group_by_label(opt))
                        .clicked()
                    {
                        actions.push(Action::SetGroupBy(opt));
                    }
                }
            });
        group_combo
            .response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "group by"));

        ui.menu_button("columns", |ui| {
            for &(id, title, _) in ALL_COLUMNS {
                let label = if title.is_empty() { id } else { title };
                let mut visible = settings.table.columns.iter().any(|c| c == id);
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut visible, label).changed() {
                        if visible {
                            settings.table.columns.push(id.to_string());
                        } else if settings.table.columns.len() > 1 {
                            settings.table.columns.retain(|c| c != id);
                        }
                    }
                    if let Some(pos) = settings.table.columns.iter().position(|c| c == id) {
                        if pos > 0
                            && ui
                                .small_button("^")
                                .on_hover_text("move column earlier")
                                .clicked()
                        {
                            settings.table.columns.swap(pos, pos - 1);
                        }
                        if pos + 1 < settings.table.columns.len()
                            && ui
                                .small_button("v")
                                .on_hover_text("move column later")
                                .clicked()
                        {
                            settings.table.columns.swap(pos, pos + 1);
                        }
                    }
                });
            }
        });

        ui.checkbox(&mut state.burst_folding, "mark bursts (5+)")
            .on_hover_text("tag runs of 5+ consecutive same kind & host rows in loaded window");

        if !state.multi_selected.is_empty() {
            ui.label(
                egui::RichText::new(format!("{} selected", state.multi_selected.len()))
                    .color(theme.accent)
                    .small(),
            );
            if ui.small_button("clear selection").clicked() {
                state.multi_selected.clear();
            }
        }
    });
}

#[derive(Clone, Copy)]
struct RowClickCtx {
    row_idx: usize,
    mods: egui::Modifiers,
    scope_hash: u64,
    reverse: bool,
}

#[derive(Clone, Copy)]
struct CellFlags<'a> {
    day_changed: bool,
    quiet_gap: Option<&'a str>,
    burst_count: usize,
}

fn render_cell(
    ui: &mut egui::Ui,
    col: &str,
    row: &RowVm,
    flags: CellFlags<'_>,
    theme: &Theme,
    tz: Tz,
) {
    match col {
        "confidence" => {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            confidence_disc(
                ui.painter(),
                rect.center(),
                4.0,
                ConfidenceGlyph::from(row.confidence),
                theme.text,
            );
        }
        "time" => {
            if row.untimed {
                ui.colored_label(theme.warn, "untimed")
                    .on_hover_text("order_only precision: no clock timestamp in source");
            } else {
                let pattern = if flags.day_changed {
                    "%d %b %H:%M:%S"
                } else {
                    "%H:%M:%S"
                };
                let label = fp_query::format_time(row.ts_utc, "exact", tz, pattern);
                ui.label(egui::RichText::new(label).monospace());
                if let Some(gap) = flags.quiet_gap {
                    ui.label(egui::RichText::new(gap).color(theme.text_dim).small());
                }
            }
        }
        "source" => {
            ui.label(row.source.as_str())
                .on_hover_text(row.source.as_str());
        }
        "kind" => {
            let (swatch, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter()
                .rect_filled(swatch, 2.0, theme.category_color(row.category));
            ui.label(row.kind.as_str());
            if flags.burst_count >= 5 {
                ui.label(
                    egui::RichText::new(format!("×{}", flags.burst_count))
                        .color(theme.accent)
                        .small(),
                );
            }
        }
        "summary" => {
            let text = row.display_text();
            let resp = ui.label(text);
            if !text.is_empty() {
                resp.on_hover_text(text);
            }
        }
        "host" => {
            if row.subject_host.is_empty() {
                ui.label(egui::RichText::new("–").color(theme.text_dim));
            } else {
                ui.label(row.subject_host.as_str())
                    .on_hover_text(row.subject_host.as_str());
            }
        }
        "ioc" if !row.ioc_hits.is_empty() => {
            let tip = row
                .ioc_hits
                .iter()
                .map(|h| h.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
            status_mark(
                ui.painter(),
                rect.center(),
                4.5,
                StatusGlyph::Ioc,
                theme.ioc,
            );
            resp.on_hover_text(tip);
        }
        _ => {}
    }
}

fn handle_row_click(
    click: RowClickCtx,
    row: &RowVm,
    cache: &mut BlockCache,
    state: &mut TableState,
    actions: &mut Vec<Action>,
) {
    if click.mods.shift
        && let Some(anchor) = state.focused_row
    {
        let lo = anchor.min(click.row_idx);
        let hi = anchor.max(click.row_idx);
        for idx in lo..=hi {
            if let Some(r) = cache.row(click.scope_hash, click.reverse, idx) {
                state.multi_selected.insert(r.id);
            }
        }
    } else if click.mods.ctrl {
        if !state.multi_selected.insert(row.id) {
            state.multi_selected.remove(&row.id);
        }
    } else {
        state.multi_selected.clear();
    }
    state.focused_row = Some(click.row_idx);
    actions.push(Action::Select(Selection::Event(row.id)));
}

fn handle_keyboard_nav(
    ui: &egui::Ui,
    model: &Model,
    cache: &mut BlockCache,
    total_rows: usize,
    state: &mut TableState,
    actions: &mut Vec<Action>,
) {
    if ui.ctx().egui_wants_keyboard_input() || total_rows == 0 {
        return;
    }
    let delta: Option<isize> = ui.input(|i| {
        if i.modifiers.any() {
            return None;
        }
        if i.key_pressed(egui::Key::ArrowDown) || i.key_pressed(egui::Key::J) {
            Some(1)
        } else if i.key_pressed(egui::Key::ArrowUp) || i.key_pressed(egui::Key::K) {
            Some(-1)
        } else if i.key_pressed(egui::Key::PageDown) {
            Some(15)
        } else if i.key_pressed(egui::Key::PageUp) {
            Some(-15)
        } else if i.key_pressed(egui::Key::Home) {
            Some(-(total_rows as isize))
        } else if i.key_pressed(egui::Key::End) {
            Some(total_rows as isize)
        } else {
            None
        }
    });
    let Some(step) = delta else {
        return;
    };
    let cur = state.focused_row.unwrap_or(0) as isize;
    let next = (cur + step).clamp(0, total_rows.saturating_sub(1) as isize) as usize;
    if Some(next) != state.focused_row {
        state.focused_row = Some(next);
        state.scroll_to_focused = true;
        let block = next / BLOCK_SIZE;
        if let Some(row) = cache.row(model.scope.hash(), model.reverse, next) {
            state.select_after_load = None;
            actions.push(Action::Select(Selection::Event(row.id)));
        } else {
            state.select_after_load = Some(next);
            if !cache.has_block((model.scope.hash(), model.reverse, block)) {
                state.needed_blocks.push(block);
            }
        }
    }
}

fn show_row_context_menu(
    ui: &mut egui::Ui,
    row: &RowVm,
    model: &Model,
    tz: Tz,
    actions: &mut Vec<Action>,
) {
    let host = row.subject_host.as_str();
    if ui
        .add_enabled(!host.is_empty(), egui::Button::new("Filter to this host"))
        .clicked()
    {
        actions.push(Action::Scope(ScopeAction::SetHosts(vec![host.to_string()])));
        ui.close();
    }
    if ui
        .add_enabled(!host.is_empty(), egui::Button::new("Exclude this host"))
        .clicked()
    {
        let mut exc = model.scope.filter().hosts_not;
        if !exc.iter().any(|h| h == host) {
            exc.push(host.to_string());
        }
        actions.push(Action::Scope(ScopeAction::SetHostsNot(exc)));
        ui.close();
    }
    if ui.button("Only this kind").clicked() {
        actions.push(Action::Scope(ScopeAction::SetKinds(vec![
            row.kind.as_str().to_string(),
        ])));
        ui.close();
    }
    if ui.button("Exclude this kind").clicked() {
        let mut exc = model.scope.filter().kinds_not;
        let kind = row.kind.as_str().to_string();
        if !exc.contains(&kind) {
            exc.push(kind);
        }
        actions.push(Action::Scope(ScopeAction::SetKindsNot(exc)));
        ui.close();
    }
    ui.separator();
    let target = Selection::Event(row.id);
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
                kind: row.kind.as_str().to_string(),
                when: (!row.untimed)
                    .then(|| fp_query::format_time(row.ts_utc, "second", tz, "%Y-%m-%d %H:%M:%S")),
                summary: if row.subject.is_empty() {
                    row.summary.as_str().to_string()
                } else {
                    row.subject.as_str().to_string()
                },
                detail: if row.subject_host.is_empty() {
                    row.source.as_str().to_string()
                } else {
                    row.subject_host.as_str().to_string()
                },
            }));
        }
        ui.close();
    }
    if ui.button("Trace").clicked() {
        actions.push(Action::Go(View::Trace));
        actions.push(Action::Select(Selection::Event(row.id)));
        actions.push(Action::RequestTrace(row.id.to_string()));
        ui.close();
    }
    if ui.button("Show in context").clicked() {
        actions.push(Action::ShowInContext(row.ts_utc, row.id));
        ui.close();
    }
    ui.separator();
    if ui.button("Copy subject").clicked() {
        ui.ctx().copy_text(row.subject.as_str().to_string());
        ui.close();
    }
    if ui.button("Copy summary").clicked() {
        ui.ctx().copy_text(row.summary.as_str().to_string());
        ui.close();
    }
    if ui.button("Copy ID").clicked() {
        ui.ctx().copy_text(row.id.to_string());
        ui.close();
    }
    if ui.button("Copy row as CLI").clicked() {
        let cli = if row.untimed {
            format!("footprint list --kind '{}' --untimed", row.kind.as_str())
        } else {
            format!(
                "footprint list --kind '{}' --since @{} --until @{}",
                row.kind.as_str(),
                row.ts_utc / 1_000_000,
                row.ts_utc / 1_000_000 + 1
            )
        };
        ui.ctx().copy_text(cli);
        ui.close();
    }
}

fn show_group_pivot(
    ui: &mut egui::Ui,
    by: GroupBy,
    model: &Model,
    theme: &Theme,
    tz: Tz,
    actions: &mut Vec<Action>,
) {
    match model.groups.status(model.group_gen) {
        LoadStatus::Loading => {
            ui.centered_and_justified(|ui| {
                ui.label(egui::RichText::new("computing group summary…").weak());
            });
            return;
        }
        LoadStatus::Error(err) => {
            ui.colored_label(theme.danger, err);
            return;
        }
        LoadStatus::Ready | LoadStatus::Stale => {}
    }

    let Some(groups) = model.groups.value() else {
        return;
    };
    if groups.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(egui::RichText::new("no groups match the current scope").weak());
        });
        return;
    }

    let max_count = groups.iter().map(|g| g.count).max().unwrap_or(0).max(1);
    let total_count: u64 = groups.iter().map(|g| g.count).sum();

    ui.scope(|ui| {
        ui.style_mut().interaction.selectable_labels = false;
        TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .column(Column::remainder().at_least(180.0).clip(true))
            .column(Column::initial(80.0).clip(true))
            .column(Column::initial(150.0).clip(true))
            .column(Column::initial(150.0).clip(true))
            .column(Column::initial(120.0).clip(true))
            .header(22.0, |mut h| {
                for title in ["VALUE", "COUNT", "FIRST", "LAST", "SHARE"] {
                    h.col(|ui| {
                        ui.label(
                            egui::RichText::new(title)
                                .small()
                                .strong()
                                .color(theme.text_dim),
                        );
                    });
                }
            })
            .body(|body| {
                body.rows(22.0, groups.len(), |mut row_ui| {
                    let g = &groups[row_ui.index()];
                    let pct = if total_count > 0 {
                        (g.count as f64 / total_count as f64) * 100.0
                    } else {
                        0.0
                    };

                    row_ui.col(|ui| {
                        ui.label(g.label.as_str())
                            .on_hover_text("double-click to filter to this group");
                    });
                    row_ui.col(|ui| {
                        ui.label(thousands(g.count));
                    });
                    row_ui.col(|ui| {
                        let text = g
                            .first_ts
                            .map(|ts| fp_query::format_time(ts, "exact", tz, "%d %b %H:%M:%S"))
                            .unwrap_or_else(|| "–".to_string());
                        ui.label(egui::RichText::new(text).monospace());
                    });
                    row_ui.col(|ui| {
                        let text = g
                            .last_ts
                            .map(|ts| fp_query::format_time(ts, "exact", tz, "%d %b %H:%M:%S"))
                            .unwrap_or_else(|| "–".to_string());
                        ui.label(egui::RichText::new(text).monospace());
                    });
                    row_ui.col(|ui| {
                        ui.horizontal(|ui| {
                            let (bar_rect, _) =
                                ui.allocate_exact_size(egui::vec2(60.0, 8.0), egui::Sense::hover());
                            ui.painter().rect_filled(bar_rect, 2.0, theme.bg2);
                            let fill_w =
                                (g.count as f32 / max_count as f32).clamp(0.03, 1.0) * 60.0;
                            let fill =
                                egui::Rect::from_min_size(bar_rect.min, egui::vec2(fill_w, 8.0));
                            ui.painter().rect_filled(fill, 2.0, theme.accent);
                            ui.label(egui::RichText::new(format!("{pct:.1}%")).small());
                        });
                    });

                    if row_ui.response().double_clicked() {
                        drill_down_group(by, g, actions);
                    }
                });
            });
    });
}

fn drill_down_group(by: GroupBy, row: &crate::vm::GroupRowVm, actions: &mut Vec<Action>) {
    let val = row.filter_key().to_string();
    match by {
        GroupBy::Host => {
            if val != "(none)" {
                actions.push(Action::Scope(ScopeAction::SetHosts(vec![val])));
            }
        }
        GroupBy::Source => {
            actions.push(Action::Scope(ScopeAction::SetSources(vec![val])));
        }
        GroupBy::Kind => {
            actions.push(Action::Scope(ScopeAction::SetKinds(vec![val])));
        }
        GroupBy::Dir => {
            actions.push(Action::Scope(ScopeAction::SetText(Some(val))));
        }
        GroupBy::Minute => {
            if let Some(first) = row.first_ts {
                actions.push(Action::Scope(ScopeAction::SetWindow {
                    since: Some(first),
                    until: Some(first.saturating_add(60_000_000)),
                }));
            }
        }
    }
    actions.push(Action::SetGroupBy(None));
}

fn group_by_label(by: Option<GroupBy>) -> &'static str {
    match by {
        None => "none (rows)",
        Some(GroupBy::Host) => "host",
        Some(GroupBy::Source) => "source",
        Some(GroupBy::Kind) => "kind",
        Some(GroupBy::Dir) => "directory",
        Some(GroupBy::Minute) => "minute",
    }
}

fn sanitized_columns(cols: &[String]) -> Vec<String> {
    let valid: Vec<String> = cols
        .iter()
        .filter(|c| ALL_COLUMNS.iter().any(|(id, _, _)| *id == c.as_str()))
        .cloned()
        .collect();
    if valid.is_empty() {
        ALL_COLUMNS
            .iter()
            .map(|(id, _, _)| (*id).to_string())
            .collect()
    } else {
        valid
    }
}

fn default_col_width(col: &str, saved: &std::collections::BTreeMap<String, f32>) -> f32 {
    if let Some(&w) = saved.get(col) {
        return w.max(20.0);
    }
    ALL_COLUMNS
        .iter()
        .find(|(id, _, _)| *id == col)
        .map(|(_, _, w)| *w)
        .unwrap_or(100.0)
}

fn col_header(col: &str, tz: Tz) -> String {
    match col {
        "confidence" => String::new(),
        "time" => match tz {
            Tz::Local => "TIME (local)".to_string(),
            Tz::Utc => "TIME (UTC)".to_string(),
            Tz::Fixed(off) => format!("TIME ({off})"),
        },
        "source" => "SOURCE".to_string(),
        "kind" => "KIND".to_string(),
        "summary" => "SUMMARY".to_string(),
        "host" => "HOST".to_string(),
        "ioc" => "IOC".to_string(),
        other => other.to_string(),
    }
}

fn day_boundary(prev: Option<&RowVm>, cur: &RowVm, tz: Tz) -> bool {
    let Some(prev) = prev else {
        return false;
    };
    if prev.untimed || cur.untimed {
        return false;
    }
    fp_query::format_time(prev.ts_utc, "exact", tz, "%Y-%m-%d")
        != fp_query::format_time(cur.ts_utc, "exact", tz, "%Y-%m-%d")
}

fn quiet_gap_threshold(model: &Model) -> i64 {
    let f = model.scope.filter();
    let span = match (f.since, f.until) {
        (Some(s), Some(u)) => u.saturating_sub(s).abs(),
        _ => match model.bounds.value().copied().flatten() {
            Some((s, e)) => e.saturating_sub(s).abs(),
            None => 3_600_000_000,
        },
    };
    (span / 48).max(5 * 60 * 1_000_000)
}

fn quiet_gap_label(prev: Option<&RowVm>, cur: &RowVm, threshold: i64) -> Option<String> {
    let prev = prev?;
    if prev.untimed || cur.untimed {
        return None;
    }
    let gap = prev.ts_utc.abs_diff(cur.ts_utc);
    if gap <= threshold as u64 {
        return None;
    }
    let secs = gap / 1_000_000;
    let text = if secs >= 86_400 {
        format!("+{}d", secs / 86_400)
    } else if secs >= 3_600 {
        format!("+{}h", secs / 3_600)
    } else if secs >= 60 {
        format!("+{}m", secs / 60)
    } else {
        format!("+{secs}s")
    };
    Some(text)
}

fn contiguous_burst_len(
    cache: &mut BlockCache,
    scope_hash: u64,
    reverse: bool,
    row_idx: usize,
    row: &RowVm,
) -> usize {
    let mut count = 1usize;
    let mut back = row_idx;
    while back > 0 && count < 100 {
        back -= 1;
        let Some(prev) = cache.row(scope_hash, reverse, back) else {
            break;
        };
        if prev.kind == row.kind && prev.subject_host == row.subject_host {
            count += 1;
        } else {
            break;
        }
    }
    let mut fwd = row_idx + 1;
    while count < 100 {
        let Some(next) = cache.row(scope_hash, reverse, fwd) else {
            break;
        };
        if next.kind == row.kind && next.subject_host == row.subject_host {
            count += 1;
            fwd += 1;
        } else {
            break;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_gap_formats_readable_units() {
        let a = RowVm::from_row(&fp_query::TimelineRow {
            id: 1,
            run_id: 1,
            ts_utc: 0,
            ts_precision: "exact".into(),
            kind: "shell.command".into(),
            source_kind: "bash".into(),
            app: None,
            profile: None,
            subject: None,
            subject_host: None,
            summary: None,
            confidence: "direct".into(),
            ioc_hits: Vec::new(),
        });
        let b = RowVm::from_row(&fp_query::TimelineRow {
            id: 2,
            run_id: 1,
            ts_utc: 1_200_000_000,
            ts_precision: "exact".into(),
            kind: "shell.command".into(),
            source_kind: "bash".into(),
            app: None,
            profile: None,
            subject: None,
            subject_host: None,
            summary: None,
            confidence: "direct".into(),
            ioc_hits: Vec::new(),
        });
        assert_eq!(
            quiet_gap_label(Some(&a), &b, 300_000_000),
            Some("+20m".into())
        );
    }
}
