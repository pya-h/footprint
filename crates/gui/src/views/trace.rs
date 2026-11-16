//! Trace view: the provenance chain of an artifact or event as a vertical
//! diagram — head card, confidence-shaped connectors with time deltas, and
//! collapsible redirect runs — plus a text view identical to `footprint trace`.

use eframe::egui;
use fp_core::Confidence;
use fp_correlate::trace::TraceArrow;

use crate::model::{Action, LoadStatus, Model, Selection, View};
use crate::theme::Theme;
use crate::vm::{TraceHeadVm, TraceRowVm, TraceVm};
use crate::widgets::ViewCtx;
use crate::widgets::badge::{ConfidenceGlyph, confidence_disc};
use crate::widgets::chip::thousands;
use crate::widgets::kit;

const GUTTER_W: f32 = 22.0;
const REDIRECT_REL: &str = "redirected_from";

#[derive(Debug, Default)]
pub struct TraceState {
    pub show_text: bool,
    pub draft: String,
    pub redirects_open: bool,
}

pub fn show_trace(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    state: &mut TraceState,
    actions: &mut Vec<Action>,
) {
    show_toolbar(ui, ctx.model, state, actions);

    let status = ctx.model.trace.status(ctx.model.trace_gen);
    let Some(trace) = ctx.model.trace.value() else {
        match status {
            LoadStatus::Error(err) => {
                ui.add_space(12.0);
                ui.colored_label(ctx.theme.danger, format!("trace failed: {err}"));
            }
            _ if ctx.model.pending_trace.is_none() => {
                kit::empty_state(
                    ui,
                    ctx.theme,
                    "trace where something came from",
                    "type an artifact path, URL, or event id above to see its provenance chain",
                );
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("or select an event in the Timeline and press t")
                            .small()
                            .color(ctx.theme.text_dim),
                    );
                });
            }
            _ => {
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("building trace…");
                });
            }
        }
        return;
    };

    if state.show_text {
        let text = render_trace_text(trace, &|ts| {
            fp_query::format_time(ts, "second", ctx.tz, "%H:%M:%S")
        });
        if ui.button("copy text").clicked() {
            ui.ctx().copy_text(text.clone());
        }
        ui.add_space(4.0);
        // the bundled fonts lack ◄/►; the display substitutes ASCII arrows while
        // the copy button still carries the exact `footprint trace` characters
        let display = text.replace('◄', "<").replace('►', ">");
        egui::ScrollArea::vertical()
            .id_salt("trace_text_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut display.as_str())
                        .font(egui::TextStyle::Monospace)
                        .desired_width(f32::INFINITY),
                );
            });
    } else {
        egui::ScrollArea::vertical()
            .id_salt("trace_chain_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                show_chain(ui, ctx.theme, ctx.tz, trace, state, actions);
            });
    }
}

fn show_toolbar(
    ui: &mut egui::Ui,
    model: &Model,
    state: &mut TraceState,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Trace").strong());
        let response = ui
            .add(
                egui::TextEdit::singleline(&mut state.draft)
                    .hint_text("path, URL, or event id…")
                    .desired_width(280.0),
            )
            .on_hover_text("artifact path, URL, or event id to trace from");
        let submit = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.button("trace").clicked() || submit) && !state.draft.trim().is_empty() {
            actions.push(Action::RequestTrace(state.draft.trim().to_string()));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let label = if state.show_text {
                "view: text"
            } else {
                "view: chain"
            };
            if ui
                .small_button(label)
                .on_hover_text("toggle between the chain diagram and the `footprint trace` text")
                .clicked()
            {
                state.show_text = !state.show_text;
            }
        });
    });
    if let Some(target) = &model.pending_trace {
        let safe = crate::vm::SafeText::new(target);
        ui.label(
            egui::RichText::new(format!("target: {safe}"))
                .small()
                .weak(),
        );
    }
}

fn show_chain(
    ui: &mut egui::Ui,
    theme: &Theme,
    tz: fp_query::Tz,
    trace: &TraceVm,
    state: &mut TraceState,
    actions: &mut Vec<Action>,
) {
    show_head_card(ui, theme, tz, &trace.head);

    let items = chain_items(&trace.rows, state.redirects_open);
    let mut prev_ts = head_ts(&trace.head);
    for item in &items {
        match item {
            ChainItem::Row(row) => {
                show_connector(
                    ui,
                    theme,
                    prev_ts,
                    row.ts_utc,
                    row.rel.as_str(),
                    row.confidence,
                );
                show_node(ui, theme, tz, row, actions);
                if let Some(ts) = row.ts_utc {
                    prev_ts = Some(ts);
                }
            }
            ChainItem::RedirectRun { rows } => {
                let Some(first) = rows.first() else {
                    continue;
                };
                show_connector(
                    ui,
                    theme,
                    prev_ts,
                    first.ts_utc,
                    first.rel.as_str(),
                    first.confidence,
                );
                if state.redirects_open {
                    for row in rows.iter() {
                        show_node(ui, theme, tz, row, actions);
                        if let Some(ts) = row.ts_utc {
                            prev_ts = Some(ts);
                        }
                    }
                } else if ui
                    .checkbox(
                        &mut state.redirects_open,
                        egui::RichText::new(format!("{} redirects", rows.len()))
                            .small()
                            .color(theme.accent),
                    )
                    .clicked()
                {
                    state.redirects_open = true;
                }
                if let Some(ts) = first.ts_utc {
                    prev_ts = Some(ts);
                }
            }
        }
    }
}

fn show_head_card(ui: &mut egui::Ui, theme: &Theme, tz: fp_query::Tz, head: &TraceHeadVm) {
    egui::Frame::group(ui.style())
        .fill(theme.bg1)
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| match head {
            TraceHeadVm::Artifact {
                path, sha256, size, ..
            } => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new("artifact")
                            .small()
                            .color(theme.text_dim),
                    );
                    ui.label(egui::RichText::new(path.as_str()).strong().monospace());
                });
                ui.horizontal_wrapped(|ui| {
                    if let Some(sha) = sha256 {
                        let short = sha.as_str().chars().take(12).collect::<String>();
                        ui.label(
                            egui::RichText::new(format!("sha256 {short}…"))
                                .small()
                                .weak(),
                        );
                        if ui.small_button("copy").clicked() {
                            ui.ctx().copy_text(sha.as_str().to_string());
                        }
                    }
                    if let Some(size) = size
                        && let Ok(bytes) = u64::try_from(*size)
                    {
                        ui.label(
                            egui::RichText::new(format!("{} B", thousands(bytes)))
                                .small()
                                .weak(),
                        );
                    }
                });
            }
            TraceHeadVm::Event {
                ts_utc,
                source,
                kind,
                subject,
                ..
            } => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(fp_query::format_time(
                            *ts_utc,
                            "exact",
                            tz,
                            "%Y-%m-%d %H:%M:%S",
                        ))
                        .monospace(),
                    );
                    ui.label(
                        egui::RichText::new(source.as_str())
                            .small()
                            .color(theme.text_dim),
                    );
                    ui.label(
                        egui::RichText::new(kind.as_str())
                            .small()
                            .color(theme.accent),
                    );
                });
                ui.label(subject.as_str());
            }
        });
}

fn show_connector(
    ui: &mut egui::Ui,
    theme: &Theme,
    prev_ts: Option<i64>,
    ts: Option<i64>,
    rel: &str,
    confidence: Option<Confidence>,
) {
    let (color, dash) = match confidence {
        Some(Confidence::Direct) => (theme.ok, 0.0f32),
        Some(Confidence::Derived) => (theme.warn, 4.0),
        _ => (theme.text_dim, 1.5),
    };
    let height = 26.0;
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::hover(),
    );
    let x = rect.left() + GUTTER_W;
    let from = egui::pos2(x, rect.top());
    let to = egui::pos2(x, rect.bottom());
    if dash > 0.0 {
        dashed_line(
            ui.painter(),
            from,
            to,
            egui::Stroke::new(1.5, color),
            dash,
            dash * 0.9,
        );
    } else {
        ui.painter()
            .line_segment([from, to], egui::Stroke::new(1.5, color));
    }
    let disc_center = egui::pos2(x, rect.top() + rect.height() / 2.0);
    if let Some(confidence) = confidence {
        confidence_disc(
            ui.painter(),
            disc_center,
            4.0,
            ConfidenceGlyph::from(confidence),
            color,
        );
    }
    let mut label = rel.to_string();
    if let (Some(prev), Some(ts)) = (prev_ts, ts) {
        if !label.is_empty() {
            label.push_str("  ·  ");
        }
        label.push_str(&delta_label(prev, ts));
    }
    if !label.is_empty() {
        ui.painter().text(
            egui::pos2(x + 12.0, rect.top() + rect.height() / 2.0),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(11.0),
            theme.text_dim,
        );
    }
}

fn show_node(
    ui: &mut egui::Ui,
    theme: &Theme,
    tz: fp_query::Tz,
    row: &TraceRowVm,
    actions: &mut Vec<Action>,
) {
    if row.arrow == TraceArrow::Note {
        ui.horizontal(|ui| {
            let pos = arrow_slot(ui);
            arrow_triangle(ui.painter(), pos, true, theme.text_dim);
            ui.label(egui::RichText::new(row.text.as_str()).small().weak());
        });
        return;
    }
    let backwards = row.arrow == TraceArrow::Back;
    let time = row
        .ts_utc
        .map(|ts| fp_query::format_time(ts, "exact", tz, "%Y-%m-%d %H:%M:%S"))
        .unwrap_or_default();
    let resp = ui
        .scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
            ui.horizontal(|ui| {
                ui.add_space(GUTTER_W * 2.0);
                let pos = arrow_slot(ui);
                arrow_triangle(ui.painter(), pos, backwards, theme.accent);
                ui.label(egui::RichText::new(time).monospace());
                ui.label(
                    egui::RichText::new(row.source.as_str())
                        .small()
                        .color(theme.text_dim),
                );
                ui.label(egui::RichText::new(row.text.as_str()));
            });
        })
        .response;
    if let Some(id) = row.event_id {
        if resp.double_clicked()
            && let Some(ts) = row.ts_utc
        {
            actions.push(Action::Go(View::Timeline));
            actions.push(Action::Select(Selection::Event(id)));
            actions.push(Action::RequestSeek(ts));
        } else if resp.clicked() {
            actions.push(Action::Select(Selection::Event(id)));
        }
        resp.on_hover_text("click: inspect · double-click: jump to timeline");
    }
}

/// Groups runs of consecutive redirect rows; runs of two or more collapse into
/// one header until the user expands them.
enum ChainItem<'a> {
    Row(&'a TraceRowVm),
    RedirectRun { rows: &'a [TraceRowVm] },
}

fn chain_items(rows: &[TraceRowVm], redirects_open: bool) -> Vec<ChainItem<'_>> {
    let mut items = Vec::new();
    let mut i = 0;
    while i < rows.len() {
        if rows[i].rel.as_str() == REDIRECT_REL {
            let mut j = i + 1;
            while j < rows.len() && rows[j].rel.as_str() == REDIRECT_REL {
                j += 1;
            }
            if j - i >= 2 && !redirects_open {
                items.push(ChainItem::RedirectRun { rows: &rows[i..j] });
            } else {
                items.extend(rows[i..j].iter().map(ChainItem::Row));
            }
            i = j;
        } else {
            items.push(ChainItem::Row(&rows[i]));
            i += 1;
        }
    }
    items
}

fn head_ts(head: &TraceHeadVm) -> Option<i64> {
    match head {
        TraceHeadVm::Event { ts_utc, .. } => Some(*ts_utc),
        TraceHeadVm::Artifact { .. } => None,
    }
}

fn delta_label(from: i64, to: i64) -> String {
    let gap = from.abs_diff(to);
    let secs = gap / 1_000_000;
    if secs >= 86_400 {
        format!("+{}d", secs / 86_400)
    } else if secs >= 3_600 {
        format!("+{}h", secs / 3_600)
    } else if secs >= 60 {
        format!("+{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("+{secs}s")
    }
}

fn dashed_line(
    painter: &egui::Painter,
    from: egui::Pos2,
    to: egui::Pos2,
    stroke: egui::Stroke,
    dash: f32,
    gap: f32,
) {
    let dir = to - from;
    let len = dir.length();
    if len == 0.0 {
        return;
    }
    let unit = dir / len;
    let mut cursor = 0.0f32;
    while cursor < len {
        let end = (cursor + dash).min(len);
        painter.line_segment([from + unit * cursor, from + unit * end], stroke);
        cursor = end + gap;
    }
}

/// Reserves the node gutter and returns where an edge arrow is painted; the bundled
/// fonts lack ◄/►, so the arrows are triangles like every other glyph in the GUI.
fn arrow_slot(ui: &mut egui::Ui) -> egui::Pos2 {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 18.0), egui::Sense::hover());
    rect.center()
}

fn arrow_triangle(painter: &egui::Painter, center: egui::Pos2, left: bool, color: egui::Color32) {
    let r = 5.0;
    let (tip, base) = if left {
        (egui::pos2(center.x - r, center.y), center.x + r)
    } else {
        (egui::pos2(center.x + r, center.y), center.x - r)
    };
    painter.add(egui::Shape::convex_polygon(
        vec![
            tip,
            egui::pos2(base, center.y - r * 0.8),
            egui::pos2(base, center.y + r * 0.8),
        ],
        color,
        egui::Stroke::NONE,
    ));
}

/// The exact text `footprint trace` prints, rebuilt from the sanitized view-model
/// so the copy/paste report form cannot carry hostile control characters.
pub fn render_trace_text(trace: &TraceVm, time: &dyn Fn(i64) -> String) -> String {
    let mut out = String::new();
    match &trace.head {
        TraceHeadVm::Artifact {
            path, sha256, size, ..
        } => {
            let sha_display = sha256
                .as_ref()
                .map(|h| {
                    let short: String = h.as_str().chars().take(8).collect();
                    format!("sha256 {short}…  ")
                })
                .unwrap_or_default();
            let size_display = size
                .and_then(|s| u64::try_from(s).ok())
                .map(|s| format!("{} B", thousands(s)))
                .unwrap_or_default();
            out.push_str(&format!("artifact  {path}  {sha_display}{size_display}\n"));
        }
        TraceHeadVm::Event {
            id,
            ts_utc,
            source,
            kind,
            subject,
            ..
        } => {
            out.push_str(&format!(
                "event {id}  {}  {source}  {kind}  {subject}\n",
                time(*ts_utc)
            ));
        }
    }
    for row in &trace.rows {
        let time = row
            .ts_utc
            .map(time)
            .unwrap_or_else(|| "        ".to_string());
        let arrow = match row.arrow {
            TraceArrow::Back | TraceArrow::Note => "◄",
            TraceArrow::Forward => "►",
        };
        if row.arrow == TraceArrow::Note {
            out.push_str(&format!(" {arrow} {}\n", row.text.as_str()));
        } else {
            out.push_str(&format!(
                " {arrow} {:<16} {time}  {:<16}  {}\n",
                row.rel.as_str(),
                row.source.as_str(),
                row.text.as_str()
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::SafeText;

    fn row(rel: &str, ts: Option<i64>, text: &str) -> TraceRowVm {
        TraceRowVm {
            arrow: TraceArrow::Back,
            rel: SafeText::new(rel),
            ts_utc: ts,
            source: SafeText::new("brave/Default"),
            text: SafeText::new(text),
            confidence: Some(Confidence::Direct),
            event_id: ts.map(|t| t as u64),
        }
    }

    #[test]
    fn consecutive_redirects_collapse_and_expand() {
        let rows = vec![
            row("downloaded_as", Some(100), "a"),
            row("redirected_from", Some(90), "r1"),
            row("redirected_from", Some(80), "r2"),
            row("redirected_from", Some(70), "r3"),
            row("navigated_from", Some(60), "b"),
        ];
        let collapsed = chain_items(&rows, false);
        assert_eq!(collapsed.len(), 3);
        assert!(matches!(collapsed[0], ChainItem::Row(_)));
        assert!(matches!(collapsed[1], ChainItem::RedirectRun { rows } if rows.len() == 3));
        assert!(matches!(collapsed[2], ChainItem::Row(_)));

        let open = chain_items(&rows, true);
        assert_eq!(open.len(), 5);
        assert!(open.iter().all(|i| matches!(i, ChainItem::Row(_))));
    }

    #[test]
    fn a_single_redirect_row_stays_visible() {
        let rows = vec![
            row("redirected_from", Some(90), "only"),
            row("x", None, "y"),
        ];
        let items = chain_items(&rows, false);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| matches!(i, ChainItem::Row(_))));
    }

    #[test]
    fn delta_label_picks_readable_units() {
        assert_eq!(delta_label(0, 42_000_000), "+42s");
        assert_eq!(delta_label(0, 96_000_000), "+1m 36s");
        assert_eq!(delta_label(0, 7_200_000_000), "+2h");
        assert_eq!(delta_label(0, 172_800_000_000), "+2d");
    }

    #[test]
    fn text_view_matches_cli_shape_and_sanitizes() {
        use fp_correlate::trace::{TraceHead, TraceRow, TraceTree};
        let tree = TraceTree {
            head: TraceHead::Event {
                id: 7,
                ts_utc: 1_760_647_493_000_000,
                source: "brave/Default".into(),
                kind: "web.visit".into(),
                subject: "visited \u{202e}evil".into(),
            },
            rows: vec![TraceRow {
                arrow: TraceArrow::Back,
                rel: "navigated_from".into(),
                ts_utc: Some(1_760_647_400_000_000),
                source: "brave/Default".into(),
                text: "redirect \u{0007}bell".into(),
                confidence: Some("derived".into()),
                event_id: Some(6),
            }],
        };
        let vm = TraceVm::from_tree(&tree);
        let cli = fp_correlate::trace::render_tree(&tree, &|_| "21:07:56".to_string());
        let gui = render_trace_text(&vm, &|_| "21:07:56".to_string());
        let normalize = |s: &str| s.replace(['\u{202e}', '\u{7}'], "");
        assert_eq!(gui, normalize(&cli));
        assert!(!gui.contains('\u{202e}'));
        assert!(!gui.contains('\u{7}'));
    }
}
