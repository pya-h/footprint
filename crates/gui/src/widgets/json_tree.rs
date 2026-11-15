//! Collapsible sanitized JSON tree widget with per-value copy, plus a two-sided
//! diff view that highlights leaves which differ between run A and run B.

use eframe::egui;

use crate::theme::Theme;
use crate::vm::SafeJson;

pub fn show_json_tree(ui: &mut egui::Ui, id_salt: &str, value: &SafeJson, theme: &Theme) {
    render_node(ui, ui.scope_id().with(id_salt), None, value, theme, true);
}

/// Side-by-side A/B tree: keys aligned, values from both runs next to each
/// other, leaves that differ carry a warn marker on the B side.
pub fn show_json_diff(ui: &mut egui::Ui, id_salt: &str, a: &SafeJson, b: &SafeJson, theme: &Theme) {
    render_diff_node(ui, ui.scope_id().with(id_salt), None, a, b, theme, true);
}

fn render_diff_node(
    ui: &mut egui::Ui,
    id: egui::Id,
    key: Option<&str>,
    a: &SafeJson,
    b: &SafeJson,
    theme: &Theme,
    default_open: bool,
) {
    if same_shape(a, b) {
        match (a, b) {
            (SafeJson::Array(items_a), SafeJson::Array(items_b)) => {
                let label = match key {
                    Some(k) => format!("{k} [{}]", items_b.len()),
                    None => format!("[{}]", items_b.len()),
                };
                egui::CollapsingHeader::new(egui::RichText::new(label).monospace())
                    .id_salt(id)
                    .default_open(default_open && items_b.len() <= 16)
                    .show(ui, |ui| {
                        for idx in 0..items_a.len().max(items_b.len()) {
                            let idx_label = idx.to_string();
                            let child_a = items_a.get(idx).unwrap_or(&SafeJson::Null);
                            let child_b = items_b.get(idx).unwrap_or(&SafeJson::Null);
                            render_diff_node(
                                ui,
                                id.with(idx),
                                Some(&idx_label),
                                child_a,
                                child_b,
                                theme,
                                false,
                            );
                        }
                    });
                return;
            }
            (SafeJson::Object(entries_a), SafeJson::Object(entries_b)) => {
                let mut keys: Vec<&str> = Vec::new();
                for (k, _) in entries_a.iter().chain(entries_b.iter()) {
                    if !keys.contains(&k.as_str()) {
                        keys.push(k.as_str());
                    }
                }
                let label = match key {
                    Some(k) => format!("{k} {{{}}}", entries_b.len()),
                    None => format!("{{{}}}", entries_b.len()),
                };
                egui::CollapsingHeader::new(egui::RichText::new(label).monospace())
                    .id_salt(id)
                    .default_open(default_open)
                    .show(ui, |ui| {
                        for (idx, k) in keys.iter().enumerate() {
                            let child_a = entries_a
                                .iter()
                                .find(|(ek, _)| ek.as_str() == *k)
                                .map(|(_, v)| v)
                                .unwrap_or(&SafeJson::Null);
                            let child_b = entries_b
                                .iter()
                                .find(|(ek, _)| ek.as_str() == *k)
                                .map(|(_, v)| v)
                                .unwrap_or(&SafeJson::Null);
                            render_diff_node(
                                ui,
                                id.with(idx),
                                Some(k),
                                child_a,
                                child_b,
                                theme,
                                false,
                            );
                        }
                    });
                return;
            }
            _ => {}
        }
    }
    // leaf, a missing side, or a shape change: one row with both values
    let changed = !json_equal(a, b);
    ui.horizontal_wrapped(|ui| {
        if let Some(k) = key {
            ui.label(
                egui::RichText::new(format!("{k}:"))
                    .monospace()
                    .color(theme.text_dim),
            );
        }
        let dim = theme.text_dim;
        ui.label(
            egui::RichText::new(scalar_display(a))
                .monospace()
                .color(dim),
        );
        if changed {
            // ASCII arrow: the bundled fonts are not guaranteed to carry →
            ui.label(egui::RichText::new("->").monospace().color(theme.warn));
            ui.label(
                egui::RichText::new(scalar_display(b))
                    .monospace()
                    .color(theme.warn),
            );
            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 14.0), egui::Sense::hover());
            ui.painter().circle_filled(rect.center(), 3.0, theme.warn);
        } else {
            ui.label(
                egui::RichText::new(scalar_display(b))
                    .monospace()
                    .color(theme.text),
            );
        }
    });
}

fn same_shape(a: &SafeJson, b: &SafeJson) -> bool {
    matches!(
        (a, b),
        (SafeJson::Array(_), SafeJson::Array(_)) | (SafeJson::Object(_), SafeJson::Object(_))
    )
}

fn json_equal(a: &SafeJson, b: &SafeJson) -> bool {
    scalar_display(a) == scalar_display(b)
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
