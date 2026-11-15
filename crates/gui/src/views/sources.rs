//! Sources view: coverage cards with hints, the per-source health matrix across
//! runs, and the runs table with durations, first-stored counts, and errors.

use eframe::egui;
use fp_core::Availability;
use fp_query::CoverageStatus;

use crate::model::{Action, LoadStatus, Selection, View};
use crate::theme::Theme;
use crate::vm::SourcesDataVm;
use crate::widgets::ViewCtx;
use crate::widgets::chip::thousands;

pub fn show_sources(ui: &mut egui::Ui, ctx: &ViewCtx<'_>, actions: &mut Vec<Action>) {
    ui.label(egui::RichText::new("Coverage").strong());
    ui.add_space(4.0);

    let status = ctx.model.sources.status(ctx.model.generation);
    let Some(data) = ctx.model.sources.value() else {
        match status {
            LoadStatus::Error(err) => {
                ui.colored_label(ctx.theme.danger, format!("Failed to load coverage: {err}"));
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("reading sources...");
                });
            }
        }
        return;
    };

    render_coverage_cards(ui, ctx.theme, data);
    ui.add_space(10.0);
    render_health_matrix(ui, ctx, data, actions);
    ui.add_space(10.0);
    render_runs_table(ui, ctx, actions);
}

fn render_coverage_cards(ui: &mut egui::Ui, theme: &Theme, data: &SourcesDataVm) {
    ui.horizontal_wrapped(|ui| {
        for card in &data.coverage {
            egui::Frame::group(ui.style())
                .fill(theme.bg1)
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::symmetric(10, 8))
                .show(ui, |ui| {
                    ui.set_width(220.0);
                    ui.label(egui::RichText::new(card.category).strong().small());
                    ui.horizontal(|ui| {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                        paint_status_disc(ui.painter(), rect.center(), 5.0, card.status, theme);
                        let label = match card.status {
                            CoverageStatus::Covered => "covered",
                            CoverageStatus::Partial => "partial",
                            CoverageStatus::Gap => "gap",
                        };
                        ui.label(
                            egui::RichText::new(label)
                                .small()
                                .color(status_color(card.status, theme)),
                        );
                    });
                    for line in &card.answering {
                        ui.label(egui::RichText::new(line.as_str()).small());
                    }
                    for line in &card.notes {
                        ui.label(egui::RichText::new(line.as_str()).small().weak());
                    }
                });
        }
    });
}

fn render_health_matrix(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    data: &SourcesDataVm,
    actions: &mut Vec<Action>,
) {
    let theme = ctx.theme;
    ui.label(egui::RichText::new("Source health by run").strong());
    ui.add_space(4.0);
    egui::ScrollArea::both()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            egui::Grid::new("source_health_grid").show(ui, |ui| {
                ui.label(
                    egui::RichText::new("SOURCE")
                        .small()
                        .strong()
                        .color(theme.text_dim),
                );
                for run in &data.run_ids {
                    ui.label(
                        egui::RichText::new(format!("run {run}"))
                            .small()
                            .strong()
                            .color(theme.text_dim),
                    );
                }
                ui.end_row();
                for row in &data.health_rows {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}  ({})",
                            row.label.as_str(),
                            row.kind.as_str()
                        ))
                        .small(),
                    );
                    for (run_idx, cell) in row.cells.iter().enumerate() {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(28.0, 16.0), egui::Sense::click());
                        if let Some(avail) = cell {
                            paint_availability(ui.painter(), rect, avail, theme);
                            let text = format!(
                                "{} — run {}: {}",
                                row.label.as_str(),
                                data.run_ids[run_idx],
                                avail
                            );
                            let resp = ui.interact(
                                rect,
                                ui.scope_id().with((row.source_id, run_idx)),
                                egui::Sense::click(),
                            );
                            let clicked = resp.clicked();
                            let resp = resp.on_hover_text(text);
                            let _ = resp;
                            if clicked {
                                actions.push(Action::Select(Selection::Source(row.source_id)));
                            }
                        } else {
                            ui.painter().circle_filled(
                                rect.center(),
                                1.5,
                                theme.text_dim.gamma_multiply(0.5),
                            );
                        }
                    }
                    ui.end_row();
                }
            });
        });
}

fn render_runs_table(ui: &mut egui::Ui, ctx: &ViewCtx<'_>, actions: &mut Vec<Action>) {
    let theme = ctx.theme;
    let Some(runs) = ctx.model.runs.value() else {
        return;
    };
    ui.label(egui::RichText::new("Runs").strong());
    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .id_salt("runs_table")
        .auto_shrink([false, false])
        .max_height(240.0)
        .show(ui, |ui| {
            for run in runs.iter().rev() {
                ui.horizontal(|ui| {
                    ui.monospace(format!("run {:<3}", run.id));
                    ui.monospace(fp_query::format_time(
                        run.started_utc,
                        "second",
                        ctx.tz,
                        "%d %b %H:%M",
                    ));
                    let duration = run
                        .duration_utc
                        .map(|us| format!("{} s", (us / 1_000_000).max(1)))
                        .unwrap_or_else(|| "running…".to_string());
                    ui.monospace(duration);
                    ui.label(
                        egui::RichText::new(format!("{} new", thousands(run.events_new))).small(),
                    );
                    if run.error_count > 0 {
                        ui.label(
                            egui::RichText::new(format!("{} errors", run.error_count))
                                .small()
                                .color(theme.danger),
                        );
                    }
                    if let Some(args) = &run.args {
                        ui.label(egui::RichText::new(args.as_str()).small().weak());
                    }
                    if run.id > 1
                        && ui
                            .small_button("diff vs previous")
                            .on_hover_text("open the diff of this run against the previous one")
                            .clicked()
                    {
                        actions.push(Action::SetDiffRuns(run.id - 1, run.id));
                        actions.push(Action::Go(View::Diff));
                    }
                    if ui
                        .small_button("events first stored")
                        .on_hover_text("scope the timeline to what this run added")
                        .clicked()
                    {
                        actions.push(Action::Scope(crate::model::ScopeAction::SetRun(Some(
                            run.id,
                        ))));
                        actions.push(Action::Go(View::Timeline));
                    }
                });
            }
        });
}

pub fn paint_status_disc(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    status: CoverageStatus,
    theme: &Theme,
) {
    let full = status == CoverageStatus::Covered;
    let hollow = status == CoverageStatus::Gap;
    let color = status_color(status, theme);
    if hollow {
        painter.circle(
            center,
            radius,
            egui::Color32::TRANSPARENT,
            egui::Stroke::new(1.5, color),
        );
    } else if full {
        painter.circle_filled(center, radius, color);
    } else {
        // half disc: clip like the confidence glyphs
        let clip = egui::Rect::from_min_max(
            center - egui::Vec2::splat(radius),
            center + egui::Vec2::new(0.0, radius),
        );
        painter
            .with_clip_rect(clip)
            .circle_filled(center, radius, color);
        painter.circle(
            center,
            radius,
            egui::Color32::TRANSPARENT,
            egui::Stroke::new(1.0, color),
        );
    }
}

fn status_color(status: CoverageStatus, theme: &Theme) -> egui::Color32 {
    match status {
        CoverageStatus::Covered => theme.ok,
        CoverageStatus::Partial => theme.warn,
        CoverageStatus::Gap => theme.text_dim,
    }
}

fn availability_color(avail: &Availability, theme: &Theme) -> egui::Color32 {
    match avail {
        Availability::Present => theme.ok,
        Availability::Partial => theme.warn,
        Availability::Unreadable { .. } | Availability::Unsupported { .. } => theme.warn,
        Availability::Error(_) => theme.danger,
        Availability::NotInstalled | Availability::Empty => theme.text_dim,
    }
}

fn paint_availability(
    painter: &egui::Painter,
    rect: egui::Rect,
    avail: &Availability,
    theme: &Theme,
) {
    let color = availability_color(avail, theme);
    match avail {
        Availability::Present => {
            painter.rect_filled(rect.shrink2(egui::vec2(6.0, 2.0)), 2.0, color);
        }
        Availability::Partial
        | Availability::Unreadable { .. }
        | Availability::Unsupported { .. } => {
            let cell = rect.shrink2(egui::vec2(6.0, 2.0));
            painter.rect_filled(
                egui::Rect::from_min_max(cell.min, egui::pos2(cell.center().x, cell.max.y)),
                2.0,
                color,
            );
            painter.rect_stroke(
                cell,
                2.0,
                egui::Stroke::new(1.0, color),
                egui::StrokeKind::Inside,
            );
        }
        Availability::NotInstalled | Availability::Empty => {
            painter.circle_filled(rect.center(), 1.5, color);
        }
        Availability::Error(_) => {
            let cell = rect.shrink2(egui::vec2(6.0, 2.0));
            let stroke = egui::Stroke::new(2.0, color);
            painter.line_segment([cell.left_top(), cell.right_bottom()], stroke);
            painter.line_segment([cell.right_top(), cell.left_bottom()], stroke);
        }
    }
}
