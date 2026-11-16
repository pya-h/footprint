//! Scope bar: removable, click-to-edit filter chips plus undo, redo, CLI copy, and counts.

use eframe::egui;
use fp_query::Tz;

use crate::model::{Action, ChipRemove, SavedViews, Scope, ScopeAction};

#[derive(Default)]
pub struct SavedViewsUiState {
    pub save_open: bool,
    pub menu_open: bool,
    pub draft_name: String,
    pub editing_id: Option<u64>,
    pub rename_text: String,
}

impl SavedViewsUiState {
    pub fn close(&mut self) {
        self.save_open = false;
        self.menu_open = false;
        self.editing_id = None;
    }

    pub fn is_open(&self) -> bool {
        self.save_open || self.menu_open
    }
}

pub struct ScopeBarCtx<'a> {
    pub scope: &'a Scope,
    pub saved_views: &'a SavedViews,
    pub counts: Option<(u64, u64)>,
    pub tz: Tz,
}

#[derive(Default)]
pub struct ChipEditorState {
    editing: Option<usize>,
    since_text: String,
    until_text: String,
    value_text: String,
    exclude: bool,
    confidence: [bool; 3],
    untimed_only: bool,
    error: Option<String>,
}

impl ChipEditorState {
    pub fn close(&mut self) {
        self.editing = None;
        self.error = None;
    }

    pub fn is_open(&self) -> bool {
        self.editing.is_some()
    }
}

pub fn show_scope_bar(
    ui: &mut egui::Ui,
    ctx: ScopeBarCtx<'_>,
    editor: &mut ChipEditorState,
    saved_ui: &mut SavedViewsUiState,
    actions: &mut Vec<Action>,
) {
    let scope = ctx.scope;
    let chips = scope.chips();
    ui.horizontal_wrapped(|ui| {
        if chips.is_empty() {
            ui.label(egui::RichText::new("everything in the case").weak());
        }
        for (idx, chip) in chips.iter().enumerate() {
            let is_editing = editor.editing == Some(idx);
            let frame = egui::Frame::group(ui.style()).corner_radius(egui::CornerRadius::same(6));
            let resp = frame
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let body = ui
                            .add(
                                egui::Button::new(&chip.label)
                                    .frame(false)
                                    .selected(is_editing),
                            )
                            .on_hover_text("click to edit chip");
                        if body.clicked() {
                            if is_editing {
                                editor.close();
                            } else {
                                saved_ui.close();
                                seed_editor(editor, idx, &chip.remove, scope, ctx.tz);
                            }
                        }
                        if ui.small_button("×").on_hover_text("remove").clicked() {
                            if is_editing {
                                editor.close();
                            }
                            actions.push(Action::Scope(ScopeAction::Remove(chip.remove.clone())));
                        }
                    });
                })
                .response;

            if is_editing {
                let area_id = ui.scope_id().with(("chip_edit", idx));
                let pos = resp.rect.left_bottom() + egui::vec2(0.0, 4.0);
                egui::Area::new(area_id)
                    .order(egui::Order::Foreground)
                    .fixed_pos(pos)
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style()).show(ui, |ui| {
                            render_editor(ui, &chip.remove, scope, ctx.tz, editor, actions);
                        });
                    });
            }
        }

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let scope_empty = scope.is_empty();
            if ui
                .add_enabled(!scope_empty, egui::Button::new("clear"))
                .on_hover_text("clear the scope (Ctrl+Shift+Backspace)")
                .clicked()
            {
                editor.close();
                actions.push(Action::Scope(ScopeAction::Clear));
            }
            if ui
                .button("copy CLI")
                .on_hover_text("copy the scope as a footprint command (Ctrl+Shift+C)")
                .clicked()
            {
                ui.ctx().copy_text(scope.to_cli());
            }
            render_saved_views_controls(ui, ctx.saved_views, editor, saved_ui, actions);
            if ui
                .add_enabled(scope.can_redo(), egui::Button::new("redo"))
                .on_hover_text("Ctrl+Y")
                .clicked()
            {
                editor.close();
                actions.push(Action::RedoScope);
            }
            if ui
                .add_enabled(scope.can_undo(), egui::Button::new("undo"))
                .on_hover_text("Ctrl+Z")
                .clicked()
            {
                editor.close();
                actions.push(Action::UndoScope);
            }
            match ctx.counts {
                Some((in_scope, in_case)) => {
                    ui.label(format!("{} / {}", thousands(in_scope), thousands(in_case)));
                }
                None => {
                    ui.label(egui::RichText::new("— / —").weak());
                }
            }
        });
    });
}

fn render_saved_views_controls(
    ui: &mut egui::Ui,
    saved_views: &SavedViews,
    editor: &mut ChipEditorState,
    saved_ui: &mut SavedViewsUiState,
    actions: &mut Vec<Action>,
) {
    let saved_label = if saved_views.is_empty() {
        "views".to_string()
    } else {
        format!("views ({})", saved_views.len())
    };
    let menu_btn = ui
        .selectable_label(saved_ui.menu_open, saved_label)
        .on_hover_text("apply or manage session saved views");
    if menu_btn.clicked() {
        saved_ui.menu_open = !saved_ui.menu_open;
        saved_ui.save_open = false;
        saved_ui.editing_id = None;
        editor.close();
    }
    if saved_ui.menu_open {
        let pos = menu_btn.rect.left_bottom() + egui::vec2(-160.0, 4.0);
        egui::Area::new(ui.scope_id().with("saved_views_menu"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(250.0);
                    ui.label(egui::RichText::new("Saved views (session only)").strong());
                    if saved_views.is_empty() {
                        ui.label(egui::RichText::new("none saved yet").small().weak());
                    }
                    for sv in saved_views.list() {
                        if saved_ui.editing_id == Some(sv.id) {
                            ui.horizontal(|ui| {
                                let ed = ui.add(
                                    egui::TextEdit::singleline(&mut saved_ui.rename_text)
                                        .desired_width(140.0),
                                );
                                let submit = ui.button("OK").clicked()
                                    || (ed.lost_focus()
                                        && ui.input(|i| i.key_pressed(egui::Key::Enter)));
                                if submit {
                                    actions.push(Action::RenameSavedView {
                                        id: sv.id,
                                        name: saved_ui.rename_text.clone(),
                                    });
                                    saved_ui.editing_id = None;
                                }
                                if ui.small_button("cancel").clicked() {
                                    saved_ui.editing_id = None;
                                }
                            });
                            continue;
                        }
                        ui.horizontal(|ui| {
                            if ui
                                .button(format!("{} ({})", sv.name, sv.view.label()))
                                .clicked()
                            {
                                actions.push(Action::ApplySavedView(sv.id));
                                saved_ui.menu_open = false;
                            }
                            if ui.small_button("rename").clicked() {
                                saved_ui.editing_id = Some(sv.id);
                                saved_ui.rename_text = sv.name.clone();
                            }
                            if ui.small_button("x").on_hover_text("remove view").clicked() {
                                actions.push(Action::RemoveSavedView(sv.id));
                            }
                        });
                    }
                });
            });
    }

    let save_btn = ui
        .selectable_label(saved_ui.save_open, "save view")
        .on_hover_text("name and save the current view + scope for this session");
    if save_btn.clicked() {
        saved_ui.save_open = !saved_ui.save_open;
        saved_ui.menu_open = false;
        editor.close();
        if saved_ui.save_open && saved_ui.draft_name.is_empty() {
            saved_ui.draft_name = format!("View {}", saved_views.len() + 1);
        }
    }
    if saved_ui.save_open {
        let pos = save_btn.rect.left_bottom() + egui::vec2(-140.0, 4.0);
        egui::Area::new(ui.scope_id().with("save_view_popup"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.label(egui::RichText::new("Save current view + scope").strong());
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut saved_ui.draft_name)
                            .desired_width(180.0)
                            .hint_text("view name…"),
                    );
                    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() || enter {
                            let name = saved_ui.draft_name.trim().to_string();
                            if !name.is_empty() {
                                actions.push(Action::SaveView(name));
                                saved_ui.draft_name.clear();
                                saved_ui.save_open = false;
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            saved_ui.save_open = false;
                        }
                    });
                });
            });
    }
}

fn seed_editor(
    editor: &mut ChipEditorState,
    idx: usize,
    remove: &ChipRemove,
    scope: &Scope,
    tz: Tz,
) {
    let f = scope.filter();
    editor.editing = Some(idx);
    editor.error = None;
    editor.since_text.clear();
    editor.until_text.clear();
    editor.value_text.clear();
    editor.exclude = false;
    editor.confidence = [false; 3];
    editor.untimed_only = false;

    match remove {
        ChipRemove::SinceUntil => {
            if let Some(s) = f.since {
                editor.since_text = fp_query::format_time(s, "exact", tz, "%Y-%m-%d %H:%M:%S");
            }
            if let Some(u) = f.until {
                editor.until_text = fp_query::format_time(u, "exact", tz, "%Y-%m-%d %H:%M:%S");
            }
        }
        ChipRemove::Kind(i) => {
            editor.value_text = f.kinds.get(*i).cloned().unwrap_or_default();
        }
        ChipRemove::KindNot(i) => {
            editor.value_text = f.kinds_not.get(*i).cloned().unwrap_or_default();
            editor.exclude = true;
        }
        ChipRemove::Host(i) => {
            editor.value_text = f.hosts.get(*i).cloned().unwrap_or_default();
        }
        ChipRemove::HostNot(i) => {
            editor.value_text = f.hosts_not.get(*i).cloned().unwrap_or_default();
            editor.exclude = true;
        }
        ChipRemove::Source(i) => {
            editor.value_text = f.sources.get(*i).cloned().unwrap_or_default();
        }
        ChipRemove::SourceNot(i) => {
            editor.value_text = f.sources_not.get(*i).cloned().unwrap_or_default();
            editor.exclude = true;
        }
        ChipRemove::Confidence(_) => {
            editor.confidence = [
                f.confidence.contains(&fp_core::Confidence::Direct),
                f.confidence.contains(&fp_core::Confidence::Derived),
                f.confidence.contains(&fp_core::Confidence::Heuristic),
            ];
        }
        ChipRemove::Run => {
            editor.value_text = f.run_id.map(|id| id.to_string()).unwrap_or_default();
        }
        ChipRemove::Timing => {
            editor.untimed_only = matches!(f.timing, fp_store::EventTiming::Untimed);
        }
        ChipRemove::Text => {
            editor.value_text = f.match_text.unwrap_or_default();
        }
        ChipRemove::IocOnly => {}
    }
}

fn render_editor(
    ui: &mut egui::Ui,
    remove: &ChipRemove,
    scope: &Scope,
    tz: Tz,
    editor: &mut ChipEditorState,
    actions: &mut Vec<Action>,
) {
    ui.set_min_width(240.0);
    let mut submit = false;
    match remove {
        ChipRemove::SinceUntil => {
            ui.label(egui::RichText::new("Time window (e.g. 2026-10-16 20:44 or -2h)").strong());
            ui.horizontal(|ui| {
                ui.label("since");
                let r = ui.text_edit_singleline(&mut editor.since_text);
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("until");
                let r = ui.text_edit_singleline(&mut editor.until_text);
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
            });
            if ui.button("apply").clicked() || submit {
                match (
                    parse_optional_time(&editor.since_text, tz),
                    parse_optional_time(&editor.until_text, tz),
                ) {
                    (Ok(since), Ok(until)) => {
                        actions.push(Action::Scope(ScopeAction::SetWindow { since, until }));
                        editor.close();
                    }
                    (Err(e), _) | (_, Err(e)) => editor.error = Some(e),
                }
            }
        }
        ChipRemove::Text => {
            ui.label(egui::RichText::new("Search text").strong());
            let r = ui.text_edit_singleline(&mut editor.value_text);
            if (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                || ui.button("apply").clicked()
            {
                let trimmed = editor.value_text.trim().to_string();
                actions.push(Action::Scope(ScopeAction::SetText(
                    (!trimmed.is_empty()).then_some(trimmed),
                )));
                editor.close();
            }
        }
        ChipRemove::Kind(_)
        | ChipRemove::KindNot(_)
        | ChipRemove::Host(_)
        | ChipRemove::HostNot(_)
        | ChipRemove::Source(_)
        | ChipRemove::SourceNot(_) => {
            ui.label(egui::RichText::new("Filter term").strong());
            let r = ui.text_edit_singleline(&mut editor.value_text);
            ui.checkbox(&mut editor.exclude, "exclude (!=)");
            if (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                || ui.button("apply").clicked()
            {
                apply_list_chip_edit(remove, scope, editor, actions);
                editor.close();
            }
        }
        ChipRemove::Confidence(_) => {
            ui.label(egui::RichText::new("Confidence levels").strong());
            ui.checkbox(&mut editor.confidence[0], "direct");
            ui.checkbox(&mut editor.confidence[1], "derived");
            ui.checkbox(&mut editor.confidence[2], "heuristic");
            if ui.button("apply").clicked() {
                let mut next = Vec::new();
                if editor.confidence[0] {
                    next.push(fp_core::Confidence::Direct);
                }
                if editor.confidence[1] {
                    next.push(fp_core::Confidence::Derived);
                }
                if editor.confidence[2] {
                    next.push(fp_core::Confidence::Heuristic);
                }
                actions.push(Action::Scope(ScopeAction::SetConfidence(next)));
                editor.close();
            }
        }
        ChipRemove::Run => {
            ui.label(egui::RichText::new("Run ID").strong());
            let r = ui.text_edit_singleline(&mut editor.value_text);
            if (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                || ui.button("apply").clicked()
            {
                let trimmed = editor.value_text.trim();
                if trimmed.is_empty() {
                    actions.push(Action::Scope(ScopeAction::SetRun(None)));
                    editor.close();
                } else if let Ok(id) = trimmed.parse::<u64>() {
                    actions.push(Action::Scope(ScopeAction::SetRun(Some(id))));
                    editor.close();
                } else {
                    editor.error = Some("run id must be a positive integer".into());
                }
            }
        }
        ChipRemove::Timing => {
            ui.label(egui::RichText::new("Timing filter").strong());
            if ui
                .selectable_label(!editor.untimed_only, "timed only")
                .clicked()
            {
                actions.push(Action::Scope(ScopeAction::SetTiming(
                    fp_store::EventTiming::Timed,
                )));
                editor.close();
            }
            if ui
                .selectable_label(editor.untimed_only, "untimed only")
                .clicked()
            {
                actions.push(Action::Scope(ScopeAction::SetTiming(
                    fp_store::EventTiming::Untimed,
                )));
                editor.close();
            }
            if ui.button("any timing (clear)").clicked() {
                actions.push(Action::Scope(ScopeAction::SetTiming(
                    fp_store::EventTiming::Any,
                )));
                editor.close();
            }
        }
        ChipRemove::IocOnly => {
            ui.label("Showing only events matching an IOC.");
            if ui.button("disable IOC-only filter").clicked() {
                actions.push(Action::Scope(ScopeAction::SetIocOnly(false)));
                editor.close();
            }
        }
    }

    if let Some(err) = &editor.error {
        ui.colored_label(ui.visuals().error_fg_color, err);
    }
    if ui.small_button("close").clicked() {
        editor.close();
    }
}

fn update_filter_pair(
    mut inc: Vec<String>,
    mut exc: Vec<String>,
    remove_idx: Result<usize, usize>,
    val: &str,
    exclude: bool,
) -> (Vec<String>, Vec<String>) {
    match remove_idx {
        Ok(i) if i < inc.len() => {
            inc.remove(i);
        }
        Err(i) if i < exc.len() => {
            exc.remove(i);
        }
        _ => {}
    }
    if !val.is_empty() {
        let target = if exclude { &mut exc } else { &mut inc };
        if !target.iter().any(|existing| existing == val) {
            target.push(val.to_string());
        }
    }
    (inc, exc)
}

fn apply_list_chip_edit(
    remove: &ChipRemove,
    scope: &Scope,
    editor: &ChipEditorState,
    actions: &mut Vec<Action>,
) {
    let f = scope.filter();
    let val = editor.value_text.trim();
    match remove {
        ChipRemove::Kind(i) => {
            let (kinds, kinds_not) =
                update_filter_pair(f.kinds, f.kinds_not, Ok(*i), val, editor.exclude);
            actions.push(Action::Scope(ScopeAction::SetKindFilter {
                kinds,
                kinds_not,
            }));
        }
        ChipRemove::KindNot(i) => {
            let (kinds, kinds_not) =
                update_filter_pair(f.kinds, f.kinds_not, Err(*i), val, editor.exclude);
            actions.push(Action::Scope(ScopeAction::SetKindFilter {
                kinds,
                kinds_not,
            }));
        }
        ChipRemove::Host(i) => {
            let (hosts, hosts_not) =
                update_filter_pair(f.hosts, f.hosts_not, Ok(*i), val, editor.exclude);
            actions.push(Action::Scope(ScopeAction::SetHostFilter {
                hosts,
                hosts_not,
            }));
        }
        ChipRemove::HostNot(i) => {
            let (hosts, hosts_not) =
                update_filter_pair(f.hosts, f.hosts_not, Err(*i), val, editor.exclude);
            actions.push(Action::Scope(ScopeAction::SetHostFilter {
                hosts,
                hosts_not,
            }));
        }
        ChipRemove::Source(i) => {
            let (sources, sources_not) =
                update_filter_pair(f.sources, f.sources_not, Ok(*i), val, editor.exclude);
            actions.push(Action::Scope(ScopeAction::SetSourceFilter {
                sources,
                sources_not,
            }));
        }
        ChipRemove::SourceNot(i) => {
            let (sources, sources_not) =
                update_filter_pair(f.sources, f.sources_not, Err(*i), val, editor.exclude);
            actions.push(Action::Scope(ScopeAction::SetSourceFilter {
                sources,
                sources_not,
            }));
        }
        _ => {}
    }
}

fn parse_optional_time(raw: &str, tz: Tz) -> Result<Option<i64>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    fp_query::time::parse_in(trimmed, Some(tz))
        .map(Some)
        .map_err(|e| e.to_string())
}

pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}
