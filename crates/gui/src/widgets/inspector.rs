//! Persistent event inspector: follows selection (or pins), navigates selection
//! history, explains forensics metadata in plain language, guards against RTL/bidi
//! tricks with a non-dismissible escaped preview, and connects links & context.

use eframe::egui;
use fp_core::Confidence;
use fp_query::{LinkDirection, Tz};

use crate::model::{Action, LoadStatus, Model, ScopeAction, Selection, View};
use crate::theme::Theme;
use crate::vm::DetailVm;
use crate::widgets::badge::{ConfidenceGlyph, confidence_disc};
use crate::widgets::chip::thousands;
use crate::widgets::json_tree::show_json_tree;

pub fn show_inspector(
    ui: &mut egui::Ui,
    model: &Model,
    theme: &Theme,
    tz: Tz,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        if ui
            .add_enabled(model.nav.can_back(), egui::Button::new("‹"))
            .on_hover_text("back in selection history (Alt+Left)")
            .clicked()
        {
            actions.push(Action::NavBack);
        }
        if ui
            .add_enabled(model.nav.can_forward(), egui::Button::new("›"))
            .on_hover_text("forward in selection history (Alt+Right)")
            .clicked()
        {
            actions.push(Action::NavForward);
        }
        ui.label(egui::RichText::new("Inspector").strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let pinned = model.inspector_pin.is_some();
            let pin_label = if pinned { "pinned" } else { "pin" };
            if ui
                .selectable_label(pinned, pin_label)
                .on_hover_text("keep inspector fixed on this item while browsing")
                .clicked()
            {
                actions.push(Action::TogglePinInspector);
            }
        });
    });
    ui.separator();

    match model.inspected() {
        Selection::None => {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new(
                        "select an event to inspect details, links, and provenance",
                    )
                    .weak(),
                );
            });
        }
        Selection::Event(id) | Selection::TraceNode(id) => {
            show_event_inspector(ui, id, model, theme, tz, actions);
        }
        Selection::State(id) => {
            show_state_inspector(ui, id, model, theme);
        }
        Selection::Ioc(id) => {
            show_ioc_inspector(ui, id, model, theme, actions);
        }
        Selection::Source(id) => {
            show_source_inspector(ui, id, model, theme);
        }
    }
}

fn show_event_inspector(
    ui: &mut egui::Ui,
    _selected_id: u64,
    model: &Model,
    theme: &Theme,
    tz: Tz,
    actions: &mut Vec<Action>,
) {
    match model.detail.status(model.selection_gen) {
        LoadStatus::Loading => {
            ui.label(egui::RichText::new("loading event…").weak());
            return;
        }
        LoadStatus::Error(err) => {
            ui.colored_label(theme.danger, err);
            return;
        }
        LoadStatus::Ready | LoadStatus::Stale => {}
    }

    let Some(detail_opt) = model.detail.value() else {
        return;
    };
    let Some(detail) = detail_opt.as_ref() else {
        ui.label(egui::RichText::new("event no longer present in case").weak());
        return;
    };

    egui::ScrollArea::vertical()
        .id_salt("inspector_scroll")
        .show(ui, |ui| {
            if !detail.escaped_warnings.is_empty() {
                show_bidi_warning(ui, detail, theme);
                ui.add_space(4.0);
            }

            ui.horizontal_wrapped(|ui| {
                let cat_color = theme.category_color(detail.category);
                let (swatch, _) =
                    ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter().rect_filled(swatch, 2.0, cat_color);
                ui.label(egui::RichText::new(detail.kind.as_str()).strong());

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let conf_label = confidence_name(detail.confidence);
                    let resp = ui.horizontal(|ui| {
                        let (r, _) =
                            ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                        confidence_disc(
                            ui.painter(),
                            r.center(),
                            4.0,
                            ConfidenceGlyph::from(detail.confidence),
                            theme.text,
                        );
                        ui.label(conf_label);
                    });
                    resp.response
                        .on_hover_text(confidence_help(detail.confidence));
                });
            });

            show_timestamp_block(ui, detail, theme, tz);

            ui.label(
                egui::RichText::new(format!(
                    "{} · run {} · #{}",
                    detail.source.as_str(),
                    detail.run_id,
                    detail.id
                ))
                .color(theme.text_dim)
                .small(),
            );
            ui.separator();

            if let Some(subject) = &detail.subject {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("SUBJECT").small().strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("copy").clicked() {
                            ui.ctx().copy_text(subject.as_str().to_string());
                        }
                    });
                });
                ui.add(egui::Label::new(egui::RichText::new(subject.as_str()).monospace()).wrap());
                ui.add_space(4.0);
            }

            if let Some(host) = &detail.subject_host {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("HOST").small().strong());
                    ui.label(egui::RichText::new(host.as_str()).monospace());
                    if ui.small_button("filter").clicked() {
                        actions.push(Action::Scope(ScopeAction::SetHosts(vec![
                            host.as_str().to_string(),
                        ])));
                    }
                    if ui.small_button("exclude").clicked() {
                        let mut exc = model.scope.filter().hosts_not;
                        let val = host.as_str().to_string();
                        if !exc.contains(&val) {
                            exc.push(val);
                        }
                        actions.push(Action::Scope(ScopeAction::SetHostsNot(exc)));
                    }
                });
                ui.add_space(4.0);
            }

            if let Some(summary) = &detail.summary {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("SUMMARY").small().strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("copy").clicked() {
                            ui.ctx().copy_text(summary.as_str().to_string());
                        }
                    });
                });
                ui.add(egui::Label::new(summary.as_str()).wrap());
            }

            ui.separator();

            show_links_section(ui, detail, theme, actions);
            show_around_section(ui, model, theme, tz, actions);

            if let Some(attrs) = &detail.attrs {
                egui::CollapsingHeader::new("ATTRIBUTES")
                    .default_open(true)
                    .show(ui, |ui| {
                        show_json_tree(ui, "attrs", attrs, theme);
                    });
            }
            if let Some(actor) = &detail.actor {
                egui::CollapsingHeader::new("ACTOR")
                    .default_open(false)
                    .show(ui, |ui| {
                        show_json_tree(ui, "actor", actor, theme);
                    });
            }

            show_provenance_section(ui, detail, theme);

            ui.separator();
            ui.horizontal_wrapped(|ui| {
                if ui
                    .button("Trace")
                    .on_hover_text("open causal trace rooted at this event")
                    .clicked()
                {
                    actions.push(Action::Go(View::Trace));
                    actions.push(Action::Select(Selection::Event(detail.id)));
                    actions.push(Action::RequestTrace(detail.id.to_string()));
                }
                if ui
                    .button("Show in context")
                    .on_hover_text("clear scope and jump timeline to this instant")
                    .clicked()
                {
                    actions.push(Action::ShowInContext(detail.ts_utc, detail.id));
                }
                if ui.button("Copy ID").clicked() {
                    ui.ctx().copy_text(detail.id.to_string());
                }
            });
        });
}

fn show_ioc_inspector(
    ui: &mut egui::Ui,
    ioc_id: u64,
    model: &Model,
    theme: &Theme,
    actions: &mut Vec<Action>,
) {
    let Some(ioc) = model
        .iocs
        .value()
        .and_then(|rows| rows.iter().find(|r| r.id == ioc_id))
    else {
        ui.label(egui::RichText::new("indicator not in the current query").weak());
        return;
    };
    ui.label(
        egui::RichText::new(format!("indicator · {}", ioc.ioc_type.as_str()))
            .small()
            .color(theme.text_dim),
    );
    ui.label(egui::RichText::new(ioc.value.as_str()).monospace().strong());
    if let Some(note) = &ioc.note {
        ui.label(egui::RichText::new(note.as_str()).small());
    }
    ui.label(
        egui::RichText::new(format!("{} linked hits", thousands(ioc.hits)))
            .small()
            .color(if ioc.hits > 0 {
                theme.ioc
            } else {
                theme.text_dim
            }),
    );
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if ui.small_button("copy value").clicked() {
            ui.ctx().copy_text(ioc.value.as_str().to_string());
        }
        if ui
            .small_button("show hits in timeline")
            .on_hover_text("scope the timeline to events matching an IOC")
            .clicked()
        {
            actions.push(Action::Scope(crate::model::ScopeAction::SetIocOnly(true)));
            actions.push(Action::Go(crate::model::View::Timeline));
        }
    });
}

fn show_source_inspector(ui: &mut egui::Ui, source_id: u64, model: &Model, theme: &Theme) {
    let Some(row) = model
        .sources
        .value()
        .and_then(|data| data.health_rows.iter().find(|r| r.source_id == source_id))
    else {
        ui.label(egui::RichText::new("source not in the current query").weak());
        return;
    };
    ui.label(
        egui::RichText::new(format!("{}  ·  {}", row.label.as_str(), row.kind.as_str())).strong(),
    );
    if let Some(data) = model.sources.value() {
        ui.add_space(4.0);
        for (run_idx, cell) in row.cells.iter().enumerate() {
            let Some(run_id) = data.run_ids.get(run_idx) else {
                continue;
            };
            let text = cell
                .as_ref()
                .map(|a| {
                    crate::vm::SafeText::new(&a.to_string())
                        .as_str()
                        .to_string()
                })
                .unwrap_or_else(|| "did not run".to_string());
            let color = match cell {
                Some(fp_core::Availability::Present) => theme.ok,
                Some(fp_core::Availability::NotInstalled) | Some(fp_core::Availability::Empty) => {
                    theme.text_dim
                }
                Some(_) => theme.warn,
                None => theme.text_dim,
            };
            ui.horizontal(|ui| {
                ui.monospace(format!("run {run_id:<3}"));
                ui.colored_label(color, text);
            });
        }
    }
}

fn show_state_inspector(ui: &mut egui::Ui, rowid: i64, model: &Model, theme: &Theme) {
    let Some(row) = model
        .states
        .value()
        .and_then(|rows| rows.iter().find(|r| r.rowid == rowid))
    else {
        ui.label(egui::RichText::new("state record not in the current query").weak());
        return;
    };
    ui.label(
        egui::RichText::new(format!("{}  ·  run {}", row.kind.as_str(), row.run_id))
            .small()
            .color(theme.text_dim),
    );
    ui.label(egui::RichText::new(row.key.as_str()).monospace().strong());
    if let Some(locator) = &row.locator {
        ui.label(
            egui::RichText::new(format!("source: {}", locator.as_str()))
                .small()
                .weak(),
        );
    }
    if !row.flags.is_empty() {
        ui.add_space(4.0);
        let words: Vec<&str> = row.flags.iter().map(|f| f.as_str()).collect();
        ui.label(
            egui::RichText::new(format!("flags: [{}]", words.join(", ")))
                .small()
                .color(theme.warn),
        );
    }
    let explanations = crate::views::state::explain_row(row);
    if !explanations.is_empty() {
        ui.add_space(2.0);
        for line in &explanations {
            ui.label(egui::RichText::new(line).small().weak());
        }
    }
    ui.separator();
    match &row.value {
        crate::vm::SafeJson::Null => {
            ui.label(egui::RichText::new("value did not parse as JSON; raw:").weak());
            ui.monospace(row.raw.as_str());
        }
        value => {
            show_json_tree(ui, "state_value", value, theme);
        }
    }
}

fn show_bidi_warning(ui: &mut egui::Ui, detail: &DetailVm, theme: &Theme) {
    egui::Frame::group(ui.style())
        .fill(theme.bg2)
        .stroke(egui::Stroke::new(1.0, theme.warn))
        .corner_radius(egui::CornerRadius::same(4))
        .show(ui, |ui| {
            ui.colored_label(
                theme.warn,
                "RTL override or control characters present in raw evidence (stripped in display).",
            );
            for (field, escaped) in &detail.escaped_warnings {
                ui.label(
                    egui::RichText::new(format!("{field}: {escaped}"))
                        .monospace()
                        .small(),
                );
            }
        });
}

fn show_timestamp_block(ui: &mut egui::Ui, detail: &DetailVm, theme: &Theme, tz: Tz) {
    let main_time = if detail.untimed {
        "untimed (order only)".to_string()
    } else {
        fp_query::format_time(detail.ts_utc, "exact", tz, "%Y-%m-%d %H:%M:%S%.6f")
    };
    let zone_label = match tz {
        Tz::Local => "local".to_string(),
        Tz::Utc => "UTC".to_string(),
        Tz::Fixed(off) => off.to_string(),
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(format!("{main_time}  {zone_label}")).monospace());
        let info = build_time_tooltip(detail);
        ui.label(egui::RichText::new("[i]").color(theme.accent))
            .on_hover_text(info);
    });
}

fn build_time_tooltip(detail: &DetailVm) -> String {
    let mut lines = Vec::new();
    if detail.untimed {
        lines.push("Precision: order_only — source carries no clock timestamp.".to_string());
    } else {
        lines.push(format!(
            "UTC:   {}",
            fp_query::format_time(detail.ts_utc, "exact", Tz::Utc, "%Y-%m-%d %H:%M:%S%.6f")
        ));
        lines.push(format!(
            "Local: {}",
            fp_query::format_time(detail.ts_utc, "exact", Tz::Local, "%Y-%m-%d %H:%M:%S%.6f")
        ));
        lines.push(format!(
            "Precision: {} ({})",
            detail.ts_precision.as_str(),
            precision_help(detail.ts_precision.as_str())
        ));
    }
    if let Some(raw) = &detail.ts_raw {
        let fmt = detail
            .ts_raw_format
            .as_ref()
            .map(crate::vm::SafeText::as_str)
            .unwrap_or("unknown");
        lines.push(format!("Raw timestamp: {} ({fmt})", raw.as_str()));
    }
    if let Some(assumed) = &detail.ts_tz_assumed {
        lines.push(format!(
            "Assumed TZ: {} — source stored naive local time without an explicit offset.",
            assumed.as_str()
        ));
    }
    lines.join("\n")
}

fn show_links_section(
    ui: &mut egui::Ui,
    detail: &DetailVm,
    theme: &Theme,
    actions: &mut Vec<Action>,
) {
    let title = format!("LINKS ({})", detail.links.len());
    egui::CollapsingHeader::new(title)
        .default_open(!detail.links.is_empty())
        .show(ui, |ui| {
            if detail.links.is_empty() {
                ui.label(egui::RichText::new("no links recorded").weak().small());
                return;
            }
            for link in &detail.links {
                let arrow = match link.direction {
                    LinkDirection::Outgoing => ">",
                    LinkDirection::Incoming => "<",
                };
                ui.horizontal_wrapped(|ui| {
                    let (r, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    confidence_disc(
                        ui.painter(),
                        r.center(),
                        3.5,
                        ConfidenceGlyph::from(link.confidence),
                        theme.text,
                    );
                    let text = format!(
                        "{arrow} {} · {}",
                        link.rel.as_str(),
                        link.target_label.as_str()
                    );
                    let resp = ui.button(text);
                    if let Some(why) = &link.why {
                        let why_str = why.as_str();
                        if resp.hovered() {
                            resp.clone().on_hover_text(why_str);
                        }
                    }
                    if resp.clicked() {
                        match link.target_type.as_str() {
                            "event" => {
                                actions.push(Action::JumpTo(Selection::Event(link.target_id)))
                            }
                            "state" => {
                                if let Ok(id) = i64::try_from(link.target_id) {
                                    actions.push(Action::JumpTo(Selection::State(id)));
                                }
                            }
                            _ => {}
                        }
                    }
                });
            }
        });
}

fn show_around_section(
    ui: &mut egui::Ui,
    model: &Model,
    theme: &Theme,
    tz: Tz,
    actions: &mut Vec<Action>,
) {
    let count = model.around.value().map_or(0, Vec::len);
    egui::CollapsingHeader::new(format!("AROUND THIS EVENT (±5 min, {count})"))
        .default_open(false)
        .show(ui, |ui| {
            match model.around.status(model.selection_gen) {
                LoadStatus::Loading => {
                    ui.label(egui::RichText::new("loading context…").weak().small());
                    return;
                }
                LoadStatus::Error(e) => {
                    ui.colored_label(theme.danger, e);
                    return;
                }
                LoadStatus::Ready | LoadStatus::Stale => {}
            }
            let Some(rows) = model.around.value() else {
                return;
            };
            if rows.is_empty() {
                ui.label(
                    egui::RichText::new("no neighbouring events in ±5 min")
                        .weak()
                        .small(),
                );
                return;
            }
            for row in rows {
                let time = if row.untimed {
                    "untimed".to_string()
                } else {
                    fp_query::format_time(row.ts_utc, "exact", tz, "%H:%M:%S")
                };
                let label = format!("{time}  {}  {}", row.kind.as_str(), row.display_text());
                if ui.selectable_label(false, label).clicked() {
                    actions.push(Action::JumpTo(Selection::Event(row.id)));
                }
            }
        });
}

fn show_provenance_section(ui: &mut egui::Ui, detail: &DetailVm, theme: &Theme) {
    egui::CollapsingHeader::new("PROVENANCE")
        .default_open(false)
        .show(ui, |ui| {
            if let Some(file) = &detail.prov_file {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("file:").color(theme.text_dim));
                    ui.label(egui::RichText::new(file.as_str()).monospace());
                    if ui.small_button("copy").clicked() {
                        ui.ctx().copy_text(file.as_str().to_string());
                    }
                });
            }
            if let Some(sha) = &detail.prov_sha256 {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("sha256:").color(theme.text_dim));
                    ui.label(egui::RichText::new(sha.as_str()).monospace());
                    if ui.small_button("copy").clicked() {
                        ui.ctx().copy_text(sha.as_str().to_string());
                    }
                });
            }
            if let Some(loc) = &detail.locator {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new("locator:").color(theme.text_dim));
                    ui.label(egui::RichText::new(loc.as_str()).monospace());
                });
            }
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(
                    "What is this? Provenance records the exact file or database copy footprint read (its SHA-256) and the line/row locator inside it, so every timeline row traces back to immutable evidence.",
                )
                .weak()
                .small(),
            );
        });
}

fn confidence_name(c: Confidence) -> &'static str {
    match c {
        Confidence::Direct => "direct",
        Confidence::Derived => "derived",
        Confidence::Heuristic => "heuristic",
    }
}

fn confidence_help(c: Confidence) -> &'static str {
    match c {
        Confidence::Direct => {
            "Direct: timestamp and facts were read straight from the source record."
        }
        Confidence::Derived => {
            "Derived: inferred cleanly from nearby records (e.g. a shell command timed from its sudo/apt run)."
        }
        Confidence::Heuristic => {
            "Heuristic: linked by time proximity or naming pattern; verify before relying on it."
        }
    }
}

fn precision_help(p: &str) -> &'static str {
    match p {
        "exact" => "sub-second precision preserved by source",
        "second" => "accurate to whole seconds",
        "minute" => "source recorded minute resolution only",
        "day" => "source recorded date only",
        "order_only" => "sequence order only, no wall clock",
        _ => "source-reported precision",
    }
}
