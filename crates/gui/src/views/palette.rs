//! Command palette (Ctrl+K): fuzzy over views, actions, themes, event ids and
//! known hosts; Enter runs the selected command.

use eframe::egui;

use crate::model::{Action, ScopeAction, Selection, View};
use crate::theme::Theme;

#[derive(Default)]
pub struct PaletteState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
}

struct Command {
    title: String,
    hint: String,
    run: Cmd,
}

enum Cmd {
    Go(View),
    Theme(&'static str),
    Actions(Vec<fn() -> Vec<Action>>),
    Host(String),
}

impl Cmd {
    fn actions(&self) -> Vec<Action> {
        match self {
            Cmd::Go(view) => vec![Action::Go(*view)],
            Cmd::Theme(name) => vec![Action::SetTheme((*name).to_string())],
            Cmd::Actions(fns) => fns.iter().flat_map(|f| f()).collect(),
            Cmd::Host(host) => vec![
                Action::Scope(ScopeAction::SetHosts(vec![host.clone()])),
                Action::Go(View::Timeline),
            ],
        }
    }
}

fn ioc_only() -> Vec<Action> {
    vec![Action::Scope(ScopeAction::SetIocOnly(true))]
}
fn clear_scope() -> Vec<Action> {
    vec![Action::Scope(ScopeAction::Clear)]
}
fn undo_scope() -> Vec<Action> {
    vec![Action::UndoScope]
}
fn open_export() -> Vec<Action> {
    vec![Action::OpenExport]
}

pub fn toggle(palette: &mut PaletteState) {
    palette.open = !palette.open;
    palette.query.clear();
    palette.selected = 0;
}

pub fn show_palette(
    ctx: &egui::Context,
    _theme: &Theme,
    palette: &mut PaletteState,
    hosts: &[String],
    actions_out: &mut Vec<Action>,
) {
    if !palette.open {
        return;
    }
    let mut open = true;
    let mut close = false;
    egui::Window::new("Command palette")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -80.0))
        .open(&mut open)
        .show(ctx, |ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut palette.query)
                    .desired_width(420.0)
                    .hint_text("go timeline · theme dark · #1832 · us09-web-zoom.com…"),
            );
            response.request_focus();
            if response.changed() {
                palette.selected = 0;
            }

            let id_jump = id_query_action(palette.query.trim());
            let commands = build_commands(hosts);
            let matches: Vec<&Command> = if id_jump.is_some() {
                Vec::new()
            } else {
                commands
                    .iter()
                    .filter(|c| fuzzy(&palette.query, &c.title))
                    .collect()
            };
            palette.selected = palette.selected.min(matches.len().saturating_sub(1));

            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .id_salt("palette_results")
                .auto_shrink([false, false])
                .max_height(280.0)
                .show(ui, |ui| {
                    if let Some(id_action) = &id_jump {
                        let resp = ui.selectable_label(
                            true,
                            egui::RichText::new(format!("jump to event {}", palette.query.trim()))
                                .monospace(),
                        );
                        if resp.clicked() {
                            actions_out.push(match id_action {
                                Action::Select(sel) => Action::Select(sel.clone()),
                                _ => Action::Select(Selection::None),
                            });
                            close = true;
                        }
                        return;
                    }
                    for (idx, command) in matches.iter().enumerate() {
                        let selected = idx == palette.selected;
                        let resp = ui
                            .selectable_label(
                                selected,
                                egui::RichText::new(&command.title).monospace(),
                            )
                            .on_hover_text(&command.hint);
                        if resp.clicked() {
                            palette.selected = idx;
                            actions_out.extend(command.run.actions());
                            close = true;
                        }
                    }
                    if matches.is_empty() {
                        ui.label(egui::RichText::new("no matches").weak());
                    }
                });

            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            let down = ui.input(|i| i.key_pressed(egui::Key::ArrowDown));
            let up = ui.input(|i| i.key_pressed(egui::Key::ArrowUp));
            if down && !matches.is_empty() {
                palette.selected = (palette.selected + 1) % matches.len();
            }
            if up && !matches.is_empty() {
                palette.selected = (palette.selected + matches.len() - 1) % matches.len();
            }
            if enter {
                if let Some(action) = id_jump {
                    actions_out.push(action);
                    close = true;
                } else if let Some(command) = matches.get(palette.selected) {
                    actions_out.extend(command.run.actions());
                    close = true;
                }
            }
        });
    if !open || close {
        palette.open = false;
    }
}

fn build_commands(hosts: &[String]) -> Vec<Command> {
    let mut out = Vec::new();
    for view in View::ALL {
        out.push(Command {
            title: format!("go {}", view.label().to_lowercase()),
            hint: "open a view".to_string(),
            run: Cmd::Go(view),
        });
    }
    for theme in ["dark", "light", "high contrast"] {
        out.push(Command {
            title: format!("theme {theme}"),
            hint: "switch the colour theme".to_string(),
            run: Cmd::Theme(theme),
        });
    }
    out.push(Command {
        title: "scope: ioc hits only".into(),
        hint: "set the scope to events matching an IOC".into(),
        run: Cmd::Actions(vec![ioc_only]),
    });
    out.push(Command {
        title: "scope: clear".into(),
        hint: "empty the scope".into(),
        run: Cmd::Actions(vec![clear_scope]),
    });
    out.push(Command {
        title: "scope: undo".into(),
        hint: "undo the last scope edit".into(),
        run: Cmd::Actions(vec![undo_scope]),
    });
    out.push(Command {
        title: "export scope".into(),
        hint: "open the export dialog".into(),
        run: Cmd::Actions(vec![open_export]),
    });
    out.push(Command {
        title: "collect".into(),
        hint: "open the collect view".into(),
        run: Cmd::Go(View::Collect),
    });
    for host in hosts.iter().take(20) {
        let host = host.clone();
        out.push(Command {
            title: format!("host {host}"),
            hint: "add a host chip and open the timeline".into(),
            run: Cmd::Host(host),
        });
    }
    out
}

/// an `#id` query selects the event directly
pub fn id_query_action(query: &str) -> Option<Action> {
    let digits = query.strip_prefix('#')?;
    let id: u64 = digits.parse().ok()?;
    Some(Action::Select(Selection::Event(id)))
}

fn fuzzy(needle: &str, haystack: &str) -> bool {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    if let Some(id_action) = id_query_action(&needle) {
        let _ = id_action;
        return true;
    }
    let mut chars = needle.chars();
    let mut current = chars.next();
    for c in haystack.to_lowercase().chars() {
        if current.is_some_and(|n| n == c) {
            current = chars.next();
        }
    }
    current.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matches_subsequence_and_handles_empty() {
        assert!(fuzzy("", "go timeline"));
        assert!(fuzzy("gtl", "go timeline"));
        assert!(fuzzy("Trace", "go trace"));
        assert!(!fuzzy("xyz", "go trace"));
    }

    #[test]
    fn hash_query_selects_the_event() {
        assert!(matches!(
            id_query_action("#1832"),
            Some(Action::Select(Selection::Event(1832)))
        ));
        assert!(id_query_action("1832").is_none());
        assert!(id_query_action("#abc").is_none());
    }
}
