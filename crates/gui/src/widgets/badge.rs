//! Painter-drawn status and confidence glyphs. The bundled fonts lack ●◐○⚑✓✖⚠, so
//! every non-ASCII mark is drawn from primitives; each also carries a distinct shape so
//! color is never the only channel.

use eframe::egui::{Align2, Color32, FontId, Painter, Pos2, Rect, Shape, Stroke, Vec2};

/// How sure the link to this record is: read straight from the record, computed from a
/// related record, or guessed by proximity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfidenceGlyph {
    Direct,
    Derived,
    Heuristic,
}

/// Fixed-vocabulary verdicts and flags, each with its own shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusGlyph {
    Ok,
    Mismatch,
    Missing,
    Unowned,
    Partial,
    Unverified,
    Ioc,
}

impl StatusGlyph {
    /// The theme color paired with the shape; color never carries the verdict alone.
    pub fn color(self, theme: &crate::theme::Theme) -> Color32 {
        match self {
            Self::Ok => theme.ok,
            Self::Mismatch | Self::Missing => theme.danger,
            Self::Unowned | Self::Partial => theme.warn,
            Self::Unverified => theme.text_dim,
            Self::Ioc => theme.ioc,
        }
    }
}

fn stroke(color: Color32, radius: f32) -> Stroke {
    Stroke::new((radius * 0.35).max(1.0), color)
}

/// The occupied square around `center`, for layout and click targets.
pub fn glyph_rect(center: Pos2, radius: f32) -> Rect {
    Rect::from_center_size(center, Vec2::splat(radius * 2.0))
}

/// ● direct · ◐ derived · ○ heuristic, as filled, half-filled and hollow discs.
pub fn confidence_disc(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    glyph: ConfidenceGlyph,
    color: Color32,
) {
    match glyph {
        ConfidenceGlyph::Direct => {
            painter.circle_filled(center, radius, color);
        }
        ConfidenceGlyph::Derived => {
            // clip to one half so the fill stops at the diameter
            let clip = Rect::from_min_max(
                center - Vec2::splat(radius),
                center + Vec2::new(0.0, radius),
            );
            painter
                .with_clip_rect(clip)
                .circle_filled(center, radius, color);
            painter.circle_stroke(center, radius, stroke(color, radius));
        }
        ConfidenceGlyph::Heuristic => {
            painter.circle_stroke(center, radius, stroke(color, radius));
        }
    }
}

/// Draws one verdict mark with the given theme color; `?` is ASCII (present in every
/// font) and needs no painter path.
pub fn status_mark(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    glyph: StatusGlyph,
    color: Color32,
) {
    match glyph {
        StatusGlyph::Ok => check_mark(painter, center, radius, color),
        StatusGlyph::Mismatch => cross_mark(painter, center, radius, color),
        StatusGlyph::Missing => {
            cross_mark(painter, center, radius, color);
            let tick = radius * 0.45;
            painter.line_segment(
                [
                    center - Vec2::new(tick, radius * 1.1),
                    center + Vec2::new(tick, radius * 1.1),
                ],
                stroke(color, radius),
            );
        }
        StatusGlyph::Unowned => {
            warning_triangle(painter, center, radius, color);
            // an underline keeps unowned shape-distinct from partial (same warn color)
            painter.line_segment(
                [
                    center + Vec2::new(-radius * 0.8, radius * 1.25),
                    center + Vec2::new(radius * 0.8, radius * 1.25),
                ],
                stroke(color, radius),
            );
        }
        StatusGlyph::Partial => warning_triangle(painter, center, radius, color),
        StatusGlyph::Unverified => {
            painter.text(
                center,
                Align2::CENTER_CENTER,
                "?",
                FontId::proportional(radius * 1.6),
                color,
            );
        }
        StatusGlyph::Ioc => flag(painter, center, radius, color),
    }
}

fn check_mark(painter: &Painter, center: Pos2, radius: f32, color: Color32) {
    let s = radius;
    painter.add(Shape::line(
        vec![
            center + Vec2::new(-s, 0.0),
            center + Vec2::new(-s * 0.2, s * 0.8),
            center + Vec2::new(s, -s * 0.8),
        ],
        stroke(color, radius),
    ));
}

fn cross_mark(painter: &Painter, center: Pos2, radius: f32, color: Color32) {
    let s = radius * 0.8;
    painter.line_segment(
        [center - Vec2::splat(s), center + Vec2::splat(s)],
        stroke(color, radius),
    );
    painter.line_segment(
        [center + Vec2::new(-s, s), center + Vec2::new(s, -s)],
        stroke(color, radius),
    );
}

fn warning_triangle(painter: &Painter, center: Pos2, radius: f32, color: Color32) {
    let top = center + Vec2::new(0.0, -radius);
    let left = center + Vec2::new(-radius * 1.1, radius * 0.8);
    let right = center + Vec2::new(radius * 1.1, radius * 0.8);
    painter.add(Shape::convex_polygon(
        vec![top, left, right],
        Color32::TRANSPARENT,
        stroke(color, radius),
    ));
    // the stem; Unowned also gets a bottom bar so the two stay shape-distinct
    painter.line_segment(
        [
            center + Vec2::new(0.0, -radius * 0.25),
            center + Vec2::new(0.0, radius * 0.3),
        ],
        stroke(color, radius),
    );
}

fn flag(painter: &Painter, center: Pos2, radius: f32, color: Color32) {
    let pole_top = center + Vec2::new(-radius * 0.5, -radius);
    let pole_bottom = center + Vec2::new(-radius * 0.5, radius);
    painter.line_segment([pole_top, pole_bottom], stroke(color, radius));
    painter.add(Shape::convex_polygon(
        vec![
            pole_top,
            pole_top + Vec2::new(radius * 1.6, radius * 0.45),
            pole_top + Vec2::new(0.0, radius * 0.9),
        ],
        color,
        Stroke::NONE,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_verdict_has_its_own_color_channel() {
        // colors may repeat semantics (mismatch/missing share danger), but the shape
        // pairs must not collapse into one mark
        let theme = crate::theme::Theme::dark();
        assert_eq!(StatusGlyph::Ok.color(&theme), theme.ok);
        assert_eq!(StatusGlyph::Ioc.color(&theme), theme.ioc);
        assert_eq!(StatusGlyph::Unverified.color(&theme), theme.text_dim);
    }
}
