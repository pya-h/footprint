use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Id, InnerResponse, Margin, Response, RichText,
    Sense, Stroke, Ui, vec2,
};

use crate::theme::Theme;

const HOVER_FADE: f32 = 0.12;

pub fn card<R>(ui: &mut Ui, theme: &Theme, body: impl FnOnce(&mut Ui) -> R) -> InnerResponse<R> {
    card_frame(theme, 0.0).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        body(ui)
    })
}

pub fn card_frame(theme: &Theme, hover: f32) -> egui::Frame {
    egui::Frame::new()
        .fill(theme.bg1.lerp_to_gamma(theme.bg2, hover))
        .stroke(Stroke::new(
            1.0,
            theme.border.lerp_to_gamma(theme.accent, hover * 0.6),
        ))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 10))
}

pub fn toggle(ui: &mut Ui, on: bool, text: impl Into<egui::WidgetText>) -> Response {
    ui.add(egui::Button::new(text).selected(on))
}

pub fn primary_button(ui: &mut Ui, theme: &Theme, text: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text).strong().color(theme.bg0))
            .fill(theme.accent)
            .stroke(Stroke::NONE),
    )
}

pub fn section_label(ui: &mut Ui, theme: &Theme, text: &str) {
    ui.label(
        RichText::new(text.to_uppercase())
            .small()
            .strong()
            .color(theme.text_dim),
    );
}

pub fn empty_state(ui: &mut Ui, theme: &Theme, title: &str, hint: &str) {
    ui.add_space(28.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(title).size(16.0).strong());
        ui.add_space(2.0);
        ui.label(RichText::new(hint).color(theme.text_dim));
    });
}

// the response arrives after the surface has already been painted
pub fn hover_amount(ui: &Ui, id: Id, theme: &Theme) -> f32 {
    let was_hovered = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    if theme.reduced_motion {
        if was_hovered { 1.0 } else { 0.0 }
    } else {
        ui.ctx()
            .animate_bool_with_time(id.with("fade"), was_hovered, HOVER_FADE)
    }
}

pub fn remember_hover(ui: &Ui, id: Id, hovered: bool) {
    ui.data_mut(|d| d.insert_temp(id, hovered));
}

pub fn nav_item(ui: &mut Ui, theme: &Theme, number: &str, label: &str, selected: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
    let (hover, active) = if theme.reduced_motion {
        (
            if resp.hovered() { 1.0 } else { 0.0 },
            if selected { 1.0 } else { 0.0 },
        )
    } else {
        (
            ui.ctx()
                .animate_bool_with_time(resp.id.with("hover"), resp.hovered(), HOVER_FADE),
            ui.ctx()
                .animate_bool_with_time(resp.id.with("active"), selected, HOVER_FADE * 1.5),
        )
    };
    let painter = ui.painter();
    let row = rect.shrink2(vec2(4.0, 1.0));
    let fill = theme.bg0.lerp_to_gamma(theme.bg2, hover.max(active));
    painter.rect_filled(row, CornerRadius::same(6), fill);
    if active > 0.0 {
        let bar = egui::Rect::from_min_size(
            row.left_top() + vec2(0.0, 6.0),
            vec2(3.0, (row.height() - 12.0) * active),
        );
        painter.rect_filled(bar, CornerRadius::same(2), theme.accent);
    }
    let text_color = if selected {
        theme.text
    } else {
        theme.text_dim.lerp_to_gamma(theme.text, hover)
    };
    painter.text(
        row.left_center() + vec2(12.0, 0.0),
        Align2::LEFT_CENTER,
        number,
        FontId::monospace(11.5),
        if selected {
            theme.accent
        } else {
            theme.text_dim
        },
    );
    painter.text(
        row.left_center() + vec2(34.0, 0.0),
        Align2::LEFT_CENTER,
        label,
        FontId::proportional(13.0),
        text_color,
    );
    let name = format!("{number}  {label}");
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &name));
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

pub fn tint(base: Color32, theme: &Theme, amount: f32) -> Color32 {
    base.lerp_to_gamma(theme.accent, amount)
}

// preserve the identifying tail of paths and keys
pub fn middle_ellipsis(ui: &Ui, text: &str, font: &FontId, max_width: f32) -> String {
    let fits = |s: &str| {
        ui.painter()
            .layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE)
            .size()
            .x
            <= max_width
    };
    if fits(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let build = |kept: usize| {
        let head = kept * 2 / 5;
        let tail = kept - head;
        let mut out: String = chars[..head].iter().collect();
        out.push('…');
        out.extend(&chars[chars.len() - tail..]);
        out
    };
    let (mut lo, mut hi) = (1, chars.len().saturating_sub(1));
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if fits(&build(mid)) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    build(lo)
}
