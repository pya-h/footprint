//! Overview strip: two-scale stacked category timeline scrubber with brushing,
//! handle resizing, panning, zoom, run start markers, IOC ticks, and click-to-seek.

use eframe::egui;
use fp_query::Tz;
use fp_query::category::Category;
use fp_query::overview::Histogram;

use crate::model::{Action, Model, ScopeAction};
use crate::theme::Theme;
use crate::vm::RunMarkerVm;

const MINIMAP_HEIGHT: f32 = 58.0;
const DETAIL_HEIGHT: f32 = 36.0;
const EDGE_HIT_PX: f32 = 7.0;
const CLICK_DRAG_THRESHOLD_PX: f32 = 4.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragMode {
    Brush {
        anchor_ts: i64,
    },
    ResizeStart {
        fixed_end: i64,
    },
    ResizeEnd {
        fixed_start: i64,
    },
    Pan {
        start_pointer_ts: i64,
        orig_start: i64,
        orig_end: i64,
    },
}

pub struct StripState {
    pub sqrt_scale: bool,
    drag: Option<DragMode>,
    preview_window: Option<(i64, i64)>,
    drag_distance: f32,
    pub last_width_px: usize,
}

impl Default for StripState {
    fn default() -> Self {
        Self {
            sqrt_scale: true,
            drag: None,
            preview_window: None,
            drag_distance: 0.0,
            last_width_px: 800,
        }
    }
}

pub fn show_strip(
    ui: &mut egui::Ui,
    model: &Model,
    theme: &Theme,
    tz: Tz,
    state: &mut StripState,
    actions: &mut Vec<Action>,
) {
    let Some(Some((case_start, case_end))) = model.bounds.value().copied() else {
        return;
    };
    let case_span_end = case_end.max(case_start.saturating_add(1));
    let filter = model.scope.filter();
    let active_window = state
        .preview_window
        .or_else(|| match (filter.since, filter.until) {
            (Some(s), Some(u)) => Some((s.min(u), s.max(u))),
            (Some(s), None) => Some((s, case_span_end)),
            (None, Some(u)) => Some((case_start, u)),
            (None, None) => None,
        });

    egui::Frame::group(ui.style())
        .fill(theme.bg1)
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(8, 6))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Overview").strong());
                if let Some((ws, we)) = active_window {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} -> {}",
                            fp_query::format_time(ws, "exact", tz, "%d %b %H:%M:%S"),
                            fp_query::format_time(we, "exact", tz, "%d %b %H:%M:%S")
                        ))
                        .small(),
                    );
                    if ui.small_button("clear window").clicked() {
                        actions.push(Action::Scope(ScopeAction::SetWindow {
                            since: None,
                            until: None,
                        }));
                    }
                } else {
                    ui.label(
                        egui::RichText::new(
                            "drag to brush window · click to seek · double-click to clear",
                        )
                        .weak()
                        .small(),
                    );
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let scale_label = if state.sqrt_scale {
                        "scale: sqrt"
                    } else {
                        "scale: linear"
                    };
                    if ui
                        .small_button(scale_label)
                        .on_hover_text("toggle linear vs sqrt height scaling")
                        .clicked()
                    {
                        state.sqrt_scale = !state.sqrt_scale;
                    }
                });
            });

            let avail_w = ui.available_width().max(64.0);
            state.last_width_px = avail_w.round() as usize;

            let (minimap_rect, resp) = ui.allocate_exact_size(
                egui::vec2(avail_w, MINIMAP_HEIGHT),
                egui::Sense::click_and_drag(),
            );

            let minimap_band = BandData {
                hist: model.histogram.value(),
                ioc_hist: model.ioc_histogram.value(),
                runs: model.runs.value().map(Vec::as_slice).unwrap_or(&[]),
                bounds: (case_start, case_span_end),
                active_window,
            };
            paint_histogram_band(ui, minimap_rect, &minimap_band, theme, state.sqrt_scale);

            handle_minimap_input(ui, &resp, minimap_rect, &minimap_band, tz, state, actions);

            if let Some((ws, we)) = active_window {
                ui.add_space(3.0);
                let detail_w = ui.available_width().max(64.0);
                let (detail_rect, detail_resp) = ui
                    .allocate_exact_size(egui::vec2(detail_w, DETAIL_HEIGHT), egui::Sense::click());
                let detail_band = BandData {
                    hist: model.detail_histogram.value(),
                    ioc_hist: None,
                    runs: &[],
                    bounds: (ws, we.max(ws.saturating_add(1))),
                    active_window: None,
                };
                paint_histogram_band(ui, detail_rect, &detail_band, theme, state.sqrt_scale);
                if detail_resp.clicked()
                    && let Some(pos) = detail_resp.interact_pointer_pos()
                {
                    let ts = x_to_ts(pos.x, detail_rect, ws, we.max(ws.saturating_add(1)));
                    actions.push(Action::RequestSeek(ts));
                }
                if let Some(pos) = detail_resp.hover_pos() {
                    let ts = x_to_ts(pos.x, detail_rect, ws, we.max(ws.saturating_add(1)));
                    show_hover_tooltip(ui, pos, ts, model.detail_histogram.value(), None, &[], tz);
                }
            }
        });
}

pub struct BandData<'a> {
    pub hist: Option<&'a Histogram>,
    pub ioc_hist: Option<&'a Histogram>,
    pub runs: &'a [RunMarkerVm],
    pub bounds: (i64, i64),
    pub active_window: Option<(i64, i64)>,
}

pub fn paint_histogram_band(
    ui: &egui::Ui,
    rect: egui::Rect,
    band: &BandData<'_>,
    theme: &Theme,
    sqrt_scale: bool,
) {
    let (band_start, band_end) = band.bounds;
    let hist = band.hist;
    let ioc_hist = band.ioc_hist;
    let runs = band.runs;
    let active_window = band.active_window;
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 3.0, theme.bg0);
    painter.rect_stroke(
        rect,
        3.0,
        egui::Stroke::new(1.0, theme.border),
        egui::StrokeKind::Inside,
    );

    if let Some(hist) = hist
        && !hist.buckets.is_empty()
    {
        let max_total = hist
            .buckets
            .iter()
            .map(|b| b.counts.iter().sum::<u64>())
            .max()
            .unwrap_or(0)
            .max(1);

        let top_pad = 4.0;
        let bottom_pad = 4.0;
        let usable_h = (rect.height() - top_pad - bottom_pad).max(1.0);

        for (idx, bucket) in hist.buckets.iter().enumerate() {
            let total: u64 = bucket.counts.iter().sum();
            let x0 = ts_to_x(bucket.start_utc, rect, band_start, band_end);
            let x1 =
                ts_to_x(bucket.end_utc.saturating_add(1), rect, band_start, band_end).max(x0 + 1.0);
            if total > 0 {
                let frac = scale_count(total, max_total, sqrt_scale);
                let total_h = (frac * usable_h).max(1.5);
                let mut cursor_y = rect.bottom() - bottom_pad;
                for (cat_idx, count) in bucket.counts.iter().enumerate() {
                    if *count == 0 {
                        continue;
                    }
                    let slice_h = total_h * (*count as f32 / total as f32);
                    let y0 = (cursor_y - slice_h).max(rect.top() + top_pad);
                    let bar = egui::Rect::from_min_max(
                        egui::pos2(x0, y0),
                        egui::pos2(x1 - 0.4, cursor_y),
                    );
                    painter.rect_filled(bar, 0.0, theme.categories[cat_idx]);
                    cursor_y = y0;
                }
            }
            if let Some(ioc) = ioc_hist
                && let Some(ioc_bucket) = ioc.buckets.get(idx)
                && ioc_bucket.counts.iter().sum::<u64>() > 0
            {
                let tick = egui::Rect::from_min_max(
                    egui::pos2(x0, rect.top() + 1.0),
                    egui::pos2(x1.max(x0 + 2.0), rect.top() + 4.0),
                );
                painter.rect_filled(tick, 0.0, theme.ioc);
            }
        }
    }

    for run in runs {
        if run.started_utc >= band_start && run.started_utc <= band_end {
            let x = ts_to_x(run.started_utc, rect, band_start, band_end);
            painter.line_segment(
                [
                    egui::pos2(x, rect.top() + 1.0),
                    egui::pos2(x, rect.bottom() - 1.0),
                ],
                egui::Stroke::new(1.0, theme.accent.linear_multiply(0.55)),
            );
        }
    }

    if let Some((ws, we)) = active_window {
        let wx0 = ts_to_x(ws, rect, band_start, band_end).clamp(rect.left(), rect.right());
        let wx1 = ts_to_x(we, rect, band_start, band_end).clamp(wx0 + 2.0, rect.right());
        let shade = egui::Color32::from_rgba_unmultiplied(0, 0, 0, 120);
        if wx0 > rect.left() {
            painter.rect_filled(
                egui::Rect::from_min_max(rect.left_top(), egui::pos2(wx0, rect.bottom())),
                0.0,
                shade,
            );
        }
        if wx1 < rect.right() {
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(wx1, rect.top()), rect.right_bottom()),
                0.0,
                shade,
            );
        }
        let handle_stroke = egui::Stroke::new(2.0, theme.accent);
        painter.line_segment(
            [egui::pos2(wx0, rect.top()), egui::pos2(wx0, rect.bottom())],
            handle_stroke,
        );
        painter.line_segment(
            [egui::pos2(wx1, rect.top()), egui::pos2(wx1, rect.bottom())],
            handle_stroke,
        );
    }
}

fn handle_minimap_input(
    ui: &egui::Ui,
    resp: &egui::Response,
    rect: egui::Rect,
    band: &BandData<'_>,
    tz: Tz,
    state: &mut StripState,
    actions: &mut Vec<Action>,
) {
    let (case_start, case_end) = band.bounds;
    let active_window = band.active_window;
    let hist = band.hist;
    let ioc_hist = band.ioc_hist;
    let runs = band.runs;
    if resp.double_clicked() {
        state.drag = None;
        state.preview_window = None;
        actions.push(Action::Scope(ScopeAction::SetWindow {
            since: None,
            until: None,
        }));
        return;
    }

    if resp.hovered() {
        let scroll_delta = ui.input(|i| {
            if i.modifiers.ctrl {
                i.smooth_scroll_delta.y
            } else {
                0.0
            }
        });
        if scroll_delta.abs() > 0.5
            && let Some(pos) = resp.hover_pos()
        {
            let anchor = x_to_ts(pos.x, rect, case_start, case_end);
            let cur = active_window.unwrap_or((case_start, case_end));
            let zoomed = zoom_window(cur, (case_start, case_end), anchor, scroll_delta > 0.0);
            actions.push(Action::Scope(ScopeAction::SetWindow {
                since: Some(zoomed.0),
                until: Some(zoomed.1),
            }));
        }
    }

    if resp.drag_started()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        state.drag_distance = 0.0;
        let pointer_ts = x_to_ts(pos.x, rect, case_start, case_end);
        let shift = ui.input(|i| i.modifiers.shift);
        state.drag = Some(choose_drag_mode(
            pos.x,
            pointer_ts,
            rect,
            (case_start, case_end),
            active_window,
            shift,
        ));
    }

    if resp.dragged()
        && let (Some(mode), Some(pos)) = (state.drag, resp.interact_pointer_pos())
    {
        state.drag_distance += resp.drag_delta().length();
        let ts = x_to_ts(pos.x, rect, case_start, case_end);
        let window = match mode {
            DragMode::Brush { anchor_ts } => (anchor_ts.min(ts), anchor_ts.max(ts)),
            DragMode::ResizeStart { fixed_end } => (ts.min(fixed_end), ts.max(fixed_end)),
            DragMode::ResizeEnd { fixed_start } => (fixed_start.min(ts), fixed_start.max(ts)),
            DragMode::Pan {
                start_pointer_ts,
                orig_start,
                orig_end,
            } => pan_window(
                (orig_start, orig_end),
                (case_start, case_end),
                ts - start_pointer_ts,
            ),
        };
        state.preview_window = Some(window);
    }

    if resp.drag_stopped() {
        let drag_dist = state.drag_distance;
        let preview = state.preview_window.take();
        state.drag = None;
        state.drag_distance = 0.0;
        if drag_dist >= CLICK_DRAG_THRESHOLD_PX
            && let Some((s, e)) = preview
        {
            actions.push(Action::Scope(ScopeAction::SetWindow {
                since: Some(s),
                until: Some(e),
            }));
        }
    } else if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let ts = x_to_ts(pos.x, rect, case_start, case_end);
        actions.push(Action::RequestSeek(ts));
    }

    if let Some(pos) = resp.hover_pos() {
        let ts = x_to_ts(pos.x, rect, case_start, case_end);
        show_hover_tooltip(ui, pos, ts, hist, ioc_hist, runs, tz);
    }
}

fn choose_drag_mode(
    x: f32,
    pointer_ts: i64,
    rect: egui::Rect,
    bounds: (i64, i64),
    active_window: Option<(i64, i64)>,
    shift: bool,
) -> DragMode {
    if let Some((ws, we)) = active_window {
        let wx0 = ts_to_x(ws, rect, bounds.0, bounds.1);
        let wx1 = ts_to_x(we, rect, bounds.0, bounds.1);
        if (x - wx0).abs() <= EDGE_HIT_PX {
            return DragMode::ResizeStart { fixed_end: we };
        }
        if (x - wx1).abs() <= EDGE_HIT_PX {
            return DragMode::ResizeEnd { fixed_start: ws };
        }
        if shift || (x > wx0 + EDGE_HIT_PX && x < wx1 - EDGE_HIT_PX) {
            return DragMode::Pan {
                start_pointer_ts: pointer_ts,
                orig_start: ws,
                orig_end: we,
            };
        }
    }
    DragMode::Brush {
        anchor_ts: pointer_ts,
    }
}

fn show_hover_tooltip(
    ui: &egui::Ui,
    _pos: egui::Pos2,
    ts: i64,
    hist: Option<&Histogram>,
    ioc_hist: Option<&Histogram>,
    runs: &[RunMarkerVm],
    tz: Tz,
) {
    let time_str = fp_query::format_time(ts, "exact", tz, "%Y-%m-%d %H:%M:%S");
    let zone = match tz {
        Tz::Local => "local".to_string(),
        Tz::Utc => "UTC".to_string(),
        Tz::Fixed(off) => off.to_string(),
    };

    let mut lines = vec![format!("{time_str} ({zone})")];
    if let Some(hist) = hist
        && let Some((idx, bucket)) = hist
            .buckets
            .iter()
            .enumerate()
            .find(|(_, b)| ts >= b.start_utc && ts <= b.end_utc)
    {
        let total: u64 = bucket.counts.iter().sum();
        lines.push(format!("{total} events in bucket"));
        for (i, c) in bucket.counts.iter().enumerate() {
            if *c > 0 {
                lines.push(format!("  {}: {c}", Category::ALL[i].label()));
            }
        }
        if let Some(ioc) = ioc_hist
            && let Some(ioc_b) = ioc.buckets.get(idx)
        {
            let hits: u64 = ioc_b.counts.iter().sum();
            if hits > 0 {
                lines.push(format!("  IOC hits: {hits}"));
            }
        }
    }
    if let Some(run) = runs
        .iter()
        .min_by_key(|r| r.started_utc.abs_diff(ts))
        .filter(|r| r.started_utc.abs_diff(ts) <= 60_000_000)
    {
        lines.push(format!("Run #{} ({} new)", run.id, run.events_new));
    }

    let text = lines.join("\n");
    egui::Tooltip::always_open(
        ui.ctx().clone(),
        ui.layer_id(),
        ui.scope_id().with("strip_hover"),
        egui::PopupAnchor::Pointer,
    )
    .show(|ui| {
        ui.label(text);
    });
}

pub fn scale_count(count: u64, max: u64, sqrt_scale: bool) -> f32 {
    if max == 0 || count == 0 {
        return 0.0;
    }
    let ratio = (count as f64 / max as f64).clamp(0.0, 1.0);
    if sqrt_scale {
        ratio.sqrt() as f32
    } else {
        ratio as f32
    }
}

pub fn ts_to_x(ts: i64, rect: egui::Rect, start: i64, end: i64) -> f32 {
    let span = (i128::from(end) - i128::from(start)).max(1) as f64;
    let frac = ((i128::from(ts) - i128::from(start)) as f64 / span).clamp(0.0, 1.0);
    rect.left() + frac as f32 * rect.width()
}

pub fn x_to_ts(x: f32, rect: egui::Rect, start: i64, end: i64) -> i64 {
    let w = rect.width().max(1.0);
    let frac = ((x - rect.left()) / w).clamp(0.0, 1.0) as f64;
    let span = (i128::from(end) - i128::from(start)).max(1) as f64;
    let delta = (frac * span).round() as i128;
    (i128::from(start) + delta).clamp(i128::from(start), i128::from(end)) as i64
}

pub fn zoom_window(
    window: (i64, i64),
    bounds: (i64, i64),
    anchor: i64,
    zoom_in: bool,
) -> (i64, i64) {
    let factor = if zoom_in { 0.7 } else { 1.4 };
    let span = (i128::from(window.1) - i128::from(window.0)).max(1_000_000);
    let new_span = ((span as f64 * factor).round() as i128)
        .clamp(1_000_000, i128::from(bounds.1) - i128::from(bounds.0));
    let ratio = ((i128::from(anchor) - i128::from(window.0)) as f64 / span as f64).clamp(0.0, 1.0);
    let mut new_start = i128::from(anchor) - (ratio * new_span as f64).round() as i128;
    let mut new_end = new_start + new_span;
    if new_start < i128::from(bounds.0) {
        new_start = i128::from(bounds.0);
        new_end = (new_start + new_span).min(i128::from(bounds.1));
    }
    if new_end > i128::from(bounds.1) {
        new_end = i128::from(bounds.1);
        new_start = (new_end - new_span).max(i128::from(bounds.0));
    }
    (new_start as i64, new_end as i64)
}

pub fn pan_window(window: (i64, i64), bounds: (i64, i64), delta: i64) -> (i64, i64) {
    let span = i128::from(window.1) - i128::from(window.0);
    let mut start = i128::from(window.0) + i128::from(delta);
    let mut end = start + span;
    if start < i128::from(bounds.0) {
        start = i128::from(bounds.0);
        end = start + span;
    }
    if end > i128::from(bounds.1) {
        end = i128::from(bounds.1);
        start = (end - span).max(i128::from(bounds.0));
    }
    (start as i64, end as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coord_round_trip_stays_within_bounds() {
        let rect = egui::Rect::from_min_max(egui::pos2(10.0, 0.0), egui::pos2(210.0, 40.0));
        let start = 1_000_000;
        let end = 101_000_000;
        let mid_ts = x_to_ts(110.0, rect, start, end);
        assert_eq!(mid_ts, 51_000_000);
        assert!((ts_to_x(mid_ts, rect, start, end) - 110.0).abs() < 0.01);
    }

    #[test]
    fn zoom_and_pan_respect_bounds() {
        let bounds = (0, 100_000_000);
        let zoomed = zoom_window(bounds, bounds, 50_000_000, true);
        assert!(zoomed.0 > bounds.0 && zoomed.1 < bounds.1);
        let panned = pan_window(zoomed, bounds, -999_999_999);
        assert_eq!(panned.0, bounds.0);
    }

    #[test]
    fn sqrt_scale_lifts_small_values() {
        let lin = scale_count(1, 100, false);
        let sq = scale_count(1, 100, true);
        assert!(sq > lin);
        assert!((sq - 0.1).abs() < 0.001);
    }
}
