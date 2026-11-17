//! Dashboard view: executive overview of the case with clickable KPI cards,
//! activity histogram, category breakdown, top hosts, and Worth-a-Look findings.

use std::path::Path;

use eframe::egui;
use fp_query::category::Category;

use crate::model::{Action, LoadStatus, ScopeAction, View};
use crate::theme::Theme;
use crate::vm::{DashboardSummaryVm, HostSummaryVm};
use crate::widgets::ViewCtx;
use crate::widgets::chip::thousands;
use crate::widgets::facets::apply_category_click;
use crate::widgets::kit;
use crate::widgets::strip::{BandData, paint_histogram_band, x_to_ts};

const HERO_HIST_HEIGHT: f32 = 88.0;
const CLICK_DRAG_THRESHOLD_PX: f32 = 5.0;
/// Below this central width the cards stack in one column.
const TWO_COLUMN_MIN_WIDTH: f32 = 840.0;
const CATEGORY_LABEL_WIDTH: f32 = 96.0;

#[derive(Default)]
pub struct DashboardState {
    pub sqrt_scale: bool,
    drag_anchor_ts: Option<i64>,
    drag_preview: Option<(i64, i64)>,
}

pub fn show_dashboard(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    case_path: &Path,
    case_size: Option<u64>,
    state: &mut DashboardState,
    actions: &mut Vec<Action>,
) {
    egui::ScrollArea::vertical()
        .id_salt("dashboard_scroll")
        .show(ui, |ui| {
            render_subtitle(ui, ctx, case_path, case_size);
            ui.add_space(8.0);
            render_kpis(ui, ctx, actions);
            ui.add_space(10.0);

            let avail = ui.available_width();
            if avail >= TWO_COLUMN_MIN_WIDTH {
                ui.columns(2, |cols| {
                    render_activity_card(&mut cols[0], ctx, state, actions);
                    cols[0].add_space(8.0);
                    render_top_hosts_card(&mut cols[0], ctx, actions);

                    render_worth_a_look(&mut cols[1], ctx, actions);
                    cols[1].add_space(8.0);
                    render_categories_card(&mut cols[1], ctx, actions);
                });
            } else {
                render_activity_card(ui, ctx, state, actions);
                ui.add_space(8.0);
                render_categories_card(ui, ctx, actions);
                ui.add_space(8.0);
                render_worth_a_look(ui, ctx, actions);
                ui.add_space(8.0);
                render_top_hosts_card(ui, ctx, actions);
            }
        });
}

fn render_subtitle(ui: &mut egui::Ui, ctx: &ViewCtx<'_>, case_path: &Path, case_size: Option<u64>) {
    let model = ctx.model;
    let summary = model.dashboard_summary.value();
    let user = summary
        .and_then(|s| s.user_name.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("unknown");
    let host = summary
        .and_then(|s| s.host_name.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("unknown");
    let file_name = case_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| case_path.display().to_string());
    let size_text = case_size.map(format_bytes).unwrap_or_else(|| "-".into());
    let run_count = model.runs.value().map(Vec::len).unwrap_or(0);
    let span = match model.bounds.value().copied().flatten() {
        Some((first, last)) => format!(
            "{} .. {}",
            fp_query::format_time(first, "exact", ctx.tz, "%Y-%m-%d %H:%M"),
            fp_query::format_time(last, "exact", ctx.tz, "%Y-%m-%d %H:%M"),
        ),
        None => "no timed events".to_string(),
    };

    ui.horizontal_wrapped(|ui| {
        ui.heading("Dashboard");
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(format!(
                "{user}@{host} · {file_name} · {size_text} · {run_count} runs · {span}"
            ))
            .color(ctx.theme.text_dim),
        );
    });
}

fn render_kpis(ui: &mut egui::Ui, ctx: &ViewCtx<'_>, actions: &mut Vec<Action>) {
    let model = ctx.model;
    let theme = ctx.theme;
    let counts = model.counts.value().copied();
    let total_text = counts
        .map(|c| thousands(c.in_scope))
        .unwrap_or_else(|| "...".into());
    let total_sub = match counts {
        Some(c) if c.in_scope != c.in_case => format!("of {} in case", thousands(c.in_case)),
        _ => "timed + untimed".to_string(),
    };

    let untimed_count = model
        .timing
        .value()
        .map(|t| t.untimed)
        .or_else(|| model.untimed.value().copied());
    let untimed_text = untimed_count.map(thousands).unwrap_or_else(|| "...".into());

    let ioc_count = model.ioc_count.value().copied();
    let ioc_text = ioc_count.map(thousands).unwrap_or_else(|| "...".into());

    let summary = model.dashboard_summary.value();
    let summary_loading = matches!(
        model.dashboard_summary.status(model.generation),
        LoadStatus::Loading
    ) && summary.is_none();

    let flagged_text = summary
        .map(|s| thousands(s.flagged_state_count))
        .unwrap_or_else(|| {
            if summary_loading {
                "...".into()
            } else {
                "0".into()
            }
        });
    let flagged_sub = summary
        .filter(|s| !s.flagged_state_kinds.is_empty())
        .map(|s| {
            s.flagged_state_kinds
                .iter()
                .take(2)
                .map(|k| k.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_else(|| "system state checks".to_string());

    let issue_count = summary.map(|s| s.source_warnings.len()).unwrap_or(0);
    let issue_text = if summary.is_some() {
        issue_count.to_string()
    } else if summary_loading {
        "...".into()
    } else {
        "0".into()
    };

    let (cov_text, cov_sub) = match summary {
        Some(s) if s.coverage_total > 0 => {
            let pct = (s.coverage_covered * 100) / s.coverage_total;
            (
                format!("{pct}%"),
                format!("{}/{} areas", s.coverage_covered, s.coverage_total),
            )
        }
        Some(_) => ("100%".into(), "no gaps".into()),
        None => ("...".into(), "coverage".into()),
    };

    let cards = [
        KpiSpec {
            title: "EVENTS",
            value: total_text,
            animated_value: counts.map(|c| c.in_scope as f32),
            sub: total_sub,
            accent: theme.text,
            action: KpiClick::Timeline,
        },
        KpiSpec {
            title: "UNTIMED",
            value: untimed_text,
            animated_value: untimed_count.map(|count| count as f32),
            sub: "order_only".into(),
            accent: if untimed_count.unwrap_or(0) > 0 {
                theme.warn
            } else {
                theme.text
            },
            action: KpiClick::ShowUntimed,
        },
        KpiSpec {
            title: "IOC HITS",
            value: ioc_text,
            animated_value: ioc_count.map(|count| count as f32),
            sub: "correlated events".into(),
            accent: if ioc_count.unwrap_or(0) > 0 {
                theme.ioc
            } else {
                theme.text
            },
            action: KpiClick::IocOnly,
        },
        KpiSpec {
            title: "FLAGGED STATE",
            value: flagged_text,
            animated_value: summary.map(|s| s.flagged_state_count as f32),
            sub: flagged_sub,
            accent: if summary.is_some_and(|s| s.flagged_state_count > 0) {
                theme.warn
            } else {
                theme.text
            },
            action: KpiClick::State,
        },
        KpiSpec {
            title: "SOURCE ISSUES",
            value: issue_text,
            animated_value: summary.map(|_| issue_count as f32),
            sub: "partial / unreadable".into(),
            accent: if issue_count > 0 {
                theme.warn
            } else {
                theme.text
            },
            action: KpiClick::Sources,
        },
        KpiSpec {
            title: "COVERAGE",
            value: cov_text,
            animated_value: summary.map(|s| {
                ((s.coverage_covered as u128) * 100)
                    .checked_div(s.coverage_total as u128)
                    .map_or(100.0, |percent| percent as f32)
            }),
            sub: cov_sub,
            accent: theme.ok,
            action: KpiClick::Sources,
        },
    ];

    let cols = if ui.available_width() >= 760.0 { 6 } else { 3 };
    for row in cards.chunks(cols) {
        ui.columns(row.len(), |ui_cols| {
            for (idx, card) in row.iter().enumerate() {
                if render_kpi_card(&mut ui_cols[idx], theme, card) {
                    emit_kpi_click(card.action, actions);
                }
            }
        });
        ui.add_space(4.0);
    }
}

#[derive(Clone, Copy)]
enum KpiClick {
    Timeline,
    ShowUntimed,
    IocOnly,
    State,
    Sources,
}

struct KpiSpec {
    title: &'static str,
    value: String,
    animated_value: Option<f32>,
    sub: String,
    accent: egui::Color32,
    action: KpiClick,
}

fn render_kpi_card(ui: &mut egui::Ui, theme: &Theme, card: &KpiSpec) -> bool {
    let id = ui.scope_id().with(("kpi", card.title));
    let frame = kit::card_frame(theme, kit::hover_amount(ui, id, theme))
        .inner_margin(egui::Margin::symmetric(12, 9));
    let outer = frame.show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.label(
            egui::RichText::new(card.title)
                .small()
                .color(theme.text_dim),
        );
        let value = card
            .animated_value
            .map(|target| {
                let value = if theme.reduced_motion {
                    target
                } else {
                    ui.ctx()
                        .animate_value_with_time(id.with("value"), target, 0.32)
                };
                if card.title == "COVERAGE" {
                    format!("{value:.0}%")
                } else {
                    thousands(value.round() as u64)
                }
            })
            .unwrap_or_else(|| card.value.clone());
        ui.label(
            egui::RichText::new(value)
                .size(22.0)
                .strong()
                .color(card.accent),
        );
        ui.add(
            egui::Label::new(egui::RichText::new(&card.sub).small().color(theme.text_dim))
                .truncate(),
        );
    });
    let resp = outer
        .response
        .interact(egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text("click to open filtered view");
    kit::remember_hover(ui, id, resp.hovered());
    resp.clicked()
}

fn emit_kpi_click(click: KpiClick, actions: &mut Vec<Action>) {
    match click {
        KpiClick::Timeline => actions.push(Action::Go(View::Timeline)),
        KpiClick::ShowUntimed => {
            actions.push(Action::Scope(ScopeAction::ShowUntimed));
            actions.push(Action::Go(View::Timeline));
        }
        KpiClick::IocOnly => {
            actions.push(Action::Scope(ScopeAction::SetIocOnly(true)));
            actions.push(Action::Go(View::Timeline));
        }
        KpiClick::State => actions.push(Action::Go(View::State)),
        KpiClick::Sources => actions.push(Action::Go(View::Sources)),
    }
}

fn render_activity_card(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    state: &mut DashboardState,
    actions: &mut Vec<Action>,
) {
    let model = ctx.model;
    let theme = ctx.theme;
    card_frame(ui, theme, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("ACTIVITY OVER TIME").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.link("open Timeline ->").clicked() {
                    actions.push(Action::Go(View::Timeline));
                }
                let scale_label = if state.sqrt_scale {
                    "scale: sqrt"
                } else {
                    "scale: linear"
                };
                if ui.small_button(scale_label).clicked() {
                    state.sqrt_scale = !state.sqrt_scale;
                }
            });
        });
        ui.add_space(4.0);

        let Some(Some((case_start, case_end))) = model.bounds.value().copied() else {
            ui.label(egui::RichText::new("no timed events in scope").color(theme.text_dim));
            return;
        };
        let case_span_end = case_end.max(case_start.saturating_add(1));
        let filter = model.scope.filter();
        let active_window = state
            .drag_preview
            .or_else(|| match (filter.since, filter.until) {
                (Some(s), Some(u)) => Some((s.min(u), s.max(u))),
                (Some(s), None) => Some((s, case_span_end)),
                (None, Some(u)) => Some((case_start, u)),
                (None, None) => None,
            });

        let avail_w = ui.available_width().max(64.0);
        let (rect, resp) = ui.allocate_exact_size(
            egui::vec2(avail_w, HERO_HIST_HEIGHT),
            egui::Sense::click_and_drag(),
        );
        let band = BandData {
            hist: model.histogram.value(),
            ioc_hist: model.ioc_histogram.value(),
            runs: model.runs.value().map(Vec::as_slice).unwrap_or(&[]),
            bounds: (case_start, case_span_end),
            active_window,
        };
        paint_histogram_band(ui, rect, &band, theme, state.sqrt_scale);

        if resp.drag_started()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let ts = x_to_ts(pos.x, rect, case_start, case_span_end);
            state.drag_anchor_ts = Some(ts);
            state.drag_preview = Some((ts, ts));
        }
        if resp.dragged()
            && let (Some(anchor), Some(pos)) = (state.drag_anchor_ts, resp.interact_pointer_pos())
        {
            let ts = x_to_ts(pos.x, rect, case_start, case_span_end);
            state.drag_preview = Some((anchor.min(ts), anchor.max(ts)));
        }
        if resp.drag_stopped() {
            let delta = ui.input(|i| i.pointer.press_origin().zip(i.pointer.interact_pos()));
            let is_drag = delta.is_some_and(|(a, b)| (a.x - b.x).abs() >= CLICK_DRAG_THRESHOLD_PX);
            if is_drag && let Some((s, e)) = state.drag_preview {
                actions.push(Action::Scope(ScopeAction::SetWindow {
                    since: Some(s),
                    until: Some(e.max(s.saturating_add(1))),
                }));
                actions.push(Action::Go(View::Timeline));
            }
            state.drag_anchor_ts = None;
            state.drag_preview = None;
        } else if resp.clicked()
            && let Some(pos) = resp.interact_pointer_pos()
        {
            let ts = x_to_ts(pos.x, rect, case_start, case_span_end);
            actions.push(Action::RequestSeek(ts));
            actions.push(Action::Go(View::Timeline));
        }

        ui.label(
            egui::RichText::new("drag to zoom window in Timeline · click to jump to timestamp")
                .small()
                .color(theme.text_dim),
        );
    });
}

fn render_categories_card(ui: &mut egui::Ui, ctx: &ViewCtx<'_>, actions: &mut Vec<Action>) {
    let model = ctx.model;
    let theme = ctx.theme;
    card_frame(ui, theme, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("BY CATEGORY").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new("click to scope & open Timeline")
                        .small()
                        .color(theme.text_dim),
                );
            });
        });
        ui.add_space(4.0);

        let Some(counts) = model.category.value() else {
            ui.label(egui::RichText::new("counting categories...").color(theme.text_dim));
            return;
        };
        let max_count = counts.iter().copied().max().unwrap_or(0).max(1);
        let filter = model.scope.filter();
        let multi = ui.input(|i| i.modifiers.ctrl);

        for (idx, cat) in Category::ALL.iter().enumerate() {
            let count = counts[idx];
            let color = theme.category_color(*cat);
            ui.horizontal(|ui| {
                let (swatch_rect, _) =
                    ui.allocate_exact_size(egui::vec2(10.0, 18.0), egui::Sense::hover());
                ui.painter().rect_filled(
                    egui::Rect::from_center_size(swatch_rect.center(), egui::vec2(9.0, 9.0)),
                    2.0,
                    color,
                );

                let btn = ui
                    .allocate_ui_with_layout(
                        egui::vec2(CATEGORY_LABEL_WIDTH, 18.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_min_width(CATEGORY_LABEL_WIDTH);
                            ui.add(egui::Button::new(cat.label()).frame(false))
                        },
                    )
                    .inner
                    .on_hover_text("click to filter to this category in Timeline");
                if btn.clicked() {
                    let (kinds, kinds_not) = apply_category_click(&filter, *cat, multi, false);
                    actions.push(Action::Scope(ScopeAction::SetKindFilter {
                        kinds,
                        kinds_not,
                    }));
                    actions.push(Action::Go(View::Timeline));
                }

                let bar_avail = (ui.available_width() - 64.0).max(24.0);
                let (bar_rect, bar_resp) =
                    ui.allocate_exact_size(egui::vec2(bar_avail, 12.0), egui::Sense::click());
                ui.painter().rect_filled(bar_rect, 2.0, theme.bg0);
                if count > 0 {
                    let frac = (count as f32 / max_count as f32).clamp(0.04, 1.0);
                    let target_width = (bar_rect.width() * frac).max(3.0);
                    let width = if theme.reduced_motion {
                        target_width
                    } else {
                        ui.ctx().animate_value_with_time(
                            ui.scope_id().with(("category-bar", idx)),
                            target_width,
                            0.28,
                        )
                    };
                    let mut filled = bar_rect;
                    filled.set_width(width);
                    ui.painter().rect_filled(filled, 2.0, color);
                }
                if bar_resp.clicked() {
                    let (kinds, kinds_not) = apply_category_click(&filter, *cat, multi, false);
                    actions.push(Action::Scope(ScopeAction::SetKindFilter {
                        kinds,
                        kinds_not,
                    }));
                    actions.push(Action::Go(View::Timeline));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.monospace(thousands(count));
                });
            });
        }
    });
}

fn render_top_hosts_card(ui: &mut egui::Ui, ctx: &ViewCtx<'_>, actions: &mut Vec<Action>) {
    let model = ctx.model;
    let theme = ctx.theme;
    let active_window = active_window_bounds(model);
    card_frame(ui, theme, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("TOP HOSTS").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.link("all hosts ->").clicked() {
                    actions.push(Action::Go(View::Hosts));
                }
            });
        });
        ui.add_space(4.0);

        let Some(hosts) = model.dashboard_hosts.value() else {
            ui.label(egui::RichText::new("loading hosts...").color(theme.text_dim));
            return;
        };
        if hosts.is_empty() {
            ui.label(egui::RichText::new("no subject_host values in scope").color(theme.text_dim));
            return;
        }
        for host in hosts.iter().take(8) {
            ui.horizontal(|ui| {
                if ui
                    .link(host.host.as_str())
                    .on_hover_text("scope Timeline to this host")
                    .clicked()
                {
                    actions.push(Action::Scope(ScopeAction::SetHosts(vec![
                        host.host_key().to_string(),
                    ])));
                    actions.push(Action::Go(View::Timeline));
                }
                if host.is_new_in_window(active_window) {
                    ui.label(
                        egui::RichText::new("[NEW]")
                            .small()
                            .strong()
                            .color(theme.warn),
                    );
                }
                if host.has_ioc {
                    ui.label(
                        egui::RichText::new("[IOC]")
                            .small()
                            .strong()
                            .color(theme.ioc),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.monospace(thousands(host.events));
                });
            });
        }
    });
}

fn render_worth_a_look(ui: &mut egui::Ui, ctx: &ViewCtx<'_>, actions: &mut Vec<Action>) {
    let model = ctx.model;
    let theme = ctx.theme;
    let active_window = active_window_bounds(model);
    let summary = model.dashboard_summary.value();
    let hosts = model
        .dashboard_hosts
        .value()
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    card_frame(ui, theme, |ui| {
        ui.label(egui::RichText::new("WORTH A LOOK").strong());
        ui.add_space(4.0);

        let Some(summary) = summary else {
            ui.label(egui::RichText::new("scanning case highlights...").color(theme.text_dim));
            return;
        };

        let mut shown_any = false;
        shown_any |= render_ioc_finding(ui, theme, summary, actions);
        shown_any |= render_state_finding(ui, theme, summary, actions);
        shown_any |= render_new_hosts_findings(ui, theme, hosts, active_window, actions);
        shown_any |= render_source_findings(ui, theme, summary, actions);
        shown_any |= render_diff_finding(ui, theme, summary, actions);

        if !shown_any {
            ui.label(
                egui::RichText::new(
                    "No IOC hits, flagged state, new hosts, or source issues detected.",
                )
                .color(theme.text_dim),
            );
        }
    });
}

fn render_ioc_finding(
    ui: &mut egui::Ui,
    theme: &Theme,
    summary: &DashboardSummaryVm,
    actions: &mut Vec<Action>,
) -> bool {
    if summary.ioc_event_hits == 0 {
        return false;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new("[IOC]")
                .small()
                .strong()
                .color(theme.ioc),
        );
        let hosts_preview = if summary.sample_ioc_hosts.is_empty() {
            String::new()
        } else {
            let list = summary
                .sample_ioc_hosts
                .iter()
                .map(|h| h.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            format!(" ({list})")
        };
        if ui
            .link(format!(
                "{} IOC-matched events{hosts_preview}",
                thousands(summary.ioc_event_hits)
            ))
            .clicked()
        {
            actions.push(Action::Scope(ScopeAction::SetIocOnly(true)));
            actions.push(Action::Go(View::Timeline));
        }
    });
    true
}

fn render_state_finding(
    ui: &mut egui::Ui,
    theme: &Theme,
    summary: &DashboardSummaryVm,
    actions: &mut Vec<Action>,
) -> bool {
    if summary.flagged_state_count == 0 {
        return false;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new("[STATE]")
                .small()
                .strong()
                .color(theme.warn),
        );
        let kinds = summary
            .flagged_state_kinds
            .iter()
            .take(3)
            .map(|k| k.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let suffix = if kinds.is_empty() {
            String::new()
        } else {
            format!(" ({kinds})")
        };
        if ui
            .link(format!(
                "{} flagged state records{suffix}",
                thousands(summary.flagged_state_count)
            ))
            .clicked()
        {
            actions.push(Action::Go(View::State));
        }
    });
    true
}

fn render_new_hosts_findings(
    ui: &mut egui::Ui,
    theme: &Theme,
    hosts: &[HostSummaryVm],
    active_window: Option<(i64, i64)>,
    actions: &mut Vec<Action>,
) -> bool {
    let mut any = false;
    for host in hosts
        .iter()
        .filter(|h| h.is_new_in_window(active_window))
        .take(3)
    {
        any = true;
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new("[NEW HOST]")
                    .small()
                    .strong()
                    .color(theme.warn),
            );
            if ui
                .link(format!(
                    "{} first appeared inside the active window ({} events)",
                    host.host.as_str(),
                    thousands(host.events)
                ))
                .clicked()
            {
                actions.push(Action::Scope(ScopeAction::SetHosts(vec![
                    host.host_key().to_string(),
                ])));
                actions.push(Action::Go(View::Timeline));
            }
        });
    }
    any
}

fn render_source_findings(
    ui: &mut egui::Ui,
    theme: &Theme,
    summary: &DashboardSummaryVm,
    actions: &mut Vec<Action>,
) -> bool {
    if summary.source_warnings.is_empty() {
        return false;
    }
    for issue in summary.source_warnings.iter().take(4) {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new("[SOURCE]")
                    .small()
                    .strong()
                    .color(theme.warn),
            );
            if ui
                .link(format!(
                    "{}: {}",
                    issue.label.as_str(),
                    issue.status.as_str()
                ))
                .clicked()
            {
                actions.push(Action::Go(View::Sources));
            }
        });
    }
    if summary.source_warnings.len() > 4 {
        let extra = summary.source_warnings.len() - 4;
        if ui
            .link(format!("+{extra} more source warnings ->"))
            .clicked()
        {
            actions.push(Action::Go(View::Sources));
        }
    }
    true
}

fn render_diff_finding(
    ui: &mut egui::Ui,
    theme: &Theme,
    summary: &DashboardSummaryVm,
    actions: &mut Vec<Action>,
) -> bool {
    let Some((run_a, run_b, added, removed, changed)) = summary.last_diff else {
        return false;
    };
    if added == 0 && removed == 0 && changed == 0 {
        return false;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(
            egui::RichText::new("[DIFF]")
                .small()
                .strong()
                .color(theme.accent),
        );
        if ui
            .link(format!(
                "Run #{run_a} -> #{run_b}: +{added} / -{removed} / ~{changed} state changes"
            ))
            .clicked()
        {
            actions.push(Action::Go(View::Diff));
        }
    });
    true
}

fn active_window_bounds(model: &crate::model::Model) -> Option<(i64, i64)> {
    let filter = model.scope.filter();
    let case_bounds = model.bounds.value().copied().flatten();
    match (filter.since, filter.until, case_bounds) {
        (Some(s), Some(u), _) => Some((s.min(u), s.max(u))),
        (Some(s), None, Some((_, ce))) => Some((s, ce.max(s))),
        (None, Some(u), Some((cs, _))) => Some((cs.min(u), u)),
        _ => None,
    }
}

fn card_frame(ui: &mut egui::Ui, theme: &Theme, body: impl FnOnce(&mut egui::Ui)) {
    kit::card(ui, theme, body);
}

fn format_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = n as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}
