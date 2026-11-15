//! Help overlay (F1 or ?): the keyboard map and a legend of every badge,
//! glyph and colour the GUI uses.

use eframe::egui;

use crate::theme::Theme;

pub fn show_help(ctx: &egui::Context, theme: &Theme, open: &mut bool) {
    if !*open {
        return;
    }
    let mut keep = true;
    egui::Window::new("Help")
        .collapsible(false)
        .resizable(true)
        .default_size([560.0, 640.0])
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .open(&mut keep)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("help_body")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Keyboard").strong());
                    for (keys, what) in [
                        ("1…0", "open a view (rail order)"),
                        ("Ctrl+K", "command palette"),
                        ("Ctrl+Z / Ctrl+Y", "undo / redo the scope"),
                        ("Ctrl+Shift+C", "copy the scope as a footprint command"),
                        ("Ctrl+Shift+Backspace", "clear the scope"),
                        ("Ctrl++ / Ctrl+- / Ctrl+0", "zoom in / out / reset"),
                        ("/", "focus the search box"),
                        ("f / i", "toggle the facets / inspector panels"),
                        (
                            "Alt+Left / Alt+Right",
                            "back / forward in selection history",
                        ),
                        ("Esc", "close dialogs, then clear the selection"),
                        ("F1 or ?", "this help"),
                    ] {
                        ui.horizontal(|ui| {
                            ui.monospace(format!("{keys:<26}"));
                            ui.label(what);
                        });
                    }
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("Confidence discs").strong());
                    for (shape, what) in [
                        ("filled disc", "direct — read straight from the record"),
                        ("half disc", "derived — computed from a related record"),
                        ("hollow ring", "heuristic — guessed by proximity"),
                    ] {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                            let sample = match shape {
                                "filled disc" => crate::widgets::badge::ConfidenceGlyph::Direct,
                                "half disc" => crate::widgets::badge::ConfidenceGlyph::Derived,
                                _ => crate::widgets::badge::ConfidenceGlyph::Heuristic,
                            };
                            crate::widgets::badge::confidence_disc(
                                ui.painter(),
                                rect.center(),
                                5.0,
                                sample,
                                theme.accent,
                            );
                            ui.label(what);
                        });
                    }
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("Status marks").strong());
                    for (glyph, what) in [
                        ("check", "ok — matches the package record"),
                        ("cross", "mismatch — differs from the package record"),
                        (
                            "cross with tick",
                            "missing — the package lists it, it is gone",
                        ),
                        ("triangle + bar", "unowned — no installed package claims it"),
                        ("triangle", "partial — some of it was readable"),
                        ("?", "unverified — the check was not possible"),
                        ("flag", "IOC — matched an indicator"),
                    ] {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                            let sample = match glyph {
                                "check" => crate::widgets::badge::StatusGlyph::Ok,
                                "cross" => crate::widgets::badge::StatusGlyph::Mismatch,
                                "cross with tick" => crate::widgets::badge::StatusGlyph::Missing,
                                "triangle + bar" => crate::widgets::badge::StatusGlyph::Unowned,
                                "triangle" => crate::widgets::badge::StatusGlyph::Partial,
                                "?" => crate::widgets::badge::StatusGlyph::Unverified,
                                _ => crate::widgets::badge::StatusGlyph::Ioc,
                            };
                            crate::widgets::badge::status_mark(
                                ui.painter(),
                                rect.center(),
                                5.0,
                                sample,
                                sample.color(theme),
                            );
                            ui.label(what);
                        });
                    }
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("Colours").strong());
                    for (name, color, what) in [
                        (
                            "accent",
                            theme.accent,
                            "links, selections, the current focus",
                        ),
                        ("ok", theme.ok, "covered, present, direct evidence"),
                        ("warn", theme.warn, "partial, unowned, changed fields"),
                        ("danger", theme.danger, "errors, mismatches, missing files"),
                        ("ioc", theme.ioc, "indicator hits and IOC-tagged rows"),
                    ] {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(16.0, 12.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 2.0, color);
                            ui.monospace(format!("{name:<8}"));
                            ui.label(what);
                        });
                    }
                });
        });
    if !keep {
        *open = false;
    }
}
