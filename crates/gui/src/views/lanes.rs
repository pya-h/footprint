//! Lanes view: swimlane timeline grouped by Source or Category, showing
//! individual events (<= 5,000 in window) or per-lane density bars (> 5,000),
//! with synchronized cursor line, brushing/zoom, and an explicit Untimed lane.

use std::collections::BTreeMap;

use eframe::egui;
use fp_query::category::Category;
use fp_query::overview::LaneMode;

use crate::model::{Action, LoadStatus, ScopeAction, Selection, View};
use crate::theme::Theme;
use crate::vm::{LanesDataVm, RowVm};
use crate::widgets::ViewCtx;
use crate::widgets::badge::confidence_disc;
use crate::widgets::chip::thousands;
use crate::widgets::strip::{scale_count, ts_to_x, x_to_ts, zoom_window};

const LABEL_COL_W: f32 = 168.0;
const COUNT_COL_W: f32 = 64.0;
const LANE_HEIGHT: f32 = 30.0;
const AXIS_HEIGHT: f32 = 24.0;
const CLICK_DRAG_THRESHOLD_PX: f32 = 4.0;
const DOT_HIT_RADIUS_PX: f32 = 8.0;

pub struct LanesState {
    pub sqrt_scale: bool,
    pub untimed_open: bool,
    pub last_width_px: usize,
    drag_anchor_ts: Option<i64>,
    drag_preview: Option<(i64, i64)>,
}

impl Default for LanesState {
    fn default() -> Self {
        Self {
            sqrt_scale: true,
            untimed_open: true,
            last_width_px: 640,
            drag_anchor_ts: None,
            drag_preview: None,
        }
    }
}

pub fn show_lanes(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    state: &mut LanesState,
    actions: &mut Vec<Action>,
) {
    render_toolbar(ui, ctx, state, actions);
    ui.add_space(4.0);

    let model = ctx.model;
    let status = model.lanes.status(model.lanes_gen);
    let Some(data) = model.lanes.value() else {
        match status {
            LoadStatus::Error(err) => {
                ui.colored_label(ctx.theme.danger, format!("Failed to load lanes: {err}"));
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("loading lanes...");
                });
            }
        }
        return;
    };

    if data.truncated_to_density {
        render_truncation_banner(ui, ctx, data.total_timed);
    }

    render_axis_and_lanes(ui, ctx, data, state, actions);
    ui.add_space(6.0);
    render_untimed_lane(ui, ctx, data, state, actions);
}

fn render_toolbar(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    state: &mut LanesState,
    actions: &mut Vec<Action>,
) {
    let model = ctx.model;
    let filter = model.scope.filter();
    ui.horizontal_wrapped(|ui| {
        ui.heading("Lanes");
        ui.add_space(8.0);
        ui.label("Group by:");
        if ui
            .selectable_label(model.lanes_mode == LaneMode::Source, "Source")
            .clicked()
        {
            actions.push(Action::SetLanesMode(LaneMode::Source));
        }
        if ui
            .selectable_label(model.lanes_mode == LaneMode::Category, "Category")
            .clicked()
        {
            actions.push(Action::SetLanesMode(LaneMode::Category));
        }

        ui.add_space(8.0);
        let scale_label = if state.sqrt_scale {
            "scale: sqrt"
        } else {
            "scale: linear"
        };
        if ui.small_button(scale_label).clicked() {
            state.sqrt_scale = !state.sqrt_scale;
        }

        if let (Some(s), Some(u)) = (filter.since, filter.until) {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(format!(
                    "{} -> {}",
                    fp_query::format_time(s, "exact", ctx.tz, "%m-%d %H:%M:%S"),
                    fp_query::format_time(u, "exact", ctx.tz, "%m-%d %H:%M:%S"),
                ))
                .small()
                .color(ctx.theme.text_dim),
            );
            if ui.small_button("reset window").clicked() {
                actions.push(Action::Scope(ScopeAction::SetWindow {
                    since: None,
                    until: None,
                }));
            }
        } else {
            ui.label(
                egui::RichText::new(
                    "drag on lanes to zoom · Ctrl+wheel to zoom · double-click to reset",
                )
                .small()
                .color(ctx.theme.text_dim),
            );
        }

        if matches!(model.lanes.status(model.lanes_gen), LoadStatus::Stale) {
            ui.spinner();
        }
    });
}

fn render_truncation_banner(ui: &mut egui::Ui, ctx: &ViewCtx<'_>, total_timed: u64) {
    egui::Frame::group(ui.style())
        .fill(ctx.theme.bg2)
        .stroke(egui::Stroke::new(1.0, ctx.theme.warn))
        .corner_radius(egui::CornerRadius::same(4))
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Density view ({} timed events) — zoom in (<= 5,000 events) to see individual events",
                    thousands(total_timed)
                ))
                .small()
                .color(ctx.theme.warn),
            );
        });
    ui.add_space(4.0);
}

fn render_axis_and_lanes(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    data: &LanesDataVm,
    state: &mut LanesState,
    actions: &mut Vec<Action>,
) {
    let win_start = data.window.start_utc;
    let win_end = data.window.end_utc.max(win_start.saturating_add(1));

    let total_w = ui.available_width();
    let track_w = (total_w - LABEL_COL_W - COUNT_COL_W - 16.0).max(120.0);
    state.last_width_px = track_w.round() as usize;

    // Synchronized pointer hover across all tracks via X coordinate lookup.
    let pointer_pos = ui.ctx().pointer_hover_pos();
    let hover_ts = ui
        .horizontal(|ui| {
            ui.allocate_exact_size(egui::vec2(LABEL_COL_W, AXIS_HEIGHT), egui::Sense::hover());
            let (axis_rect, _) =
                ui.allocate_exact_size(egui::vec2(track_w, AXIS_HEIGHT), egui::Sense::hover());
            paint_time_axis(ui, axis_rect, win_start, win_end, ctx);

            let hover_ts = pointer_pos
                .filter(|p| p.x >= axis_rect.left() && p.x <= axis_rect.right())
                .map(|p| x_to_ts(p.x, axis_rect, win_start, win_end));
            if let Some(ts) = hover_ts {
                let x = ts_to_x(ts, axis_rect, win_start, win_end);
                ui.painter().line_segment(
                    [
                        egui::pos2(x, axis_rect.top()),
                        egui::pos2(x, axis_rect.bottom()),
                    ],
                    egui::Stroke::new(1.0, ctx.theme.accent),
                );
            }
            hover_ts
        })
        .inner;

    let cursor_text = match hover_ts {
        Some(ts) => format!(
            "Cursor: {}",
            fp_query::format_time(ts, "exact", ctx.tz, "%Y-%m-%d %H:%M:%S")
        ),
        None => "Cursor: -".to_string(),
    };
    let cursor_color = if hover_ts.is_some() {
        ctx.theme.accent
    } else {
        ctx.theme.text_dim
    };
    ui.label(
        egui::RichText::new(cursor_text)
            .small()
            .monospace()
            .color(cursor_color),
    );

    let scroll_h = (ui.available_height() - 120.0).max(180.0);
    egui::ScrollArea::vertical()
        .id_salt("lanes_scroll")
        .max_height(scroll_h)
        .show(ui, |ui| {
            if data.truncated_to_density {
                render_density_lanes(
                    ui,
                    ctx,
                    data,
                    track_w,
                    (win_start, win_end),
                    hover_ts,
                    state,
                    actions,
                );
            } else {
                render_individual_lanes(
                    ui,
                    ctx,
                    data,
                    track_w,
                    (win_start, win_end),
                    hover_ts,
                    state,
                    actions,
                );
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn render_density_lanes(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    data: &LanesDataVm,
    track_w: f32,
    bounds: (i64, i64),
    hover_ts: Option<i64>,
    state: &mut LanesState,
    actions: &mut Vec<Action>,
) {
    if data.density_lanes.is_empty() {
        ui.label(egui::RichText::new("No timed events in window.").color(ctx.theme.text_dim));
        return;
    }
    let global_max = data
        .density_lanes
        .iter()
        .flat_map(|l| l.buckets.iter().copied())
        .max()
        .unwrap_or(1)
        .max(1);

    for lane in &data.density_lanes {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(LABEL_COL_W, LANE_HEIGHT),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_size(egui::vec2(LABEL_COL_W, LANE_HEIGHT));
                    ui.add(egui::Label::new(lane.label.as_str()).truncate());
                },
            );
            let (rect, resp) = ui.allocate_exact_size(
                egui::vec2(track_w, LANE_HEIGHT),
                egui::Sense::click_and_drag(),
            );
            paint_lane_bg(ui, rect, ctx.theme, state.drag_preview, bounds);
            paint_density_bars(
                ui,
                rect,
                &data.bucket_bounds,
                &lane.buckets,
                global_max,
                bounds,
                ctx.theme.category_color(lane.category_hint),
                state.sqrt_scale,
            );
            if let Some(ts) = hover_ts {
                let cx = ts_to_x(ts, rect, bounds.0, bounds.1);
                paint_cursor_line(ui, rect, cx, ctx.theme);
            }
            handle_track_window_input(ui, &resp, rect, bounds, ctx, state, actions);
            ui.add_sized(
                egui::vec2(COUNT_COL_W, LANE_HEIGHT),
                egui::Label::new(
                    egui::RichText::new(thousands(lane.total))
                        .small()
                        .monospace(),
                ),
            );
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn render_individual_lanes(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    data: &LanesDataVm,
    track_w: f32,
    bounds: (i64, i64),
    hover_ts: Option<i64>,
    state: &mut LanesState,
    actions: &mut Vec<Action>,
) {
    let grouped = group_events_for_lanes(&data.events, data.mode);
    if grouped.is_empty() {
        ui.label(egui::RichText::new("No timed events in window.").color(ctx.theme.text_dim));
        return;
    }

    let selected_id = match ctx.model.inspected() {
        Selection::Event(id) => Some(id),
        _ => None,
    };

    for (label, rows) in grouped {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(LABEL_COL_W, LANE_HEIGHT),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_size(egui::vec2(LABEL_COL_W, LANE_HEIGHT));
                    ui.add(egui::Label::new(label.as_str()).truncate());
                },
            );
            let (rect, resp) = ui.allocate_exact_size(
                egui::vec2(track_w, LANE_HEIGHT),
                egui::Sense::click_and_drag(),
            );
            paint_lane_bg(ui, rect, ctx.theme, state.drag_preview, bounds);

            // Baseline across the track.
            let y_mid = rect.center().y;
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left(), y_mid),
                    egui::pos2(rect.right(), y_mid),
                ],
                egui::Stroke::new(1.0, ctx.theme.border),
            );

            for ev in &rows {
                let x = ts_to_x(ev.ts_utc, rect, bounds.0, bounds.1);
                let center = egui::pos2(x, y_mid);
                let color = ctx.theme.category_color(ev.category);
                if selected_id == Some(ev.id) {
                    ui.painter().circle_stroke(
                        center,
                        6.5,
                        egui::Stroke::new(1.5, ctx.theme.accent),
                    );
                }
                if !ev.ioc_hits.is_empty() {
                    ui.painter()
                        .circle_stroke(center, 5.2, egui::Stroke::new(1.2, ctx.theme.ioc));
                }
                confidence_disc(ui.painter(), center, 3.6, ev.confidence.into(), color);
            }

            if let Some(ts) = hover_ts {
                let cx = ts_to_x(ts, rect, bounds.0, bounds.1);
                paint_cursor_line(ui, rect, cx, ctx.theme);
            }

            let hovered_ev = resp
                .hover_pos()
                .and_then(|pos| nearest_event(&rows, pos.x, rect, bounds));
            if let Some(ev) = hovered_ev {
                egui::Tooltip::always_open(
                    ui.ctx().clone(),
                    ui.layer_id(),
                    resp.id.with("ev_tip"),
                    egui::PopupAnchor::Pointer,
                )
                .show(|ui| {
                    ui.label(fp_query::format_time(
                        ev.ts_utc,
                        if ev.untimed { "order_only" } else { "exact" },
                        ctx.tz,
                        "%Y-%m-%d %H:%M:%S",
                    ));
                    ui.monospace(format!("{} · {}", ev.kind.as_str(), ev.source.as_str()));
                    ui.label(ev.display_text());
                    if !ev.ioc_hits.is_empty() {
                        ui.colored_label(
                            ctx.theme.ioc,
                            format!("[IOC] {}", ev.ioc_hits[0].as_str()),
                        );
                    }
                });
            }

            let mut clicked_event = false;
            if resp.clicked()
                && let Some(pos) = resp.interact_pointer_pos()
                && let Some(ev) = nearest_event(&rows, pos.x, rect, bounds)
            {
                actions.push(Action::Select(Selection::Event(ev.id)));
                clicked_event = true;
            }
            if !clicked_event {
                handle_track_window_input(ui, &resp, rect, bounds, ctx, state, actions);
            }

            ui.add_sized(
                egui::vec2(COUNT_COL_W, LANE_HEIGHT),
                egui::Label::new(
                    egui::RichText::new(thousands(rows.len() as u64))
                        .small()
                        .monospace(),
                ),
            );
        });
    }
}

fn group_events_for_lanes<'a>(
    events: &'a [RowVm],
    mode: LaneMode,
) -> Vec<(String, Vec<&'a RowVm>)> {
    let mut map: BTreeMap<String, Vec<&'a RowVm>> = BTreeMap::new();
    match mode {
        LaneMode::Source => {
            for ev in events {
                map.entry(ev.source.as_str().to_string())
                    .or_default()
                    .push(ev);
            }
            let mut list: Vec<(String, Vec<&'a RowVm>)> = map.into_iter().collect();
            list.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
            list
        }
        LaneMode::Category => {
            let mut by_cat: [Vec<&'a RowVm>; Category::ALL.len()] =
                std::array::from_fn(|_| Vec::new());
            for ev in events {
                by_cat[ev.category as usize].push(ev);
            }
            let mut list = Vec::new();
            for (idx, cat) in Category::ALL.iter().enumerate() {
                if !by_cat[idx].is_empty() {
                    list.push((cat.label().to_string(), std::mem::take(&mut by_cat[idx])));
                }
            }
            list
        }
    }
}

fn nearest_event<'a>(
    rows: &[&'a RowVm],
    pointer_x: f32,
    rect: egui::Rect,
    bounds: (i64, i64),
) -> Option<&'a RowVm> {
    let mut best: Option<(f32, &'a RowVm)> = None;
    for &ev in rows {
        let x = ts_to_x(ev.ts_utc, rect, bounds.0, bounds.1);
        let dist = (x - pointer_x).abs();
        if dist <= DOT_HIT_RADIUS_PX && best.is_none_or(|(best_d, _)| dist < best_d) {
            best = Some((dist, ev));
        }
    }
    best.map(|(_, ev)| ev)
}

fn paint_time_axis(ui: &egui::Ui, rect: egui::Rect, start: i64, end: i64, ctx: &ViewCtx<'_>) {
    let painter = ui.painter_at(rect);
    painter.line_segment(
        [
            egui::pos2(rect.left(), rect.bottom() - 1.0),
            egui::pos2(rect.right(), rect.bottom() - 1.0),
        ],
        egui::Stroke::new(1.0, ctx.theme.border),
    );
    let ticks = 5usize;
    for i in 0..=ticks {
        let frac = i as f64 / ticks as f64;
        let ts = start + ((i128::from(end) - i128::from(start)) as f64 * frac).round() as i64;
        let x = ts_to_x(ts, rect, start, end);
        painter.line_segment(
            [
                egui::pos2(x, rect.bottom() - 5.0),
                egui::pos2(x, rect.bottom() - 1.0),
            ],
            egui::Stroke::new(1.0, ctx.theme.text_dim),
        );
        let label = fp_query::format_time(ts, "exact", ctx.tz, "%H:%M:%S");
        let align = if i == 0 {
            egui::Align2::LEFT_TOP
        } else if i == ticks {
            egui::Align2::RIGHT_TOP
        } else {
            egui::Align2::CENTER_TOP
        };
        painter.text(
            egui::pos2(x, rect.top() + 2.0),
            align,
            label,
            egui::FontId::monospace(10.5),
            ctx.theme.text_dim,
        );
    }
}

fn paint_lane_bg(
    ui: &egui::Ui,
    rect: egui::Rect,
    theme: &Theme,
    drag_preview: Option<(i64, i64)>,
    bounds: (i64, i64),
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, theme.bg0);
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0, theme.border),
        egui::StrokeKind::Inside,
    );
    if let Some((ps, pe)) = drag_preview {
        let x1 = ts_to_x(ps, rect, bounds.0, bounds.1);
        let x2 = ts_to_x(pe, rect, bounds.0, bounds.1).max(x1 + 2.0);
        let sel =
            egui::Rect::from_min_max(egui::pos2(x1, rect.top()), egui::pos2(x2, rect.bottom()));
        painter.rect_filled(sel, 2.0, theme.accent.gamma_multiply(0.18));
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_density_bars(
    ui: &egui::Ui,
    rect: egui::Rect,
    bucket_bounds: &[(i64, i64)],
    buckets: &[u64],
    global_max: u64,
    bounds: (i64, i64),
    color: egui::Color32,
    sqrt_scale: bool,
) {
    let painter = ui.painter_at(rect);
    let pad = 3.0;
    let usable_h = (rect.height() - pad * 2.0).max(1.0);
    for (idx, count) in buckets.iter().copied().enumerate() {
        if count == 0 {
            continue;
        }
        let Some(&(bs, be)) = bucket_bounds.get(idx) else {
            continue;
        };
        let x1 = ts_to_x(bs, rect, bounds.0, bounds.1);
        let x2 = ts_to_x(be, rect, bounds.0, bounds.1).max(x1 + 2.0);
        let h = (scale_count(count, global_max, sqrt_scale) * usable_h).max(2.0);
        let bar = egui::Rect::from_min_max(
            egui::pos2(x1, rect.bottom() - pad - h),
            egui::pos2(x2, rect.bottom() - pad),
        );
        painter.rect_filled(bar, 1.0, color);
    }
}

fn paint_cursor_line(ui: &egui::Ui, rect: egui::Rect, x: f32, theme: &Theme) {
    if x >= rect.left() && x <= rect.right() {
        ui.painter().line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(1.0, theme.accent.gamma_multiply(0.8)),
        );
    }
}

fn handle_track_window_input(
    ui: &egui::Ui,
    resp: &egui::Response,
    rect: egui::Rect,
    current_window: (i64, i64),
    ctx: &ViewCtx<'_>,
    state: &mut LanesState,
    actions: &mut Vec<Action>,
) {
    if resp.double_clicked() {
        actions.push(Action::Scope(ScopeAction::SetWindow {
            since: None,
            until: None,
        }));
        return;
    }
    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let ts = x_to_ts(pos.x, rect, current_window.0, current_window.1);
        state.drag_anchor_ts = Some(ts);
        state.drag_preview = Some((ts, ts));
    }
    if resp.dragged()
        && let (Some(anchor), Some(pos)) = (state.drag_anchor_ts, resp.interact_pointer_pos())
    {
        let ts = x_to_ts(pos.x, rect, current_window.0, current_window.1);
        state.drag_preview = Some((anchor.min(ts), anchor.max(ts)));
    }
    if resp.drag_stopped() {
        let is_drag = ui
            .input(|i| i.pointer.press_origin().zip(i.pointer.interact_pos()))
            .is_some_and(|(a, b)| (a.x - b.x).abs() >= CLICK_DRAG_THRESHOLD_PX);
        if is_drag && let Some((s, e)) = state.drag_preview {
            actions.push(Action::Scope(ScopeAction::SetWindow {
                since: Some(s),
                until: Some(e.max(s.saturating_add(1))),
            }));
        }
        state.drag_anchor_ts = None;
        state.drag_preview = None;
    }

    if resp.hovered() {
        let (scroll_y, ctrl) = ui.input(|i| (i.smooth_scroll_delta.y, i.modifiers.ctrl));
        if ctrl
            && scroll_y.abs() > 0.1
            && let Some(Some((cs, ce))) = ctx.model.bounds.value().copied()
        {
            let anchor = resp
                .hover_pos()
                .map(|p| x_to_ts(p.x, rect, current_window.0, current_window.1))
                .unwrap_or((current_window.0 + current_window.1) / 2);
            let (ns, ne) = zoom_window(
                current_window,
                (cs, ce.max(cs.saturating_add(1))),
                anchor,
                scroll_y > 0.0,
            );
            actions.push(Action::Scope(ScopeAction::SetWindow {
                since: Some(ns),
                until: Some(ne),
            }));
        }
    }
}

fn render_untimed_lane(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    data: &LanesDataVm,
    state: &mut LanesState,
    actions: &mut Vec<Action>,
) {
    egui::Frame::group(ui.style())
        .fill(ctx.theme.bg1)
        .stroke(egui::Stroke::new(1.0, ctx.theme.border))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                let toggle = if state.untimed_open { "[-]" } else { "[+]" };
                if ui.small_button(toggle).clicked() {
                    state.untimed_open = !state.untimed_open;
                }
                ui.label(
                    egui::RichText::new(format!(
                        "Untimed lane (order_only): {} events",
                        thousands(data.untimed_total)
                    ))
                    .strong(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("show as list").clicked() {
                        actions.push(Action::Scope(ScopeAction::ShowUntimed));
                        actions.push(Action::Go(View::Timeline));
                    }
                });
            });
            if state.untimed_open {
                ui.add_space(3.0);
                ui.label(
                    egui::RichText::new(
                        "order_only rows have no timestamp and cannot be placed on a time axis",
                    )
                    .small()
                    .color(ctx.theme.text_dim),
                );
                if data.untimed_lanes.is_empty() {
                    ui.label(
                        egui::RichText::new("No untimed events in scope.")
                            .small()
                            .color(ctx.theme.text_dim),
                    );
                } else {
                    ui.horizontal_wrapped(|ui| {
                        for item in data.untimed_lanes.iter().take(12) {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{}: {}",
                                    item.label.as_str(),
                                    thousands(item.count)
                                ))
                                .small()
                                .monospace(),
                            );
                        }
                    });
                }
            }
        });
}
