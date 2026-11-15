//! Hosts view: SQL-sorted and searchable table of subject hosts (capped at 500)
//! with NEW-in-window flags, category mix bars, Timeline drill-down, and Add-as-IOC.

use eframe::egui;
use egui_extras::{Column, TableBuilder};
use fp_query::category::Category;
use fp_query::overview::HostSort;

use crate::model::{Action, LoadStatus, Model, ScopeAction, View};
use crate::theme::Theme;
use crate::vm::HostSummaryVm;
use crate::widgets::ViewCtx;
use crate::widgets::chip::thousands;

pub const HOSTS_CAP: usize = 500;

#[derive(Default)]
pub struct HostsState {
    pub search: String,
    pub submitted_search: String,
    pub selected_host: Option<String>,
}

pub fn show_hosts(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    state: &mut HostsState,
    actions: &mut Vec<Action>,
) {
    render_toolbar(ui, ctx, state, actions);
    render_ioc_status(ui, ctx, actions);
    ui.add_space(4.0);

    let model = ctx.model;
    let status = model.hosts.status(model.hosts_gen);
    let Some(rows) = model.hosts.value() else {
        match status {
            LoadStatus::Error(err) => {
                ui.colored_label(ctx.theme.danger, format!("Failed to load hosts: {err}"));
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("querying hosts...");
                });
            }
        }
        return;
    };

    if rows.is_empty() {
        ui.label(
            egui::RichText::new("No subject hosts match the current scope and search filter.")
                .color(ctx.theme.text_dim),
        );
        return;
    }

    if rows.len() >= HOSTS_CAP {
        ui.label(
            egui::RichText::new(format!(
                "showing top {HOSTS_CAP} — narrow scope or search to refine"
            ))
            .small()
            .color(ctx.theme.warn),
        );
    }

    render_hosts_table(ui, ctx, rows, state, actions);
}

fn render_toolbar(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    state: &mut HostsState,
    actions: &mut Vec<Action>,
) {
    let model = ctx.model;
    ui.horizontal_wrapped(|ui| {
        ui.heading("Hosts");
        ui.add_space(8.0);

        ui.label("Search host:");
        let search_resp = ui.add(
            egui::TextEdit::singleline(&mut state.search)
                .desired_width(220.0)
                .hint_text("substring match..."),
        );
        let enter_pressed =
            search_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if search_resp.changed() || enter_pressed {
            let trimmed = state.search.trim().to_string();
            if trimmed != state.submitted_search {
                state.submitted_search = trimmed;
                actions.push(Action::RequestHostsQuery);
            }
        }
        if !state.search.is_empty() && ui.small_button("clear search").clicked() {
            state.search.clear();
            if !state.submitted_search.is_empty() {
                state.submitted_search.clear();
                actions.push(Action::RequestHostsQuery);
            }
        }

        ui.add_space(8.0);
        ui.label("Sort:");
        for (sort, label) in [
            (HostSort::EventsDesc, "Events"),
            (HostSort::HostAsc, "Host"),
            (HostSort::FirstInScopeAsc, "First seen"),
            (HostSort::LastInScopeDesc, "Last seen"),
        ] {
            if ui
                .selectable_label(model.host_sort == sort, label)
                .clicked()
            {
                actions.push(Action::SetHostSort(sort));
            }
        }

        if matches!(model.hosts.status(model.hosts_gen), LoadStatus::Stale) {
            ui.spinner();
        }
    });
}

fn render_ioc_status(ui: &mut egui::Ui, ctx: &ViewCtx<'_>, actions: &mut Vec<Action>) {
    let Some(status) = &ctx.model.ioc_status else {
        return;
    };
    ui.add_space(4.0);
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

fn render_hosts_table(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    rows: &[HostSummaryVm],
    state: &mut HostsState,
    actions: &mut Vec<Action>,
) {
    let model = ctx.model;
    let theme = ctx.theme;
    let active_window = active_window_bounds(model);
    let ctrl_held = ui.input(|i| i.modifiers.ctrl);
    let row_h = 24.0;
    let max_host_events = rows.iter().map(|r| r.events).max().unwrap_or(1).max(1);

    ui.scope(|ui| {
        ui.style_mut().interaction.selectable_labels = false;
        let builder = TableBuilder::new(ui)
            .striped(false)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(160.0).at_least(100.0).resizable(true).clip(true)) // HOST
            .column(Column::initial(46.0).at_least(40.0).clip(true))                  // NEW
            .column(Column::initial(64.0).at_least(50.0).resizable(true).clip(true))  // EVENTS
            .column(Column::initial(108.0).at_least(80.0).resizable(true).clip(true)) // FIRST
            .column(Column::initial(108.0).at_least(80.0).resizable(true).clip(true)) // LAST
            .column(Column::initial(100.0).at_least(76.0).resizable(true).clip(true)) // KINDS
            .column(Column::remainder().at_least(80.0).clip(true))                    // SOURCES
            .column(Column::initial(46.0).at_least(40.0).clip(true));                 // IOC

        builder
            .header(row_h, |mut header| {
                sort_header_cell(&mut header, "HOST", HostSort::HostAsc, model.host_sort, theme, actions);
                plain_header_cell(&mut header, "NEW", theme);
                sort_header_cell(&mut header, "EVENTS", HostSort::EventsDesc, model.host_sort, theme, actions);
                sort_header_cell(&mut header, "FIRST SEEN", HostSort::FirstInScopeAsc, model.host_sort, theme, actions);
                sort_header_cell(&mut header, "LAST SEEN", HostSort::LastInScopeDesc, model.host_sort, theme, actions);
                plain_header_cell(&mut header, "KINDS", theme);
                plain_header_cell(&mut header, "SOURCES", theme);
                plain_header_cell(&mut header, "IOC", theme);
            })
            .body(|body| {
                body.rows(row_h, rows.len(), |mut row_ui| {
                    let row = &rows[row_ui.index()];
                    let key = row.host_key();
                    let is_selected = state.selected_host.as_deref() == Some(key);
                    row_ui.set_selected(is_selected);

                    row_ui.col(|ui| {
                        let link = ui
                            .link(row.host.as_str())
                            .on_hover_text("Click: add host chip to Timeline | Ctrl+click: exclude | Double-click row: only this host");
                        if link.clicked() {
                            if ctrl_held {
                                emit_exclude_host(model, key, actions);
                            } else {
                                emit_add_host_chip(model, key, actions);
                                actions.push(Action::Go(View::Timeline));
                            }
                        }
                        attach_host_context_menu(&link, model, key, actions);
                    });

                    row_ui.col(|ui| {
                        if row.is_new_in_window(active_window) {
                            ui.label(egui::RichText::new("[NEW]").small().strong().color(theme.warn));
                        }
                    });

                    row_ui.col(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.monospace(thousands(row.events));
                        });
                    });

                    row_ui.col(|ui| {
                        ui.monospace(fmt_opt_ts(row.first_in_scope, ctx.tz));
                    });

                    row_ui.col(|ui| {
                        ui.monospace(fmt_opt_ts(row.last_in_scope, ctx.tz));
                    });

                    row_ui.col(|ui| {
                        paint_category_mix_bar(ui, row, max_host_events, theme);
                        ui.add_space(4.0);
                        ui.label(
                            egui::RichText::new(row.distinct_kinds.to_string())
                                .small()
                                .color(theme.text_dim),
                        );
                    });

                    row_ui.col(|ui| {
                        let text = if row.sources.is_empty() {
                            "-".to_string()
                        } else {
                            row.sources
                                .iter()
                                .map(|s| s.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        };
                        ui.add(egui::Label::new(egui::RichText::new(text).small()).truncate());
                    });

                    row_ui.col(|ui| {
                        if row.has_ioc {
                            ui.label(egui::RichText::new("[IOC]").small().strong().color(theme.ioc));
                        } else {
                            ui.label(egui::RichText::new("-").small().color(theme.text_dim));
                        }
                    });

                    let resp = row_ui.response();
                    if resp.clicked() {
                        state.selected_host = Some(key.to_string());
                    }
                    if resp.double_clicked() {
                        actions.push(Action::Scope(ScopeAction::SetHosts(vec![key.to_string()])));
                        actions.push(Action::Go(View::Timeline));
                    }
                    attach_host_context_menu(&resp, model, key, actions);
                });
            });
    });
}

fn sort_header_cell(
    header: &mut egui_extras::TableRow<'_, '_>,
    title: &str,
    sort: HostSort,
    active: HostSort,
    theme: &Theme,
    actions: &mut Vec<Action>,
) {
    header.col(|ui| {
        let label = if active == sort {
            format!("{title} *")
        } else {
            title.to_string()
        };
        if ui
            .add(
                egui::Button::new(egui::RichText::new(label).small().strong().color(
                    if active == sort {
                        theme.accent
                    } else {
                        theme.text_dim
                    },
                ))
                .frame(false),
            )
            .clicked()
        {
            actions.push(Action::SetHostSort(sort));
        }
    });
}

fn plain_header_cell(header: &mut egui_extras::TableRow<'_, '_>, title: &str, theme: &Theme) {
    header.col(|ui| {
        ui.label(
            egui::RichText::new(title)
                .small()
                .strong()
                .color(theme.text_dim),
        );
    });
}

fn attach_host_context_menu(
    resp: &egui::Response,
    model: &Model,
    host_key: &str,
    actions: &mut Vec<Action>,
) {
    resp.context_menu(|ui| {
        if ui.button("Add host chip -> Timeline").clicked() {
            emit_add_host_chip(model, host_key, actions);
            actions.push(Action::Go(View::Timeline));
            ui.close();
        }
        if ui.button("Only this host -> Timeline").clicked() {
            actions.push(Action::Scope(ScopeAction::SetHosts(vec![
                host_key.to_string(),
            ])));
            actions.push(Action::Go(View::Timeline));
            ui.close();
        }
        if ui.button("Exclude host").clicked() {
            emit_exclude_host(model, host_key, actions);
            ui.close();
        }
        if ui.button("Copy host").clicked() {
            ui.ctx().copy_text(host_key.to_string());
            ui.close();
        }
        ui.separator();
        if ui.button("Add as IOC").clicked() {
            let ioc_type = fp_query::iocs::detect_ioc_type(host_key).to_string();
            actions.push(Action::AddIoc {
                ioc_type,
                value: host_key.to_string(),
            });
            ui.close();
        }
    });
}

fn emit_add_host_chip(model: &Model, host: &str, actions: &mut Vec<Action>) {
    let f = model.scope.filter();
    let mut hosts = f.hosts;
    let mut hosts_not = f.hosts_not;
    hosts_not.retain(|h| h != host);
    if !hosts.iter().any(|h| h == host) {
        hosts.push(host.to_string());
    }
    actions.push(Action::Scope(ScopeAction::SetHostFilter {
        hosts,
        hosts_not,
    }));
}

fn emit_exclude_host(model: &Model, host: &str, actions: &mut Vec<Action>) {
    let f = model.scope.filter();
    let mut hosts = f.hosts;
    let mut hosts_not = f.hosts_not;
    hosts.retain(|h| h != host);
    if !hosts_not.iter().any(|h| h == host) {
        hosts_not.push(host.to_string());
    }
    actions.push(Action::Scope(ScopeAction::SetHostFilter {
        hosts,
        hosts_not,
    }));
}

fn paint_category_mix_bar(
    ui: &mut egui::Ui,
    row: &HostSummaryVm,
    max_host_events: u64,
    theme: &Theme,
) {
    let total: u64 = row.category_counts.iter().sum();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(76.0, 10.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, theme.bg0);
    if total == 0 {
        return;
    }
    let fill_ratio = (row.events as f32 / max_host_events as f32).clamp(0.15, 1.0);
    let usable_w = (rect.width() * fill_ratio).max(4.0);
    let mut x = rect.left();
    for (idx, cat) in Category::ALL.iter().enumerate() {
        let count = row.category_counts[idx];
        if count == 0 {
            continue;
        }
        let seg_w = ((count as f32 / total as f32) * usable_w).max(1.5);
        let right = (x + seg_w).min(rect.right());
        let seg = egui::Rect::from_min_max(
            egui::pos2(x, rect.top() + 1.0),
            egui::pos2(right, rect.bottom() - 1.0),
        );
        ui.painter()
            .rect_filled(seg, 1.0, theme.category_color(*cat));
        x = right;
        if x >= rect.right() {
            break;
        }
    }
}

fn fmt_opt_ts(ts: Option<i64>, tz: fp_query::Tz) -> String {
    match ts {
        Some(v) => fp_query::format_time(v, "exact", tz, "%m-%d %H:%M:%S"),
        None => "-".into(),
    }
}

fn active_window_bounds(model: &Model) -> Option<(i64, i64)> {
    let filter = model.scope.filter();
    let case_bounds = model.bounds.value().copied().flatten();
    match (filter.since, filter.until, case_bounds) {
        (Some(s), Some(u), _) => Some((s.min(u), s.max(u))),
        (Some(s), None, Some((_, ce))) => Some((s, ce.max(s))),
        (None, Some(u), Some((cs, _))) => Some((cs.min(u), u)),
        _ => None,
    }
}
