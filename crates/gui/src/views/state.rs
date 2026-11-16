//! State view: how things are — state records grouped by kind, with flagged-only
//! and text filters, a run selector, and integrity/flag glyphs in the table.

use std::collections::BTreeMap;

use eframe::egui;
use egui_extras::{Column, Size, StripBuilder, TableBuilder};

use crate::model::{Action, LoadStatus, Selection};
use crate::theme::Theme;
use crate::vm::{SafeJson, StateRowVm, state_flag_help};
use crate::widgets::ViewCtx;
use crate::widgets::badge::{StatusGlyph, status_mark};
use crate::widgets::kit;

#[derive(Default)]
pub struct StateViewState {
    /// `None` means the latest run
    pub run: Option<u64>,
    pub search: String,
    pub submitted_search: String,
    pub flagged_only: bool,
    pub selected_kind: Option<String>,
}

pub fn show_state(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    state: &mut StateViewState,
    actions: &mut Vec<Action>,
) {
    render_toolbar(ui, ctx, state, actions);
    ui.add_space(4.0);

    let model = ctx.model;
    let status = model.states.status(model.states_gen);
    let Some(rows) = model.states.value() else {
        match status {
            LoadStatus::Error(err) => {
                ui.colored_label(ctx.theme.danger, format!("Failed to load state: {err}"));
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("reading state records...");
                });
            }
        }
        return;
    };

    if rows.is_empty() {
        kit::empty_state(
            ui,
            ctx.theme,
            "no state records for this run and filter",
            "clear the filter or pick another run; collect captures permissions, extensions and startup items",
        );
        return;
    }

    let visible: Vec<&StateRowVm> = rows
        .iter()
        .filter(|r| !state.flagged_only || !r.flags.is_empty())
        .collect();

    // a strip gives both panes the full remaining height, which `horizontal` would not
    let tree_width = (ui.available_width() * 0.22).clamp(150.0, 230.0);
    StripBuilder::new(ui)
        .size(Size::exact(tree_width))
        .size(Size::remainder().at_least(220.0))
        .horizontal(|mut strip| {
            strip.cell(|ui| render_kind_tree(ui, ctx.theme, &visible, state));
            strip.cell(|ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: 10,
                        ..Default::default()
                    })
                    .show(ui, |ui| {
                        render_rows_table(ui, ctx, &visible, state, actions)
                    });
            });
        });
}

fn render_toolbar(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    state: &mut StateViewState,
    actions: &mut Vec<Action>,
) {
    let model = ctx.model;
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("State").strong());
        ui.add_space(8.0);

        ui.label("Run:");
        let runs = model.runs.value().map(Vec::as_slice).unwrap_or(&[]);
        let picker = egui::ComboBox::from_id_salt("state_run_picker")
            .width(180.0)
            .selected_text(run_label(state.run, runs, ctx.tz))
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(state.run.is_none(), "last run")
                    .clicked()
                {
                    state.run = None;
                    actions.push(Action::RequestStateQuery);
                }
                for run in runs.iter().rev() {
                    let label = format!(
                        "run {} · {}",
                        run.id,
                        fp_query::format_time(run.started_utc, "second", ctx.tz, "%d %b %H:%M")
                    );
                    if ui
                        .selectable_label(state.run == Some(run.id), label)
                        .clicked()
                    {
                        state.run = Some(run.id);
                        actions.push(Action::RequestStateQuery);
                    }
                }
            });
        picker
            .response
            .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, "Run"));

        ui.add_space(8.0);
        ui.checkbox(&mut state.flagged_only, "flagged only")
            .on_hover_text("show only records that carry at least one flag (client-side filter)");

        ui.add_space(8.0);
        ui.label("Filter:");
        let search_resp = ui.add(
            egui::TextEdit::singleline(&mut state.search)
                .desired_width(200.0)
                .hint_text("key substring, Enter to apply"),
        );
        let enter = search_resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if enter {
            let trimmed = state.search.trim().to_string();
            if trimmed != state.submitted_search {
                state.submitted_search = trimmed;
                actions.push(Action::RequestStateQuery);
            }
        }
        if !state.search.is_empty() && ui.small_button("clear").clicked() {
            state.search.clear();
            if !state.submitted_search.is_empty() {
                state.submitted_search.clear();
                actions.push(Action::RequestStateQuery);
            }
        }
    });
}

fn render_kind_tree(
    ui: &mut egui::Ui,
    theme: &Theme,
    visible: &[&StateRowVm],
    state: &mut StateViewState,
) {
    let mut kinds: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for row in visible {
        let entry = kinds.entry(row.kind.as_str()).or_default();
        entry.0 += 1;
        if !row.flags.is_empty() {
            entry.1 += 1;
        }
    }
    ui.vertical(|ui| {
        ui.label(
            egui::RichText::new("KINDS")
                .small()
                .strong()
                .color(theme.text_dim),
        );
        egui::ScrollArea::vertical()
            .id_salt("state_kind_tree")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let all_selected = state.selected_kind.is_none();
                let total: usize = kinds.values().map(|(count, _)| count).sum();
                if ui
                    .selectable_label(all_selected, format!("all ({total})"))
                    .clicked()
                {
                    state.selected_kind = None;
                }
                for (kind, (count, flagged)) in kinds {
                    let selected = state.selected_kind.as_deref() == Some(kind);
                    let mut text = format!("{kind}  {count}");
                    if flagged > 0 {
                        // ⚑ has no bundled-font glyph; the exclamation keeps it ASCII-safe
                        text.push_str(&format!("  !{flagged}"));
                    }
                    if ui.selectable_label(selected, text).clicked() {
                        state.selected_kind = Some(kind.to_string());
                    }
                }
            });
    });
}

fn render_rows_table(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    visible: &[&StateRowVm],
    state: &mut StateViewState,
    actions: &mut Vec<Action>,
) {
    let theme = ctx.theme;
    let rows: Vec<&StateRowVm> = visible
        .iter()
        .copied()
        .filter(|r| {
            state
                .selected_kind
                .as_deref()
                .is_none_or(|k| r.kind.as_str() == k)
        })
        .collect();
    if rows.is_empty() {
        kit::empty_state(
            ui,
            theme,
            "no records in this kind",
            "pick another kind, or switch the flagged-only filter off",
        );
        return;
    }

    let show_decision = rows.iter().any(|r| field_string(r, "decision").is_some());
    let show_integrity = rows.iter().any(|r| r.integrity.is_some());
    let show_pkg = rows.iter().any(|r| field_string(r, "owner_pkg").is_some());
    let current_selection = ctx.model.selection();

    let row_h = 24.0;
    let mut columns: Vec<Column> = vec![
        Column::initial(280.0)
            .at_least(140.0)
            .resizable(true)
            .clip(true),
    ];
    if show_decision {
        columns.push(Column::initial(90.0).at_least(70.0).clip(true));
    }
    if show_integrity {
        columns.push(Column::initial(96.0).at_least(80.0).clip(true));
    }
    if show_pkg {
        columns.push(Column::initial(110.0).at_least(80.0).clip(true));
    }
    columns.push(Column::remainder().at_least(120.0).clip(true));

    ui.scope(|ui| {
        ui.style_mut().interaction.selectable_labels = false;
        let mut builder = TableBuilder::new(ui)
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
        for column in columns {
            builder = builder.column(column);
        }
        builder
            .header(row_h, |mut header| {
                let head = |title: &str, ui: &mut egui::Ui| {
                    ui.label(
                        egui::RichText::new(title)
                            .small()
                            .strong()
                            .color(theme.text_dim),
                    );
                };
                header.col(|ui| head("KEY", ui));
                if show_decision {
                    header.col(|ui| head("DECISION", ui));
                }
                if show_integrity {
                    header.col(|ui| head("INTEGRITY", ui));
                }
                if show_pkg {
                    header.col(|ui| head("PACKAGE", ui));
                }
                header.col(|ui| head("DETAILS", ui));
            })
            .body(|body| {
                body.rows(row_h, rows.len(), |mut row_ui| {
                    let row = rows[row_ui.index()];
                    let selected =
                        matches!(current_selection, Selection::State(id) if id == row.rowid);
                    row_ui.set_selected(selected);

                    row_ui.col(|ui| {
                        let font = egui::TextStyle::Monospace.resolve(ui.style());
                        let shown = kit::middle_ellipsis(
                            ui,
                            row.key.as_str(),
                            &font,
                            ui.available_width() - 8.0,
                        );
                        let truncated = shown != row.key.as_str();
                        let label = ui.label(egui::RichText::new(shown).monospace());
                        if truncated {
                            label.on_hover_text(row.key.as_str());
                        }
                    });
                    if show_decision {
                        row_ui.col(|ui| {
                            let text = field_string(row, "decision").unwrap_or_default();
                            ui.label(egui::RichText::new(text).small());
                        });
                    }
                    if show_integrity {
                        row_ui.col(|ui| {
                            ui.horizontal(|ui| {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(16.0, 16.0),
                                    egui::Sense::hover(),
                                );
                                if let Some(verdict) = row.integrity.as_ref().map(|s| s.as_str())
                                    && let Some(glyph) = integrity_glyph(verdict)
                                {
                                    status_mark(
                                        ui.painter(),
                                        rect.center(),
                                        5.0,
                                        glyph,
                                        glyph.color(theme),
                                    );
                                }
                                if let Some(integ) = &row.integrity {
                                    ui.label(egui::RichText::new(integ.as_str()).small());
                                }
                            });
                        });
                    }
                    if show_pkg {
                        row_ui.col(|ui| {
                            let text = field_string(row, "owner_pkg").unwrap_or_default();
                            ui.label(egui::RichText::new(text).small());
                        });
                    }
                    row_ui.col(|ui| {
                        let details = if row.details.is_empty() {
                            "-".to_string()
                        } else {
                            row.details.join("; ")
                        };
                        ui.add(egui::Label::new(egui::RichText::new(details).small()).truncate());
                    });

                    if row_ui.response().clicked() {
                        actions.push(Action::Select(Selection::State(row.rowid)));
                    }
                });
            });
    });
}

fn run_label(selected: Option<u64>, runs: &[crate::vm::RunMarkerVm], tz: fp_query::Tz) -> String {
    match selected {
        None => "last run".to_string(),
        Some(id) => runs
            .iter()
            .find(|r| r.id == id)
            .map(|r| {
                format!(
                    "run {} · {}",
                    r.id,
                    fp_query::format_time(r.started_utc, "second", tz, "%d %b %H:%M")
                )
            })
            .unwrap_or_else(|| format!("run {id}")),
    }
}

fn field_string<'a>(row: &'a StateRowVm, field: &str) -> Option<&'a str> {
    match &row.value {
        SafeJson::Object(entries) => {
            entries
                .iter()
                .find(|(k, _)| k.as_str() == field)
                .and_then(|(_, v)| match v {
                    SafeJson::String(s) => Some(s.as_str()),
                    _ => None,
                })
        }
        _ => None,
    }
}

/// Integrity verdict as a fixed-vocabulary glyph; unknown verdicts render no mark.
pub fn integrity_glyph(verdict: &str) -> Option<StatusGlyph> {
    match verdict {
        "ok" => Some(StatusGlyph::Ok),
        "mismatch" => Some(StatusGlyph::Mismatch),
        "missing" => Some(StatusGlyph::Missing),
        "unowned" => Some(StatusGlyph::Unowned),
        "unverified" => Some(StatusGlyph::Unverified),
        _ => None,
    }
}

/// Plain-language lines for the integrity verdict and every flag on a row;
/// a word already explained via the verdict is not repeated.
pub fn explain_row(row: &StateRowVm) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    if let Some(integ) = &row.integrity
        && let Some(help) = state_flag_help(integ.as_str())
    {
        seen.push(integ.as_str());
        out.push(format!("{} — {help}", integ.as_str()));
    }
    for flag in &row.flags {
        if seen.contains(&flag.as_str()) {
            continue;
        }
        if let Some(help) = state_flag_help(flag.as_str()) {
            seen.push(flag.as_str());
            out.push(format!("{} — {help}", flag.as_str()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{SafeJson, SafeText};

    fn row_with(value: serde_json::Value) -> StateRowVm {
        let raw = value.to_string();
        StateRowVm {
            rowid: 1,
            run_id: 2,
            kind: SafeText::new("persist.systemd"),
            key: SafeText::new("/etc/systemd/system/foo.service"),
            value: SafeJson::from_value(&value),
            raw: SafeText::new(&raw),
            details: Vec::new(),
            flags: value
                .get("flags")
                .and_then(|f| f.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.as_str())
                        .map(SafeText::new)
                        .collect()
                })
                .unwrap_or_default(),
            integrity: value
                .get("integrity")
                .and_then(|i| i.as_str())
                .map(SafeText::new),
            locator: None,
        }
    }

    #[test]
    fn integrity_glyph_covers_the_fixed_vocabulary() {
        assert!(integrity_glyph("ok").is_some());
        assert!(integrity_glyph("mismatch").is_some());
        assert!(integrity_glyph("missing").is_some());
        assert!(integrity_glyph("unowned").is_some());
        assert!(integrity_glyph("unverified").is_some());
        assert!(integrity_glyph("sideways").is_none());
    }

    #[test]
    fn explain_row_gives_plain_language_for_known_words() {
        let row = row_with(serde_json::json!({
            "integrity": "unowned",
            "flags": ["unowned", "mtime_in_window", "whatever_new"],
        }));
        let lines = explain_row(&row);
        // the unowned flag repeats the verdict, so it is explained once;
        // unknown words get no line of their own
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("no installed package"));
        assert!(lines[1].contains("mtime_in_window"));
        assert!(!lines.iter().any(|l| l.contains("whatever_new")));
        assert!(!lines[1].contains("no installed package"));
    }

    #[test]
    fn field_string_reads_string_fields_only() {
        let row = row_with(serde_json::json!({
            "decision": "grant",
            "active": true,
        }));
        assert_eq!(field_string(&row, "decision"), Some("grant"));
        assert_eq!(field_string(&row, "active"), None);
        assert_eq!(field_string(&row, "absent"), None);
    }
}
