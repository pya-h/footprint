//! Collapsible sanitized JSON tree widget with per-value copy.

use eframe::egui;

use crate::theme::Theme;
use crate::vm::SafeJson;

pub fn show_json_tree(ui: &mut egui::Ui, id_salt: &str, value: &SafeJson, theme: &Theme) {
    render_node(ui, ui.scope_id().with(id_salt), None, value, theme, true);
}

fn render_node(
    ui: &mut egui::Ui,
    id: egui::Id,
    key: Option<&str>,
    value: &SafeJson,
    theme: &Theme,
    default_open: bool,
) {
    match value {
        SafeJson::Null | SafeJson::Bool(_) | SafeJson::Number(_) | SafeJson::String(_) => {
            ui.horizontal_wrapped(|ui| {
                if let Some(k) = key {
                    ui.label(
                        egui::RichText::new(format!("{k}:"))
                            .monospace()
                            .color(theme.text_dim),
                    );
                }
                let text = scalar_display(value);
                let color = match value {
                    SafeJson::Null => theme.text_dim,
                    SafeJson::Bool(_) => theme.accent,
                    SafeJson::Number(_) => theme.ok,
                    SafeJson::String(_) => theme.text,
                    _ => theme.text,
                };
                ui.label(egui::RichText::new(&text).monospace().color(color));
                if ui
                    .small_button("copy")
                    .on_hover_text("copy value")
                    .clicked()
                {
                    ui.ctx().copy_text(scalar_copy_value(value));
                }
            });
        }
        SafeJson::Array(items) => {
            let label = match key {
                Some(k) => format!("{k} [{}]", items.len()),
                None => format!("[{}]", items.len()),
            };
            egui::CollapsingHeader::new(egui::RichText::new(label).monospace())
                .id_salt(id)
                .default_open(default_open && items.len() <= 16)
                .show(ui, |ui| {
                    for (idx, item) in items.iter().enumerate() {
                        let idx_label = idx.to_string();
                        render_node(ui, id.with(idx), Some(&idx_label), item, theme, false);
                    }
                });
        }
        SafeJson::Object(entries) => {
            let label = match key {
                Some(k) => format!("{k} {{{}}}", entries.len()),
                None => format!("{{{}}}", entries.len()),
            };
            egui::CollapsingHeader::new(egui::RichText::new(label).monospace())
                .id_salt(id)
                .default_open(default_open)
                .show(ui, |ui| {
                    for (idx, (k, v)) in entries.iter().enumerate() {
                        render_node(ui, id.with(idx), Some(k.as_str()), v, theme, false);
                    }
                });
        }
    }
}

fn scalar_display(value: &SafeJson) -> String {
    match value {
        SafeJson::Null => "null".to_string(),
        SafeJson::Bool(b) => b.to_string(),
        SafeJson::Number(n) => n.as_str().to_string(),
        SafeJson::String(s) => format!("\"{}\"", s.as_str()),
        SafeJson::Array(_) | SafeJson::Object(_) => value.to_pretty_string(),
    }
}

fn scalar_copy_value(value: &SafeJson) -> String {
    match value {
        SafeJson::Null => "null".to_string(),
        SafeJson::Bool(b) => b.to_string(),
        SafeJson::Number(n) => n.as_str().to_string(),
        SafeJson::String(s) => s.as_str().to_string(),
        SafeJson::Array(_) | SafeJson::Object(_) => value.to_pretty_string(),
    }
}
