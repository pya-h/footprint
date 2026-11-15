//! The window shell around the views: top bar, scope bar, rail, banners and the
//! status bar. The views themselves land in later milestones; the shell already
//! owns the case header, the counts and every full-window state (empty, loading,
//! error, busy, changed).

use std::collections::HashSet;
use std::path::PathBuf;

use eframe::egui;

use crate::model::{Action, Counts, LoadStatus, Model, ScopeAction, Selection, View};
use crate::settings::Settings;
use crate::theme::Theme;
use crate::theme::ThemeName;
use crate::worker::{QueryError, ReqKind, Worker};

pub struct Shell {
    case: PathBuf,
    tz: fp_query::Tz,
    theme: ThemeName,
    settings: Settings,
    settings_warning: Option<String>,
    model: Model,
    worker: Option<Worker>,
    startup_error: Option<String>,
    search: String,
    case_missing: bool,
    submitted: Option<crate::worker::Generation>,
    outstanding: HashSet<u64>,
}

fn apply_theme(name: ThemeName, ui_scale: f32, ctx: &egui::Context) {
    match name {
        ThemeName::Dark => Theme::dark(),
        ThemeName::Light => Theme::light(),
        ThemeName::HighContrast => Theme::high_contrast(),
    }
    .apply(ctx);
    ctx.set_zoom_factor(ui_scale.clamp(0.8, 1.6));
}

fn view_from_label(label: &str) -> Option<View> {
    View::ALL
        .iter()
        .find(|v| v.label().eq_ignore_ascii_case(label))
        .copied()
}

fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn bytes(n: u64) -> String {
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

impl Shell {
    pub fn new(
        case: PathBuf,
        tz: fp_query::Tz,
        settings: Settings,
        settings_warning: Option<String>,
        ctx: &egui::Context,
    ) -> Self {
        apply_theme(settings.theme, settings.ui_scale, ctx);
        let worker = Worker::spawn(&case, 3, {
            let ctx = ctx.clone();
            move || ctx.request_repaint()
        });
        // an unreadable case is not a crash: the window opens with an error card
        let startup_error = worker.as_ref().err().map(|e| e.to_string());
        let case_missing = !case.exists();

        let mut model = Model::new();
        if !case_missing && let Some(view) = settings.last_view.as_deref().and_then(view_from_label)
        {
            model.nav.navigate(view, Selection::None);
        }
        Self {
            case,
            tz,
            theme: settings.theme,
            settings,
            settings_warning,
            model,
            worker: worker.ok(),
            startup_error,
            search: String::new(),
            case_missing,
            submitted: None,
            outstanding: HashSet::new(),
        }
    }

    fn apply(&mut self, action: Action) {
        self.model.apply(action);
    }

    fn drain_worker(&mut self) {
        let responses = self.worker.as_ref().map(|w| w.poll()).unwrap_or_default();
        for resp in responses {
            self.outstanding.remove(&resp.id);
            match resp.kind {
                ReqKind::Count => {
                    let result = match resp.result {
                        Ok(payload) => payload
                            .downcast::<(Counts, u64)>()
                            .map(|boxed| *boxed)
                            .map_err(|_| "internal: counts payload mismatch".to_string()),
                        Err(QueryError::Message(m)) => Err(m),
                        Err(QueryError::Interrupted) => continue,
                    };
                    let generation = resp.generation;
                    match result {
                        Ok((counts, untimed)) => {
                            self.apply(Action::Counts {
                                generation,
                                result: Ok(counts),
                            });
                            self.apply(Action::Untimed {
                                generation,
                                result: Ok(untimed),
                            });
                        }
                        Err(message) => {
                            self.apply(Action::Counts {
                                generation,
                                result: Err(message.clone()),
                            });
                            self.apply(Action::Untimed {
                                generation,
                                result: Err(message),
                            });
                        }
                    }
                }
                ReqKind::Bounds => {
                    let result = match resp.result {
                        Ok(payload) => payload
                            .downcast::<Option<(i64, i64)>>()
                            .map(|boxed| *boxed)
                            .map_err(|_| "internal: bounds payload mismatch".to_string()),
                        Err(QueryError::Message(m)) => Err(m),
                        Err(QueryError::Interrupted) => continue,
                    };
                    self.apply(Action::Bounds {
                        generation: resp.generation,
                        result,
                    });
                }
                ReqKind::Watch => {
                    let Some(version) = resp
                        .result
                        .ok()
                        .and_then(|p| p.downcast::<i64>().map(|b| *b).ok())
                    else {
                        continue;
                    };
                    if self.case_missing {
                        // the case appeared after startup: leave the empty state and load it
                        self.case_missing = false;
                        self.submitted = None;
                    }
                    self.apply(Action::Observed { version });
                }
                _ => {}
            }
        }
    }

    fn ensure_requests(&mut self) {
        if self.case_missing || self.startup_error.is_some() || self.worker.is_none() {
            return;
        }
        if self.submitted == Some(self.model.generation) {
            return;
        }
        self.submitted = Some(self.model.generation);
        let generation = self.model.generation;
        let filter = self.model.scope.filter();
        let Some(worker) = &self.worker else {
            return;
        };

        let id = worker.submit(ReqKind::Count, None, generation, move |store| {
            let in_case = store.event_count()?;
            let in_scope = store.count_events(&fp_store::EventFilter::from(&filter))?;
            let untimed_filter = fp_query::TimelineFilter {
                since: None,
                until: None,
                timing: fp_store::EventTiming::Untimed,
                ..filter.clone()
            };
            let untimed = store.count_events(&fp_store::EventFilter::from(&untimed_filter))?;
            Ok(Box::new((Counts { in_scope, in_case }, untimed)))
        });
        self.outstanding.insert(id);

        let id = worker.submit(ReqKind::Bounds, None, generation, |store| {
            Ok(Box::new(store.time_bounds()?))
        });
        self.outstanding.insert(id);
    }

    fn shortcuts(&mut self, ui: &egui::Ui) {
        if ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        let pressed =
            |ui: &egui::Ui, key: egui::Key| ui.input(|i| i.key_pressed(key) && i.modifiers.ctrl);
        if pressed(ui, egui::Key::Z) {
            self.apply(Action::UndoScope);
        }
        if pressed(ui, egui::Key::Y) {
            self.apply(Action::RedoScope);
        }
        if ui.input(|i| i.key_pressed(egui::Key::C) && i.modifiers.ctrl && i.modifiers.shift) {
            ui.ctx().copy_text(self.model.scope.to_cli());
        }
        if ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Minus)) {
            self.zoom_by(ui, -0.1);
        }
        if ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Equals)) {
            self.zoom_by(ui, 0.1);
        }
        if ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Num0)) {
            self.settings.ui_scale = 1.0;
            apply_theme(self.theme, self.settings.ui_scale, ui.ctx());
        }
        for (i, key) in [
            egui::Key::Num1,
            egui::Key::Num2,
            egui::Key::Num3,
            egui::Key::Num4,
            egui::Key::Num5,
            egui::Key::Num6,
            egui::Key::Num7,
            egui::Key::Num8,
            egui::Key::Num9,
            egui::Key::Num0,
        ]
        .iter()
        .enumerate()
        {
            if ui.input(|i| i.key_pressed(*key))
                && let Some(view) = View::from_shortcut(i)
            {
                self.apply(Action::Go(view));
            }
        }
    }

    fn zoom_by(&mut self, ui: &egui::Ui, delta: f32) {
        self.settings.ui_scale = (self.settings.ui_scale + delta).clamp(0.8, 1.6);
        apply_theme(self.theme, self.settings.ui_scale, ui.ctx());
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("footprint").strong());
            let name = self
                .case
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.case.display().to_string());
            if ui
                .button(name)
                .on_hover_text(format!("{} — click to copy the path", self.case.display()))
                .clicked()
            {
                ui.ctx().copy_text(self.case.display().to_string());
            }
            ui.add_enabled_ui(false, |ui| {
                let _ = ui.button("run: all");
            })
            .response
            .on_hover_text("run filtering arrives with the sources view");
            let response = ui.add_sized(
                [280.0, 20.0],
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("search subjects, hosts, commands…"),
            );
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                let text = (!self.search.is_empty()).then(|| self.search.clone());
                self.apply(Action::Scope(ScopeAction::SetText(text)));
                self.search.clear();
                response.surrender_focus();
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("Collect")
                    .on_hover_text("collect arrives with the collect view")
                    .clicked()
                {
                    self.apply(Action::Go(View::Collect));
                }
                let glyph = match self.theme {
                    ThemeName::Dark => "☾",
                    ThemeName::Light => "☀",
                    ThemeName::HighContrast => "◐",
                };
                if ui.button(glyph).on_hover_text("switch theme").clicked() {
                    self.theme = match self.theme {
                        ThemeName::Dark => ThemeName::Light,
                        ThemeName::Light => ThemeName::HighContrast,
                        ThemeName::HighContrast => ThemeName::Dark,
                    };
                    self.settings.theme = self.theme;
                    apply_theme(self.theme, self.settings.ui_scale, ui.ctx());
                }
                ui.label(self.tz_label());
            });
        });
    }

    fn tz_label(&self) -> String {
        match self.tz {
            fp_query::Tz::Local => "local".to_string(),
            fp_query::Tz::Utc => "UTC".to_string(),
            fp_query::Tz::Fixed(offset) => offset.to_string(),
        }
    }

    fn scope_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            let chips = self.model.scope.chips();
            if chips.is_empty() {
                ui.label(egui::RichText::new("everything in the case").weak());
            }
            for chip in &chips {
                egui::Frame::group(ui.style())
                    .corner_radius(egui::CornerRadius::same(6))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(&chip.label);
                            if ui.small_button("×").on_hover_text("remove").clicked() {
                                self.apply(Action::Scope(ScopeAction::Remove(chip.remove.clone())));
                            }
                        });
                    });
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let scope_empty = self.model.scope.is_empty();
                if ui
                    .add_enabled(!scope_empty, egui::Button::new("clear"))
                    .on_hover_text("clear the scope (Ctrl+Shift+Backspace)")
                    .clicked()
                {
                    self.apply(Action::Scope(ScopeAction::Clear));
                }
                if ui
                    .button("copy CLI")
                    .on_hover_text("copy the scope as a footprint command (Ctrl+Shift+C)")
                    .clicked()
                {
                    ui.ctx().copy_text(self.model.scope.to_cli());
                }
                if ui
                    .add_enabled(self.model.scope.can_redo(), egui::Button::new("redo"))
                    .on_hover_text("Ctrl+Y")
                    .clicked()
                {
                    self.apply(Action::RedoScope);
                }
                if ui
                    .add_enabled(self.model.scope.can_undo(), egui::Button::new("undo"))
                    .on_hover_text("Ctrl+Z")
                    .clicked()
                {
                    self.apply(Action::UndoScope);
                }
                match self.model.counts.value() {
                    Some(counts) => {
                        ui.label(format!(
                            "{} / {}",
                            thousands(counts.in_scope),
                            thousands(counts.in_case)
                        ));
                    }
                    None => {
                        ui.label(egui::RichText::new("— / —").weak());
                    }
                }
            });
        });
    }

    fn banners(&mut self, ui: &mut egui::Ui) {
        let settings_warning = self.settings_warning.clone();
        ui.horizontal_wrapped(|ui| {
            if let Some(warning) = settings_warning {
                ui.colored_label(ui.visuals().warn_fg_color, warning);
                if ui.small_button("×").clicked() {
                    self.settings_warning = None;
                }
            } else if self.model.watch.case_writing() {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "case is being written; views refresh when the run finishes",
                );
            } else if self.model.watch.case_changed() {
                ui.colored_label(ui.visuals().warn_fg_color, "case changed (new run)");
                if ui.button("Refresh").clicked() {
                    self.apply(Action::RefreshCase);
                    self.submitted = None;
                }
            } else if let Some(error) = self.fresh_error() {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        });
    }

    /// a query error for the generation currently on screen, if any
    fn fresh_error(&self) -> Option<String> {
        let generation = self.model.generation;
        let counts = &self.model.counts;
        let bounds = &self.model.bounds;
        for (slot_generation, status) in [
            (counts.generation(), counts.status(generation)),
            (bounds.generation(), bounds.status(generation)),
        ] {
            if slot_generation == generation
                && let LoadStatus::Error(message) = status
            {
                return Some(message);
            }
        }
        None
    }

    fn has_banners(&self) -> bool {
        self.settings_warning.is_some()
            || self.model.watch.case_writing()
            || self.model.watch.case_changed()
            || self.fresh_error().is_some()
    }

    fn rail(&mut self, ui: &mut egui::Ui) {
        for (i, view) in View::ALL.iter().enumerate() {
            let selected = self.model.view() == *view;
            if ui
                .selectable_label(selected, format!("{}  {}", i + 1, view.label()))
                .clicked()
            {
                self.apply(Action::Go(*view));
            }
        }
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            match self.model.counts.status(self.model.generation) {
                LoadStatus::Loading => {
                    ui.spinner();
                    ui.label("counting events…");
                }
                LoadStatus::Stale => {
                    ui.spinner();
                    if let Some(counts) = self.model.counts.value() {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} events in scope · {} in case",
                                thousands(counts.in_scope),
                                thousands(counts.in_case)
                            ))
                            .weak(),
                        );
                    }
                }
                LoadStatus::Ready => {
                    if let Some(counts) = self.model.counts.value() {
                        ui.label(format!(
                            "{} events in scope · {} in case",
                            thousands(counts.in_scope),
                            thousands(counts.in_case)
                        ));
                    }
                }
                LoadStatus::Error(_) => {
                    ui.label(egui::RichText::new("count failed").weak());
                }
            }
            if let Some(hidden) = self.model.untimed_hidden() {
                ui.label(format!("· {hidden} untimed hidden by the time window"));
                if ui
                    .link("show")
                    .on_hover_text("drop the window and view the untimed events")
                    .clicked()
                {
                    self.apply(Action::Scope(ScopeAction::ShowUntimed));
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.outstanding.is_empty() {
                    ui.label(egui::RichText::new("worker idle").weak());
                } else {
                    ui.spinner();
                    ui.label("worker busy");
                }
            });
        });
    }

    fn main(&mut self, ui: &mut egui::Ui) {
        if self.case_missing {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("no case yet");
                    ui.label(format!("{}", self.case.display()));
                    ui.label(
                        "run `footprint collect` to create it — the GUI picks it up automatically",
                    );
                });
            });
            return;
        }
        if self.model.counts.value().is_some_and(|c| c.in_case == 0) {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("this case has no events yet");
                    ui.label("run `footprint collect`, then watch the totals appear");
                });
            });
            return;
        }
        match self.model.view() {
            View::Dashboard => self.dashboard(ui),
            view => {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        ui.heading(view.label());
                        ui.label("this view is built in a later milestone");
                    });
                });
            }
        }
    }

    fn dashboard(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(24.0);
            ui.heading(self.model.view().label());
            ui.add_space(8.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_min_width(420.0);
                let row = |ui: &mut egui::Ui, key: &str, value: String| {
                    ui.horizontal(|ui| {
                        ui.add(egui::Label::new(key).truncate());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.monospace(value);
                        });
                    });
                };
                row(ui, "case", self.case.display().to_string());
                if let Ok(meta) = std::fs::metadata(&self.case) {
                    row(ui, "size", bytes(meta.len()));
                }
                match self.model.bounds.status(self.model.generation) {
                    LoadStatus::Ready => {
                        if let Some(Some((first, last))) = self.model.bounds.value() {
                            let fmt = |ts: i64| {
                                fp_query::format_time(ts, "exact", self.tz, "%Y-%m-%d %H:%M")
                            };
                            row(ui, "timespan", format!("{} → {}", fmt(*first), fmt(*last)));
                        } else {
                            row(ui, "timespan", "no timed events".to_string());
                        }
                    }
                    _ => {
                        row(ui, "timespan", "…".to_string());
                    }
                }
                if let Some(counts) = self.model.counts.value() {
                    row(ui, "events in case", thousands(counts.in_case));
                    if counts.in_scope != counts.in_case {
                        row(ui, "events in scope", thousands(counts.in_scope));
                    }
                }
            });
        });
    }
}

impl eframe::App for Shell {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.drain_worker();
        if let Some(error) = self.startup_error.clone() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.colored_label(ui.visuals().error_fg_color, "cannot open the case");
                    ui.monospace(self.case.display().to_string());
                    ui.label(error);
                    ui.label(
                        "run `footprint collect` to start a new case, or open a different one",
                    );
                });
            });
            return;
        }
        self.ensure_requests();
        self.shortcuts(ui);

        egui::Panel::top("top_bar").show(ui, |ui| self.top_bar(ui));
        egui::Panel::top("scope_bar").show(ui, |ui| self.scope_bar(ui));
        if self.has_banners() {
            egui::Panel::top("banners").show(ui, |ui| self.banners(ui));
        }
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("rail")
            .resizable(false)
            .default_size(96.0)
            .show(ui, |ui| self.rail(ui));
        egui::CentralPanel::default().show(ui, |ui| self.main(ui));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.settings.theme = self.theme;
        self.settings.last_view = Some(self.model.view().label().to_lowercase());
        // a failure to write settings is non-fatal
        let _ = self.settings.save();
    }
}
