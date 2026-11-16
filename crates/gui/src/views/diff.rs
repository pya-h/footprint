//! Diff view: what changed between two runs — grouped entries with +/−/~
//! markers, count chips that double as filters, and an A/B value comparison
//! with changed leaves highlighted.

use std::collections::BTreeMap;

use eframe::egui;
use egui_extras::{Size, StripBuilder};
use fp_query::DiffChange;

use crate::model::{Action, LoadStatus};
use crate::theme::Theme;
use crate::vm::DiffEntryVm;
use crate::widgets::ViewCtx;
use crate::widgets::json_tree::show_json_diff;
use crate::widgets::kit;

pub struct DiffViewState {
    pub last_pair: Option<(u64, u64)>,
    pub show_added: bool,
    pub show_removed: bool,
    pub show_changed: bool,
    pub selected: Option<usize>,
}

impl Default for DiffViewState {
    fn default() -> Self {
        Self {
            last_pair: None,
            // the chips double as filters; everything starts visible
            show_added: true,
            show_removed: true,
            show_changed: true,
            selected: None,
        }
    }
}

pub fn show_diff(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    state: &mut DiffViewState,
    actions: &mut Vec<Action>,
) {
    // default the pickers to the last two runs only if nothing selected a pair yet
    if ctx.model.pending_diff.is_none()
        && let Some(runs) = ctx.model.runs.value()
        && runs.len() >= 2
    {
        let pair = (runs[runs.len() - 2].id, runs[runs.len() - 1].id);
        actions.push(Action::SetDiffRuns(pair.0, pair.1));
    }
    if state.last_pair != ctx.model.pending_diff {
        state.last_pair = ctx.model.pending_diff;
        state.selected = None;
    }

    render_toolbar(ui, ctx, state, actions);
    ui.add_space(4.0);

    if ctx.model.runs.value().is_some_and(|r| r.len() < 2) {
        kit::empty_state(
            ui,
            ctx.theme,
            "only one run so far",
            "collect again later; the diff compares the captured state of two runs",
        );
        return;
    }

    let model = ctx.model;
    let status = model.diff.status(model.diff_gen);
    let Some(entries) = model.diff.value() else {
        match status {
            LoadStatus::Error(err) => {
                ui.colored_label(ctx.theme.danger, format!("Failed to load diff: {err}"));
            }
            _ => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("comparing runs...");
                });
            }
        }
        return;
    };

    let filtered: Vec<(usize, &DiffEntryVm)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| match e.change {
            DiffChange::Added => state.show_added,
            DiffChange::Removed => state.show_removed,
            DiffChange::Changed => state.show_changed,
        })
        .collect();
    if entries.is_empty() {
        kit::empty_state(
            ui,
            ctx.theme,
            "nothing changed",
            "the two runs hold identical state",
        );
        return;
    }

    let selected_entry = state
        .selected
        .and_then(|idx| filtered.iter().find(|(i, _)| *i == idx).copied())
        .map(|(_, entry)| entry);
    let list_width = if selected_entry.is_some() {
        (ui.available_width() * 0.45).clamp(300.0, 560.0)
    } else {
        ui.available_width()
    };
    let mut builder = StripBuilder::new(ui).size(Size::exact(list_width));
    if selected_entry.is_some() {
        builder = builder.size(Size::remainder().at_least(240.0));
    }
    builder.horizontal(|mut strip| {
        strip.cell(|ui| render_entry_list(ui, ctx.theme, &filtered, state));
        if let Some(entry) = selected_entry {
            strip.cell(|ui| {
                egui::Frame::new()
                    .inner_margin(egui::Margin {
                        left: 10,
                        ..Default::default()
                    })
                    .show(ui, |ui| render_comparison(ui, ctx.theme, entry));
            });
        }
    });
}

fn render_toolbar(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    state: &mut DiffViewState,
    actions: &mut Vec<Action>,
) {
    let model = ctx.model;
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Diff").strong());
        ui.add_space(8.0);
        let runs = model.runs.value().map(Vec::as_slice).unwrap_or(&[]);
        let (mut run_a, mut run_b) = model
            .pending_diff
            .map(|(a, b)| (Some(a), Some(b)))
            .unwrap_or((None, None));
        ui.label("Run A:");
        let changed_a = run_picker(ui, "diff_run_a", "Run A", &mut run_a, run_b, runs, ctx.tz);
        ui.label("<->");
        ui.label("Run B:");
        let changed_b = run_picker(ui, "diff_run_b", "Run B", &mut run_b, run_a, runs, ctx.tz);
        if (changed_a || changed_b)
            && let (Some(a), Some(b)) = (run_a, run_b)
        {
            actions.push(Action::SetDiffRuns(a, b));
        }

        ui.add_space(8.0);
        if model.diff.value().is_some() {
            let entries = model.diff.value().map(Vec::as_slice).unwrap_or(&[]);
            let (added, removed, changed) = counts(entries);
            chip(
                ui,
                ctx.theme,
                "+ added",
                added,
                ctx.theme.ok,
                &mut state.show_added,
            );
            chip(
                ui,
                ctx.theme,
                "- removed",
                removed,
                ctx.theme.danger,
                &mut state.show_removed,
            );
            chip(
                ui,
                ctx.theme,
                "~ changed",
                changed,
                ctx.theme.warn,
                &mut state.show_changed,
            );
        }
    });
}

fn run_picker(
    ui: &mut egui::Ui,
    id: &str,
    a11y_label: &str,
    selected: &mut Option<u64>,
    other: Option<u64>,
    runs: &[crate::vm::RunMarkerVm],
    tz: fp_query::Tz,
) -> bool {
    let mut changed = false;
    let picker = egui::ComboBox::from_id_salt(id)
        .width(160.0)
        .selected_text(match selected {
            Some(id) => format!("run {id}"),
            None => "pick a run".to_string(),
        })
        .show_ui(ui, |ui| {
            for run in runs.iter().rev() {
                if Some(run.id) == other {
                    continue;
                }
                let label = format!(
                    "run {} · {}",
                    run.id,
                    fp_query::format_time(run.started_utc, "second", tz, "%d %b %H:%M")
                );
                if ui
                    .selectable_label(*selected == Some(run.id), label)
                    .clicked()
                {
                    *selected = Some(run.id);
                    changed = true;
                }
            }
        });
    picker
        .response
        .widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, a11y_label));
    changed
}

fn chip(
    ui: &mut egui::Ui,
    theme: &Theme,
    label: &str,
    count: usize,
    color: egui::Color32,
    on: &mut bool,
) {
    let text = format!("{label} ({count})");
    let (fill, ink) = if *on {
        (color, theme.bg0)
    } else {
        (theme.bg2, color)
    };
    if ui
        .add(egui::Button::new(egui::RichText::new(text).small().strong().color(ink)).fill(fill))
        .on_hover_text("click to show/hide this change kind")
        .clicked()
    {
        *on = !*on;
    }
}

fn counts(entries: &[DiffEntryVm]) -> (usize, usize, usize) {
    let mut counts = (0, 0, 0);
    for e in entries {
        match e.change {
            DiffChange::Added => counts.0 += 1,
            DiffChange::Removed => counts.1 += 1,
            DiffChange::Changed => counts.2 += 1,
        }
    }
    counts
}

fn render_entry_list(
    ui: &mut egui::Ui,
    theme: &Theme,
    filtered: &[(usize, &DiffEntryVm)],
    state: &mut DiffViewState,
) {
    let mut groups: BTreeMap<&str, Vec<(usize, &DiffEntryVm)>> = BTreeMap::new();
    for (idx, entry) in filtered {
        groups
            .entry(entry.kind.as_str())
            .or_default()
            .push((*idx, entry));
    }
    ui.vertical(|ui| {
        egui::ScrollArea::both()
            .id_salt("diff_entries_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (kind, items) in groups {
                    let header = summarize_group(kind, &items);
                    egui::CollapsingHeader::new(
                        egui::RichText::new(header).monospace().color(theme.text),
                    )
                    .default_open(true)
                    .show(ui, |ui| {
                        for (idx, entry) in items {
                            let marker = match entry.change {
                                DiffChange::Added => ("+", theme.ok),
                                DiffChange::Removed => ("-", theme.danger),
                                DiffChange::Changed => ("~", theme.warn),
                            };
                            let is_selected = state.selected == Some(idx);
                            let text = format!(
                                "{}  {}  {}",
                                marker.0,
                                entry.key.as_str(),
                                entry.detail.as_str()
                            );
                            let resp = ui.selectable_label(
                                is_selected,
                                egui::RichText::new(text).monospace().color(marker.1),
                            );
                            if resp.clicked() {
                                state.selected = Some(idx);
                            }
                            resp.on_hover_text("click to compare the two values");
                        }
                    });
                }
            });
    });
}

fn summarize_group(kind: &str, items: &[(usize, &DiffEntryVm)]) -> String {
    let mut added = 0;
    let mut removed = 0;
    let mut changed = 0;
    for (_, e) in items {
        match e.change {
            DiffChange::Added => added += 1,
            DiffChange::Removed => removed += 1,
            DiffChange::Changed => changed += 1,
        }
    }
    let mut summary = format!("{kind}  ");
    let mut parts = Vec::new();
    if added > 0 {
        parts.push(format!("+{added}"));
    }
    if removed > 0 {
        parts.push(format!("-{removed}"));
    }
    if changed > 0 {
        parts.push(format!("~{changed}"));
    }
    summary.push_str(&parts.join(" "));
    summary
}

fn render_comparison(ui: &mut egui::Ui, theme: &Theme, entry: &DiffEntryVm) {
    ui.vertical(|ui| {
        ui.label(
            egui::RichText::new(format!("{}  {}", entry.kind.as_str(), entry.key.as_str()))
                .monospace()
                .strong(),
        );
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .id_salt("diff_compare_scroll")
            .auto_shrink([false, false])
            .show(ui, |ui| match (&entry.run_a, &entry.run_b) {
                (Some(a), Some(b)) => {
                    ui.label(
                        egui::RichText::new("A on the left, B on the right; marked leaves differ")
                            .small()
                            .weak(),
                    );
                    show_json_diff(ui, "diff_values", a, b, theme);
                }
                (None, Some(b)) => {
                    ui.label(egui::RichText::new("added in run B:").small().weak());
                    crate::widgets::json_tree::show_json_tree(ui, "diff_added", b, theme);
                }
                (Some(a), None) => {
                    ui.label(egui::RichText::new("removed after run A:").small().weak());
                    crate::widgets::json_tree::show_json_tree(ui, "diff_removed", a, theme);
                }
                (None, None) => {
                    ui.label(egui::RichText::new("both values failed to parse").weak());
                }
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::SafeText;

    fn entry(change: DiffChange, kind: &str, key: &str) -> DiffEntryVm {
        DiffEntryVm {
            change,
            kind: SafeText::new(kind),
            key: SafeText::new(key),
            detail: SafeText::new("detail"),
            run_a: None,
            run_b: None,
        }
    }

    #[test]
    fn counts_and_group_summary_partition_by_change() {
        let entries = vec![
            entry(DiffChange::Added, "persist.cron", "a"),
            entry(DiffChange::Added, "persist.cron", "b"),
            entry(DiffChange::Removed, "persist.cron", "c"),
            entry(DiffChange::Changed, "ufw.status", "d"),
        ];
        assert_eq!(counts(&entries), (2, 1, 1));
        let items: Vec<(usize, &DiffEntryVm)> = entries.iter().enumerate().collect();
        let cron: Vec<(usize, &DiffEntryVm)> = items
            .iter()
            .copied()
            .filter(|(_, e)| e.kind.as_str() == "persist.cron")
            .collect();
        assert_eq!(
            summarize_group("persist.cron", &cron),
            "persist.cron  +2 -1"
        );
    }
}
