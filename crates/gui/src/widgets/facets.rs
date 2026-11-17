//! Facet panel: category distribution, lazy-loaded SQL value facets with
//! include/multi-select/exclude actions, timing summary, and IOC-only toggle.

use eframe::egui;
use fp_query::category::Category;
use fp_query::overview::Facet;
use fp_store::{EventTiming, FacetPage};

use crate::model::{Action, Model, ScopeAction};
use crate::theme::{Theme, contrast};
use crate::vm::SafeText;
use crate::widgets::chip::thousands;

const SHARE_BAR_WIDTH: f32 = 28.0;
const SHARE_BAR_HEIGHT: f32 = 4.0;

#[derive(Debug, Default)]
pub struct FacetsState {
    pub kind_open: bool,
    pub source_open: bool,
    pub host_open: bool,
    pub confidence_open: bool,
    pub timing_open: bool,
    pub run_open: bool,
    pub host_filter: String,
}

impl FacetsState {
    pub fn expanded_facets(&self) -> Vec<Facet> {
        let mut out = Vec::new();
        if self.kind_open {
            out.push(Facet::Kind);
        }
        if self.source_open {
            out.push(Facet::Source);
        }
        if self.host_open {
            out.push(Facet::Host);
        }
        if self.confidence_open {
            out.push(Facet::Confidence);
        }
        if self.run_open {
            out.push(Facet::Run);
        }
        out
    }
}

pub fn show_facets(
    ui: &mut egui::Ui,
    model: &Model,
    theme: &Theme,
    state: &mut FacetsState,
    actions: &mut Vec<Action>,
) {
    let filter = model.scope.filter();
    egui::ScrollArea::vertical()
        .id_salt("facets_scroll")
        .show(ui, |ui| {
            ui.label(egui::RichText::new("CATEGORY").small().strong());
            show_categories(ui, model, theme, &filter, actions);
            ui.separator();

            show_collapsing_facet(
                ui,
                FacetSection {
                    title: "KIND",
                    page: model.facets.get(&Facet::Kind).and_then(|s| s.value()),
                    included: &filter.kinds,
                    excluded: &filter.kinds_not,
                    theme,
                },
                &mut state.kind_open,
                None,
                |value, multi, exclude| {
                    let (kinds, kinds_not) = toggle_string_lists(
                        &filter.kinds,
                        &filter.kinds_not,
                        value,
                        multi,
                        exclude,
                    );
                    actions.push(Action::Scope(ScopeAction::SetKindFilter {
                        kinds,
                        kinds_not,
                    }));
                },
            );

            show_collapsing_facet(
                ui,
                FacetSection {
                    title: "SOURCE",
                    page: model.facets.get(&Facet::Source).and_then(|s| s.value()),
                    included: &filter.sources,
                    excluded: &filter.sources_not,
                    theme,
                },
                &mut state.source_open,
                None,
                |value, multi, exclude| {
                    let (sources, sources_not) = toggle_string_lists(
                        &filter.sources,
                        &filter.sources_not,
                        value,
                        multi,
                        exclude,
                    );
                    actions.push(Action::Scope(ScopeAction::SetSourceFilter {
                        sources,
                        sources_not,
                    }));
                },
            );
            if state.source_open {
                ui.label(
                    egui::RichText::new(
                        "Counts are per identity; an app filter also includes its profiles.",
                    )
                    .small()
                    .weak(),
                );
            }

            show_collapsing_facet(
                ui,
                FacetSection {
                    title: "HOST",
                    page: model.facets.get(&Facet::Host).and_then(|s| s.value()),
                    included: &filter.hosts,
                    excluded: &filter.hosts_not,
                    theme,
                },
                &mut state.host_open,
                Some(&mut state.host_filter),
                |value, multi, exclude| {
                    let (hosts, hosts_not) = toggle_string_lists(
                        &filter.hosts,
                        &filter.hosts_not,
                        value,
                        multi,
                        exclude,
                    );
                    actions.push(Action::Scope(ScopeAction::SetHostFilter {
                        hosts,
                        hosts_not,
                    }));
                },
            );

            show_confidence_facet(
                ui,
                &mut state.confidence_open,
                model.facets.get(&Facet::Confidence).and_then(|s| s.value()),
                &filter.confidence,
                theme,
                actions,
            );

            show_timing_facet(ui, &mut state.timing_open, model, theme, &filter, actions);

            show_run_facet(
                ui,
                &mut state.run_open,
                model.facets.get(&Facet::Run).and_then(|s| s.value()),
                filter.run_id,
                theme,
                actions,
            );

            ui.separator();
            let mut ioc_only = filter.ioc_only;
            let count_suffix = match model.ioc_count.value() {
                Some(&n) => format!(" ({})", thousands(n)),
                None => String::new(),
            };
            if ui
                .checkbox(&mut ioc_only, format!("IOC hits only{count_suffix}"))
                .clicked()
            {
                actions.push(Action::Scope(ScopeAction::SetIocOnly(ioc_only)));
            }
        });
}

fn show_categories(
    ui: &mut egui::Ui,
    model: &Model,
    theme: &Theme,
    filter: &fp_query::TimelineFilter,
    actions: &mut Vec<Action>,
) {
    let counts = model.category.value().copied().unwrap_or([0; 9]);
    let max_count = counts.iter().copied().max().unwrap_or(0).max(1);
    let mods = ui.input(|i| i.modifiers);

    for (idx, &cat) in Category::ALL.iter().enumerate() {
        let count = counts[idx];
        let included = category_included(filter, cat);
        let excluded = category_excluded(filter, cat);

        ui.horizontal(|ui| {
            let (swatch_rect, _) =
                ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
            let color = if count == 0 && !included && !excluded {
                theme.category_color(cat).linear_multiply(0.35)
            } else {
                theme.category_color(cat)
            };
            ui.painter().rect_filled(swatch_rect, 2.0, color);
            ui.painter().text(
                swatch_rect.center(),
                egui::Align2::CENTER_CENTER,
                Theme::category_letter(cat),
                egui::FontId::monospace(10.0),
                if contrast(theme.bg0, color) >= contrast(theme.text, color) {
                    theme.bg0
                } else {
                    theme.text
                },
            );

            let label_text = if excluded {
                egui::RichText::new(cat.label())
                    .strikethrough()
                    .color(theme.text_dim)
            } else if count == 0 && !included {
                egui::RichText::new(cat.label()).color(theme.text_dim)
            } else if included {
                egui::RichText::new(cat.label())
                    .strong()
                    .color(theme.accent)
            } else {
                egui::RichText::new(cat.label()).color(theme.text)
            };

            let row_resp = ui.add(
                egui::Button::new(label_text)
                    .frame(false)
                    .selected(included),
            );
            if row_resp.clicked() {
                let (kinds, kinds_not) =
                    apply_category_click(filter, cat, mods.ctrl || mods.shift, mods.alt);
                actions.push(Action::Scope(ScopeAction::SetKindFilter {
                    kinds,
                    kinds_not,
                }));
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let exclude_other = cat == Category::Other;
                let exclude = ui
                    .add_enabled(!exclude_other, egui::Button::new("!=").small())
                    .on_hover_text(if exclude_other {
                        "Other kinds cannot be excluded as a group"
                    } else {
                        "exclude category (Alt+click)"
                    });
                if exclude.clicked() {
                    let (kinds, kinds_not) = apply_category_click(filter, cat, false, true);
                    actions.push(Action::Scope(ScopeAction::SetKindFilter {
                        kinds,
                        kinds_not,
                    }));
                }
                let count_text = if count == 0 {
                    egui::RichText::new("0").color(theme.text_dim).small()
                } else {
                    egui::RichText::new(thousands(count)).small()
                };
                ui.label(count_text);
                draw_share_bar(ui, count, max_count, excluded, theme);
            });
        });
    }
}

struct FacetSection<'a> {
    title: &'a str,
    page: Option<&'a FacetPage>,
    included: &'a [String],
    excluded: &'a [String],
    theme: &'a Theme,
}

fn show_collapsing_facet(
    ui: &mut egui::Ui,
    section: FacetSection<'_>,
    is_open: &mut bool,
    mut search_box: Option<&mut String>,
    mut on_pick: impl FnMut(&str, bool, bool),
) {
    let FacetSection {
        title,
        page,
        included,
        excluded,
        theme,
    } = section;
    let header = facet_header_text(title, page);
    let resp = egui::CollapsingHeader::new(header)
        .default_open(*is_open)
        .show(ui, |ui| {
            if let Some(query) = search_box.as_deref_mut() {
                ui.add(
                    egui::TextEdit::singleline(query)
                        .hint_text("filter hosts…")
                        .desired_width(f32::INFINITY),
                );
            }
            let Some(page) = page else {
                ui.label(egui::RichText::new("loading…").weak().small());
                return;
            };
            let max_count = page
                .values
                .iter()
                .map(|v| v.count)
                .max()
                .unwrap_or(0)
                .max(1);
            let needle = search_box
                .as_deref()
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_default();
            let mods = ui.input(|i| i.modifiers);

            let mut shown = 0usize;
            for item in &page.values {
                let Some(raw) = item.key.as_deref() else {
                    continue;
                };
                let safe = SafeText::new(raw);
                if !needle.is_empty() && !safe.as_str().to_ascii_lowercase().contains(&needle) {
                    continue;
                }
                shown += 1;
                let is_inc = included.iter().any(|s| s == raw);
                let is_exc = excluded.iter().any(|s| s == raw);

                ui.horizontal(|ui| {
                    let text = if is_exc {
                        egui::RichText::new(safe.as_str())
                            .strikethrough()
                            .color(theme.text_dim)
                    } else if is_inc {
                        egui::RichText::new(safe.as_str())
                            .strong()
                            .color(theme.accent)
                    } else {
                        egui::RichText::new(safe.as_str())
                    };
                    if ui
                        .add(egui::Button::new(text).frame(false).selected(is_inc))
                        .on_hover_text(safe.as_str())
                        .clicked()
                    {
                        on_pick(raw, mods.ctrl || mods.shift, mods.alt);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("!=")
                            .on_hover_text("exclude (Alt+click)")
                            .clicked()
                        {
                            on_pick(raw, false, true);
                        }
                        ui.label(egui::RichText::new(thousands(item.count)).small());
                        draw_share_bar(ui, item.count, max_count, is_exc, theme);
                    });
                });
            }

            if shown == 0 {
                ui.label(egui::RichText::new("(no matching values)").weak().small());
            }
            if page.distinct_values as usize > page.values.len() {
                let remaining = page.distinct_values as usize - page.values.len();
                ui.label(
                    egui::RichText::new(format!("… {} more", thousands(remaining as u64)))
                        .weak()
                        .small(),
                );
            }
        });
    *is_open = resp.fully_open();
}

fn show_confidence_facet(
    ui: &mut egui::Ui,
    is_open: &mut bool,
    page: Option<&FacetPage>,
    active: &[fp_core::Confidence],
    theme: &Theme,
    actions: &mut Vec<Action>,
) {
    let header = facet_header_text("CONFIDENCE", page);
    let resp = egui::CollapsingHeader::new(header)
        .default_open(*is_open)
        .show(ui, |ui| {
            let Some(page) = page else {
                ui.label(egui::RichText::new("loading…").weak().small());
                return;
            };
            let max_count = page
                .values
                .iter()
                .map(|v| v.count)
                .max()
                .unwrap_or(0)
                .max(1);
            let multi = ui.input(|i| i.modifiers.ctrl || i.modifiers.shift);
            for item in &page.values {
                let Some(raw) = item.key.as_deref() else {
                    continue;
                };
                let Some(conf) = parse_confidence(raw) else {
                    continue;
                };
                let is_inc = active.contains(&conf);
                ui.horizontal(|ui| {
                    let label = if is_inc {
                        egui::RichText::new(raw).strong().color(theme.accent)
                    } else {
                        egui::RichText::new(raw)
                    };
                    if ui
                        .add(egui::Button::new(label).frame(false).selected(is_inc))
                        .clicked()
                    {
                        let mut next = if multi { active.to_vec() } else { Vec::new() };
                        if let Some(pos) = next.iter().position(|c| *c == conf) {
                            next.remove(pos);
                        } else if !multi && active == [conf] {
                            next.clear();
                        } else {
                            next.push(conf);
                        }
                        actions.push(Action::Scope(ScopeAction::SetConfidence(next)));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(thousands(item.count)).small());
                        draw_share_bar(ui, item.count, max_count, false, theme);
                    });
                });
            }
        });
    *is_open = resp.fully_open();
}

fn show_timing_facet(
    ui: &mut egui::Ui,
    is_open: &mut bool,
    model: &Model,
    theme: &Theme,
    filter: &fp_query::TimelineFilter,
    actions: &mut Vec<Action>,
) {
    let counts = model.timing.value().copied();
    let header = match counts {
        Some(c) => format!(
            "TIMING (timed {} · untimed {})",
            thousands(c.timed),
            thousands(c.untimed)
        ),
        None => "TIMING".to_string(),
    };
    let resp = egui::CollapsingHeader::new(header)
        .default_open(*is_open)
        .show(ui, |ui| {
            let Some(c) = counts else {
                ui.label(egui::RichText::new("loading…").weak().small());
                return;
            };
            let max_c = c.timed.max(c.untimed).max(1);
            for (label, target, count, tip) in [
                (
                    "timed",
                    EventTiming::Timed,
                    c.timed,
                    "Events with a parsed or derived UTC timestamp.",
                ),
                (
                    "untimed",
                    EventTiming::Untimed,
                    c.untimed,
                    "Order-only events have no timestamp (for example bash history without HISTTIMEFORMAT). Active time windows hide them until you clear the window or pick untimed.",
                ),
            ] {
                let selected = filter.timing == target;
                ui.horizontal(|ui| {
                    let text = if selected {
                        egui::RichText::new(label).strong().color(theme.accent)
                    } else {
                        egui::RichText::new(label)
                    };
                    if ui
                        .add(egui::Button::new(text).frame(false).selected(selected))
                        .on_hover_text(tip)
                        .clicked()
                    {
                        let next = if selected { EventTiming::Any } else { target };
                        actions.push(Action::Scope(ScopeAction::SetTiming(next)));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(thousands(count)).small());
                        draw_share_bar(ui, count, max_c, false, theme);
                    });
                });
            }
        });
    *is_open = resp.fully_open();
}

fn show_run_facet(
    ui: &mut egui::Ui,
    is_open: &mut bool,
    page: Option<&FacetPage>,
    active_run: Option<u64>,
    theme: &Theme,
    actions: &mut Vec<Action>,
) {
    let header = facet_header_text("RUN", page);
    let resp = egui::CollapsingHeader::new(header)
        .default_open(*is_open)
        .show(ui, |ui| {
            let Some(page) = page else {
                ui.label(egui::RichText::new("loading…").weak().small());
                return;
            };
            let max_count = page
                .values
                .iter()
                .map(|v| v.count)
                .max()
                .unwrap_or(0)
                .max(1);
            for item in &page.values {
                let Some(raw) = item.key.as_deref() else {
                    continue;
                };
                let Ok(id) = raw.parse::<u64>() else {
                    continue;
                };
                let selected = active_run == Some(id);
                ui.horizontal(|ui| {
                    let label = format!("run {id}");
                    let text = if selected {
                        egui::RichText::new(label).strong().color(theme.accent)
                    } else {
                        egui::RichText::new(label)
                    };
                    if ui
                        .add(egui::Button::new(text).frame(false).selected(selected))
                        .clicked()
                    {
                        let next = if selected { None } else { Some(id) };
                        actions.push(Action::Scope(ScopeAction::SetRun(next)));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(thousands(item.count)).small());
                        draw_share_bar(ui, item.count, max_count, false, theme);
                    });
                });
            }
        });
    *is_open = resp.fully_open();
}

fn facet_header_text(title: &str, page: Option<&FacetPage>) -> String {
    match page {
        Some(p) => format!("{title} ({})", thousands(p.distinct_values)),
        None => title.to_string(),
    }
}

/// Allocated left of the count label in a right-to-left row, so the bar never
/// overlaps the number it illustrates.
fn draw_share_bar(ui: &mut egui::Ui, count: u64, max: u64, excluded: bool, theme: &Theme) {
    let (track, _) = ui.allocate_exact_size(
        egui::vec2(SHARE_BAR_WIDTH, SHARE_BAR_HEIGHT),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(track, 2.0, theme.bg2);
    if count == 0 || max == 0 {
        return;
    }
    let width = (count as f32 / max as f32).clamp(0.06, 1.0) * track.width();
    let fill = egui::Rect::from_min_size(
        egui::pos2(track.right() - width, track.top()),
        egui::vec2(width, track.height()),
    );
    let color = if excluded {
        theme.text_dim.linear_multiply(0.5)
    } else {
        theme.accent.linear_multiply(0.7)
    };
    ui.painter().rect_filled(fill, 2.0, color);
}

fn parse_confidence(raw: &str) -> Option<fp_core::Confidence> {
    match raw {
        "direct" => Some(fp_core::Confidence::Direct),
        "derived" => Some(fp_core::Confidence::Derived),
        "heuristic" => Some(fp_core::Confidence::Heuristic),
        _ => None,
    }
}

pub fn toggle_string_lists(
    current_inc: &[String],
    current_exc: &[String],
    value: &str,
    multi: bool,
    exclude: bool,
) -> (Vec<String>, Vec<String>) {
    let mut inc = current_inc.to_vec();
    let mut exc = current_exc.to_vec();
    if exclude {
        inc.retain(|v| v != value);
        if let Some(pos) = exc.iter().position(|v| v == value) {
            exc.remove(pos);
        } else {
            exc.push(value.to_string());
        }
        return (inc, exc);
    }
    exc.retain(|v| v != value);
    if multi {
        if let Some(pos) = inc.iter().position(|v| v == value) {
            inc.remove(pos);
        } else {
            inc.push(value.to_string());
        }
    } else if inc == [value] {
        inc.clear();
    } else {
        inc = vec![value.to_string()];
    }
    (inc, exc)
}

const KNOWN_CATEGORY_PREFIXES: &[&str] = &[
    "web.*",
    "dl.*",
    "pkg.*",
    "priv.*",
    "auth.*",
    "shell.*",
    "file.*",
    "persist.*",
    "ext.*",
];

fn patterns_for(cat: Category) -> &'static [&'static str] {
    match cat {
        Category::Web => &["web.*"],
        Category::Downloads => &["web.download", "dl.*"],
        Category::Packages => &["pkg.*"],
        Category::Access => &["priv.*", "auth.*"],
        Category::Shell => &["shell.*"],
        Category::Files => &["file.*"],
        Category::Persistence => &["persist.*"],
        Category::Extensions => &["ext.*"],
        Category::Other => &[],
    }
}

pub fn category_included(filter: &fp_query::TimelineFilter, cat: Category) -> bool {
    if cat == Category::Other {
        return filter.kinds.is_empty()
            && KNOWN_CATEGORY_PREFIXES
                .iter()
                .all(|p| filter.kinds_not.iter().any(|k| k == p));
    }
    if cat == Category::Web {
        let has_web = filter.kinds.iter().any(|k| k == "web.*");
        let excludes_dl = filter.kinds_not.iter().any(|k| k == "web.download");
        return has_web && (excludes_dl || filter.kinds.iter().any(|k| k == "dl.*"));
    }
    patterns_for(cat)
        .iter()
        .all(|p| filter.kinds.iter().any(|k| k == p))
}

pub fn category_excluded(filter: &fp_query::TimelineFilter, cat: Category) -> bool {
    if cat == Category::Other {
        return false;
    }
    patterns_for(cat)
        .iter()
        .all(|p| filter.kinds_not.iter().any(|k| k == p))
}

pub fn apply_category_click(
    filter: &fp_query::TimelineFilter,
    cat: Category,
    multi: bool,
    exclude: bool,
) -> (Vec<String>, Vec<String>) {
    if cat == Category::Other {
        if exclude {
            return (filter.kinds.clone(), filter.kinds_not.clone());
        }
        let already = category_included(filter, Category::Other);
        if already {
            return (Vec::new(), Vec::new());
        }
        return (
            Vec::new(),
            KNOWN_CATEGORY_PREFIXES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        );
    }

    if exclude {
        let mut inc = filter.kinds.clone();
        let mut exc = filter.kinds_not.clone();
        let pats = patterns_for(cat);
        if category_excluded(filter, cat) {
            exc.retain(|k| !pats.contains(&k.as_str()));
        } else {
            inc.retain(|k| !pats.contains(&k.as_str()));
            for &p in pats {
                if !exc.iter().any(|k| k == p) {
                    exc.push(p.to_string());
                }
            }
        }
        return (inc, exc);
    }

    let mut active: Vec<Category> = Category::ALL
        .iter()
        .copied()
        .filter(|&c| c != Category::Other && category_included(filter, c))
        .collect();

    if multi {
        if let Some(pos) = active.iter().position(|&c| c == cat) {
            active.remove(pos);
        } else {
            active.push(cat);
        }
    } else if active == [cat] {
        active.clear();
    } else {
        active = vec![cat];
    }

    let mut inc = Vec::new();
    let mut exc = Vec::new();
    let both_web_and_dl = active.contains(&Category::Web) && active.contains(&Category::Downloads);
    for &c in &active {
        for &p in patterns_for(c) {
            if both_web_and_dl && p == "web.download" {
                continue;
            }
            if !inc.iter().any(|k| k == p) {
                inc.push(p.to_string());
            }
        }
    }
    if active.contains(&Category::Web) && !active.contains(&Category::Downloads) {
        exc.push("web.download".to_string());
    }
    (inc, exc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_category_excludes_web_download_unless_downloads_also_selected() {
        let empty = fp_query::TimelineFilter::default();
        let (inc, exc) = apply_category_click(&empty, Category::Web, false, false);
        assert_eq!(inc, vec!["web.*"]);
        assert_eq!(exc, vec!["web.download"]);

        let web_only = fp_query::TimelineFilter {
            kinds: inc,
            kinds_not: exc,
            ..empty.clone()
        };
        assert!(category_included(&web_only, Category::Web));
        assert!(!category_included(&web_only, Category::Downloads));

        let (inc2, exc2) = apply_category_click(&web_only, Category::Downloads, true, false);
        assert_eq!(inc2, vec!["web.*", "dl.*"]);
        assert!(exc2.is_empty());
    }

    #[test]
    fn other_category_cannot_be_excluded_as_a_group() {
        let filter = fp_query::TimelineFilter {
            kinds: vec!["web.*".into()],
            kinds_not: vec!["web.download".into()],
            ..Default::default()
        };

        assert_eq!(
            apply_category_click(&filter, Category::Other, false, true),
            (filter.kinds.clone(), filter.kinds_not.clone())
        );
    }

    #[test]
    fn toggle_string_lists_supports_only_multi_and_exclude() {
        let (inc, exc) = toggle_string_lists(&[], &[], "zoom.us", false, false);
        assert_eq!(inc, vec!["zoom.us"]);
        assert!(exc.is_empty());

        let (inc, exc) = toggle_string_lists(&inc, &exc, "google.com", true, false);
        assert_eq!(inc, vec!["zoom.us", "google.com"]);

        let (inc, exc) = toggle_string_lists(&inc, &exc, "zoom.us", false, true);
        assert_eq!(inc, vec!["google.com"]);
        assert_eq!(exc, vec!["zoom.us"]);
    }
}
