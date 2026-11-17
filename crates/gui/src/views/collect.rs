//! Collect view: the full CollectParams form with presets and the equivalent
//! CLI line, progress from `CollectEvent::Planned`, per-source rows, cancel,
//! and a summary with "view what's new" / "diff vs previous".

use std::path::PathBuf;
use std::sync::mpsc::Receiver;

use eframe::egui;
use fp_query::{CollectEvent, CollectParams};

use crate::model::{Action, View};
use crate::widgets::ViewCtx;
use crate::widgets::chip::thousands;
use crate::widgets::kit;

/// standard form families ↔ source-kind prefixes; opt-ins (`with_live`,
/// `with_scanners`) live in their own toggles so `--sources all` excludes them
const FAMILIES: &[(&str, &[&str])] = &[
    ("Browsers", &["chromium", "firefox"]),
    ("Downloads", &["dlmgr"]),
    ("Packages", &["pkg"]),
    ("Privilege", &["auth"]),
    ("Shell", &["shell"]),
    ("Desktop", &["desktop"]),
    ("Filesystem", &["fs"]),
    ("Persistence", &["persist"]),
];

const PRESETS: &[(&str, &str)] = &[
    ("last 24 h", "-1d"),
    ("last 7 d", "-7d"),
    ("last 30 d", "-30d"),
];

pub struct CollectViewState {
    pub preset: String,
    pub since: String,
    pub until: String,
    /// checked standard families by name; empty or all = `--sources all`
    pub families: Vec<String>,
    pub with_live: bool,
    pub with_scanners: bool,
    pub advanced_open: bool,
    pub assume_tz: String,
    pub homes: String,
    pub all_users: bool,
    pub root: String,
    pub exclude: String,
    /// `Some` while a collect runs
    pub run: Option<CollectRun>,
}

pub struct CollectRun {
    pub rx: Receiver<CollectEvent>,
    pub planned_total: Option<usize>,
    pub done: usize,
    pub rows: Vec<InstanceRow>,
    pub totals: fp_query::IngestTotals,
    pub finished: Option<crate::vm::SafeText>,
    pub run_id: Option<u64>,
}

#[derive(Debug)]
pub struct InstanceRow {
    pub label: crate::vm::SafeText,
    pub state: InstanceState,
}

#[derive(Debug)]
pub enum InstanceState {
    Reading,
    Done {
        events_new: usize,
        status: crate::vm::SafeText,
        warnings: Vec<crate::vm::SafeText>,
    },
    Error(crate::vm::SafeText),
}

impl Default for CollectViewState {
    fn default() -> Self {
        Self {
            preset: "-7d".to_string(),
            since: String::new(),
            until: String::new(),
            families: FAMILIES
                .iter()
                .map(|(name, _)| (*name).to_string())
                .collect(),
            with_live: false,
            with_scanners: false,
            advanced_open: false,
            assume_tz: String::new(),
            homes: String::new(),
            all_users: false,
            root: String::new(),
            exclude: String::new(),
            run: None,
        }
    }
}

impl CollectViewState {
    pub fn params(&self) -> CollectParams {
        let non_empty = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
        let since = if self.preset.is_empty() {
            non_empty(&self.since)
        } else {
            Some(self.preset.clone())
        };
        let all_standard = FAMILIES
            .iter()
            .all(|(name, _)| self.families.iter().any(|f| f == name));
        let mut sources: Vec<String> = Vec::new();
        if !all_standard {
            for (name, prefixes) in FAMILIES {
                if self.families.iter().any(|f| f == name) {
                    sources.extend(prefixes.iter().map(|p| (*p).to_string()));
                }
            }
        }
        CollectParams {
            since,
            until: if self.preset.is_empty() {
                non_empty(&self.until)
            } else {
                None
            },
            sources,
            exclude: self
                .exclude
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            assume_tz: (!self.assume_tz.trim().is_empty())
                .then(|| self.assume_tz.trim().to_string()),
            homes: self
                .homes
                .split(',')
                .map(|s| PathBuf::from(s.trim()))
                .filter(|p| !p.as_os_str().is_empty())
                .collect(),
            all_users: self.all_users,
            root: PathBuf::from(self.root.trim()),
            with_live: self.with_live,
            with_scanners: self.with_scanners,
        }
    }

    /// the CLI line this form would run, shown live under the form
    pub fn equivalent_command(&self) -> String {
        let p = self.params();
        let mut cmd = vec!["footprint".to_string(), "collect".to_string()];
        if let Some(since) = &p.since {
            cmd.push("--since".into());
            cmd.push(since.clone());
        }
        if let Some(until) = &p.until {
            cmd.push("--until".into());
            cmd.push(until.clone());
        }
        if !p.sources.is_empty() {
            cmd.push("--sources".into());
            cmd.push(p.sources.join(","));
        }
        if !p.exclude.is_empty() {
            cmd.push("--exclude".into());
            cmd.push(p.exclude.join(","));
        }
        if let Some(tz) = &p.assume_tz {
            cmd.push("--assume-tz".into());
            cmd.push(tz.clone());
        }
        for home in &p.homes {
            cmd.push("--home".into());
            cmd.push(home.display().to_string());
        }
        if p.all_users {
            cmd.push("--all-users".into());
        }
        if !p.root.as_os_str().is_empty() {
            cmd.push("--root".into());
            cmd.push(p.root.display().to_string());
        }
        if p.with_live {
            cmd.push("--with-live".into());
        }
        if p.with_scanners {
            cmd.push("--with-scanners".into());
        }
        cmd.join(" ")
    }
}

pub fn show_collect(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    state: &mut CollectViewState,
    actions: &mut Vec<Action>,
) {
    ui.label(egui::RichText::new("Collect").strong());
    ui.add_space(6.0);

    if state.run.is_some() {
        render_progress(ui, ctx, state, actions);
        return;
    }

    egui::ScrollArea::vertical()
        .id_salt("collect_form")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            render_window(ui, state);
            ui.add_space(8.0);
            render_sources(ui, state);
            ui.add_space(8.0);
            render_advanced(ui, state);
            ui.add_space(8.0);
            let command = state.equivalent_command();
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("equivalent command:").small().weak());
                ui.monospace(&command);
                if ui.small_button("copy").clicked() {
                    ui.ctx().copy_text(command.clone());
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if kit::primary_button(ui, ctx.theme, "Start")
                    .on_hover_text("start collecting into this case")
                    .clicked()
                {
                    actions.push(Action::StartCollect);
                }
            });
        });
}

fn render_window(ui: &mut egui::Ui, state: &mut CollectViewState) {
    ui.label(egui::RichText::new("WINDOW").small().strong());
    ui.horizontal_wrapped(|ui| {
        for (label, preset) in PRESETS {
            let selected = state.preset == *preset;
            if ui.selectable_label(selected, *label).clicked() {
                state.preset = preset.to_string();
            }
        }
        if ui
            .selectable_label(state.preset.is_empty(), "custom")
            .clicked()
        {
            state.preset.clear();
        }
    });
    if state.preset.is_empty() {
        ui.horizontal(|ui| {
            ui.label("since:");
            ui.add(egui::TextEdit::singleline(&mut state.since).desired_width(160.0));
            ui.label("until:");
            ui.add(egui::TextEdit::singleline(&mut state.until).desired_width(160.0));
            ui.label(
                egui::RichText::new("e.g. -1d, 2026-10-10, @1760647493")
                    .small()
                    .weak(),
            );
        });
    }
}

fn render_sources(ui: &mut egui::Ui, state: &mut CollectViewState) {
    ui.label(egui::RichText::new("SOURCES").small().strong());
    ui.horizontal_wrapped(|ui| {
        for (name, _) in FAMILIES {
            let mut checked = state.families.iter().any(|f| f == name);
            if ui.checkbox(&mut checked, *name).changed() {
                if checked {
                    state.families.push((*name).to_string());
                } else {
                    state.families.retain(|f| f != name);
                }
            }
        }
        ui.checkbox(&mut state.with_live, "Live state (--with-live)");
        let scanner_label = if state.with_scanners {
            "Scanners (--with-scanners)  (runs clamscan/rkhunter)"
        } else {
            "Scanners (--with-scanners)"
        };
        ui.checkbox(&mut state.with_scanners, scanner_label);
    });
    if state.families.is_empty() {
        ui.label(
            egui::RichText::new("nothing checked = --sources all (opt-ins still excluded)")
                .small()
                .weak(),
        );
    }
}

fn render_advanced(ui: &mut egui::Ui, state: &mut CollectViewState) {
    egui::CollapsingHeader::new(egui::RichText::new("ADVANCED").small().strong())
        .default_open(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("assume tz:");
                ui.add(egui::TextEdit::singleline(&mut state.assume_tz).desired_width(120.0));
                ui.label("extra homes (comma separated):");
                ui.add(egui::TextEdit::singleline(&mut state.homes).desired_width(240.0));
            });
            ui.horizontal(|ui| {
                let resp = ui.add_enabled(
                    false,
                    egui::Checkbox::new(&mut state.all_users, "all users (root only)"),
                );
                resp.on_hover_text(
                    "the GUI refuses root; run `sudo footprint collect` in a terminal for all users",
                );
                ui.label("root dir:");
                ui.add(egui::TextEdit::singleline(&mut state.root).desired_width(160.0));
                ui.label("exclude (comma separated kinds):");
                ui.add(egui::TextEdit::singleline(&mut state.exclude).desired_width(200.0));
            });
        });
}

fn render_progress(
    ui: &mut egui::Ui,
    ctx: &ViewCtx<'_>,
    state: &mut CollectViewState,
    actions: &mut Vec<Action>,
) {
    let theme = ctx.theme;
    let Some(run) = state.run.as_mut() else {
        return;
    };
    if run.finished.is_none() {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(80));
    }
    if let Some(total) = run.planned_total {
        let progress = run.done as f32 / total.max(1) as f32;
        ui.add(egui::ProgressBar::new(progress).text(format!("{} / {} sources", run.done, total)));
    } else {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("discovering sources…");
        });
    }
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!(
                "+{} new · {} known · {} state · {} artifacts",
                thousands(run.totals.events_new as u64),
                thousands(run.totals.events_known as u64),
                run.totals.states,
                run.totals.artifacts,
            ))
            .small(),
        );
        if run.finished.is_none()
            && ui
                .button("Cancel")
                .on_hover_text("stop after the current source")
                .clicked()
        {
            actions.push(Action::CancelCollect);
        }
    });
    ui.add_space(6.0);

    egui::ScrollArea::vertical()
        .id_salt("collect_rows")
        .auto_shrink([false, false])
        .max_height(320.0)
        .show(ui, |ui| {
            for row in &run.rows {
                ui.horizontal(|ui| {
                    // ✓/✖ have no bundled-font glyph; the marks are painter-drawn
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                    let mark = match &row.state {
                        InstanceState::Reading => None,
                        InstanceState::Done { .. } => {
                            Some((crate::widgets::badge::StatusGlyph::Ok, theme.ok))
                        }
                        InstanceState::Error(_) => {
                            Some((crate::widgets::badge::StatusGlyph::Mismatch, theme.danger))
                        }
                    };
                    if let Some((glyph, color)) = mark {
                        crate::widgets::badge::status_mark(
                            ui.painter(),
                            rect.center(),
                            5.0,
                            glyph,
                            color,
                        );
                    } else {
                        ui.spinner();
                    }
                    ui.label(egui::RichText::new(row.label.as_str()).monospace());
                    match &row.state {
                        InstanceState::Reading => {
                            ui.label(egui::RichText::new("reading…").small().weak());
                        }
                        InstanceState::Done {
                            events_new,
                            status,
                            warnings,
                        } => {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{events_new} new · {}",
                                    status.as_str()
                                ))
                                .small(),
                            );
                            for warning in warnings {
                                ui.label(
                                    egui::RichText::new(warning.as_str())
                                        .small()
                                        .color(theme.warn),
                                );
                            }
                        }
                        InstanceState::Error(err) => {
                            ui.label(
                                egui::RichText::new(err.as_str())
                                    .small()
                                    .color(theme.danger),
                            );
                        }
                    }
                });
            }
        });

    if let Some(summary) = &run.finished {
        ui.separator();
        ui.label(summary.as_str());
        if let Some(run_id) = run.run_id {
            let prev_id = ctx
                .model
                .runs
                .value()
                .and_then(|runs| runs.windows(2).find(|w| w[1].id == run_id).map(|w| w[0].id))
                .or_else(|| (run_id > 1).then_some(run_id - 1));
            ui.horizontal(|ui| {
                if ui.button("View what's new").clicked() {
                    actions.push(Action::Scope(crate::model::ScopeAction::SetRun(Some(
                        run_id,
                    ))));
                    actions.push(Action::Go(View::Timeline));
                }
                if let Some(prev) = prev_id
                    && ui.button("Diff vs previous").clicked()
                {
                    actions.push(Action::SetDiffRuns(prev, run_id));
                    actions.push(Action::Go(View::Diff));
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equivalent_command_handles_defaults_subsets_and_opt_ins() {
        let mut state = CollectViewState::default();
        assert_eq!(state.equivalent_command(), "footprint collect --since -7d");

        state.with_live = true;
        state.with_scanners = true;
        let cmd = state.equivalent_command();
        assert!(cmd.contains("--with-live"));
        assert!(cmd.contains("--with-scanners"));
        assert!(!cmd.contains("--sources"));

        state.families.retain(|f| f == "Browsers");
        let subset = state.equivalent_command();
        assert!(subset.contains("--sources chromium,firefox"));
        assert!(subset.contains("--with-live"));
    }
}
