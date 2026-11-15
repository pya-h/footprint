//! Palettes, category colors and the visual tokens shared by every widget.
//! Colors are data so tests can assert accessibility invariants.

use eframe::egui::Color32;
use fp_query::category::Category;

/// Text/background pairs must meet WCAG AA (4.5:1) on every panel background.
pub const MIN_CONTRAST: f64 = 4.5;
/// CIE76 distance above which two category colors read as clearly different.
pub const MIN_CATEGORY_DELTA_E: f64 = 20.0;

pub const CATEGORY_LETTERS: [char; 9] = ['W', 'D', 'P', 'A', 'S', 'F', 'R', 'E', 'O'];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeName {
    #[default]
    Dark,
    Light,
    HighContrast,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: ThemeName,
    pub bg0: Color32,
    pub bg1: Color32,
    pub bg2: Color32,
    pub border: Color32,
    pub text: Color32,
    pub text_dim: Color32,
    pub accent: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub danger: Color32,
    pub ioc: Color32,
    /// [`Category::ALL`] order, so `categories[category as usize]` is the swatch.
    pub categories: [Color32; 9],
    /// table row height per density, in points
    pub row_compact: f32,
    pub row_comfortable: f32,
    pub base_font_size: f32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            name: ThemeName::Dark,
            bg0: rgb(0x0F1419),
            bg1: rgb(0x151B23),
            bg2: rgb(0x1C242E),
            border: rgb(0x2A3441),
            text: rgb(0xE6EAF0),
            text_dim: rgb(0x93A1B3),
            accent: rgb(0x4EA1FF),
            ok: rgb(0x3FB950),
            warn: rgb(0xE3B341),
            danger: rgb(0xF85149),
            ioc: rgb(0xFF5C5C),
            categories: [
                rgb(0x3C71DD), // Web
                rgb(0x3CDDCF), // Downloads
                rgb(0xE68C33), // Packages
                rgb(0x8C3EDA), // Access
                rgb(0x4DCB77), // Shell
                rgb(0x7390A5), // Files
                rgb(0xDD3C9A), // Persistence
                rgb(0xDFD13A), // Extensions
                rgb(0xC0C7CE), // Other
            ],
            row_compact: 20.0,
            row_comfortable: 26.0,
            base_font_size: 13.0,
        }
    }

    pub fn light() -> Self {
        Self {
            name: ThemeName::Light,
            bg0: rgb(0xF6F8FA),
            bg1: rgb(0xEEF1F4),
            bg2: rgb(0xE4E9EE),
            border: rgb(0xC9D1D9),
            text: rgb(0x1C2128),
            text_dim: rgb(0x57606A),
            accent: rgb(0x0550AE),
            ok: rgb(0x14612A),
            warn: rgb(0x7A5200),
            danger: rgb(0xB5232B),
            ioc: rgb(0xA40E26),
            categories: [
                rgb(0x153779),
                rgb(0x157971),
                rgb(0x7F4710),
                rgb(0x471778),
                rgb(0x206F3A),
                rgb(0x5D7C92),
                rgb(0x791550),
                rgb(0x7B7214),
                rgb(0x3F4750),
            ],
            row_compact: 20.0,
            row_comfortable: 26.0,
            base_font_size: 13.0,
        }
    }

    pub fn high_contrast() -> Self {
        Self {
            name: ThemeName::HighContrast,
            bg0: rgb(0x000000),
            bg1: rgb(0x0A0A0A),
            bg2: rgb(0x141414),
            border: rgb(0x6E6E6E),
            text: rgb(0xFFFFFF),
            text_dim: rgb(0xC7C7CC),
            accent: rgb(0xFFD60A),
            ok: rgb(0x32D74B),
            warn: rgb(0xFF9F0A),
            danger: rgb(0xFF453A),
            ioc: rgb(0xFF5C5C),
            categories: [
                rgb(0x3C71DD),
                rgb(0x3CDDCF),
                rgb(0xE68C33),
                rgb(0x8C3EDA),
                rgb(0x4DCB77),
                rgb(0x7390A5),
                rgb(0xDD3C9A),
                rgb(0xDFD13A),
                rgb(0xD4D9DD),
            ],
            row_compact: 22.0,
            row_comfortable: 28.0,
            base_font_size: 14.0,
        }
    }

    pub fn all() -> [Self; 3] {
        [Self::dark(), Self::light(), Self::high_contrast()]
    }

    /// A category's swatch; neighbors stay distinguishable (asserted as ΔE in tests).
    pub fn category_color(&self, category: Category) -> Color32 {
        self.categories[category as usize]
    }

    /// The letter paired with the color, so color is never the only channel.
    pub fn category_letter(category: Category) -> char {
        CATEGORY_LETTERS[category as usize]
    }

    /// Installs the theme's fonts, spacing and widget skin into the live egui context.
    pub fn apply(&self, ctx: &eframe::egui::Context) {
        use eframe::egui::{FontId, TextStyle, Theme as EguiTheme, ThemePreference, Vec2};

        ctx.set_theme(ThemePreference::from(match self.name {
            ThemeName::Light => EguiTheme::Light,
            ThemeName::Dark | ThemeName::HighContrast => EguiTheme::Dark,
        }));
        let base = self.base_font_size;
        ctx.all_styles_mut(|style| {
            for text_style in [TextStyle::Body, TextStyle::Button, TextStyle::Heading] {
                style
                    .text_styles
                    .insert(text_style, FontId::proportional(base));
            }
            style
                .text_styles
                .insert(TextStyle::Monospace, FontId::monospace(base - 0.5));
            style.spacing.item_spacing = Vec2::new(8.0, 4.0);
        });
        ctx.set_visuals_of(
            match self.name {
                ThemeName::Light => EguiTheme::Light,
                ThemeName::Dark | ThemeName::HighContrast => EguiTheme::Dark,
            },
            self.visuals(),
        );
    }

    /// egui's widget skin mapped onto this palette (accent fills get bg0 text so the
    /// contrast invariants hold everywhere, not just in our own widgets).
    fn visuals(&self) -> eframe::egui::Visuals {
        use eframe::egui::style::{WidgetVisuals, Widgets};
        use eframe::egui::{CornerRadius, Stroke};

        let widget = |bg: eframe::egui::Color32, fg: eframe::egui::Color32| WidgetVisuals {
            bg_fill: bg,
            weak_bg_fill: bg,
            bg_stroke: Stroke::new(1.0, self.border),
            fg_stroke: Stroke::new(1.0, fg),
            corner_radius: CornerRadius::same(4),
            expansion: 0.0,
        };
        let mut visuals = match self.name {
            ThemeName::Light => eframe::egui::Visuals::light(),
            ThemeName::Dark | ThemeName::HighContrast => eframe::egui::Visuals::dark(),
        };
        visuals.panel_fill = self.bg0;
        visuals.window_fill = self.bg1;
        visuals.extreme_bg_color = self.bg0;
        visuals.faint_bg_color = self.bg2;
        visuals.window_stroke = Stroke::new(1.0, self.border);
        visuals.widgets = Widgets {
            noninteractive: widget(self.bg1, self.text),
            inactive: widget(self.bg2, self.text),
            hovered: widget(self.bg2, self.text),
            active: widget(self.accent, self.bg0),
            open: widget(self.bg2, self.text),
        };
        visuals.selection.bg_fill = self.accent;
        visuals.selection.stroke = Stroke::new(2.0, self.accent);
        visuals.hyperlink_color = self.accent;
        visuals
    }
}

fn relative_luminance(c: Color32) -> f64 {
    fn channel(v: u8) -> f64 {
        let c = f64::from(v) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
}

/// WCAG contrast ratio between two colors (order does not matter).
pub fn contrast(a: Color32, b: Color32) -> f64 {
    let (mut hi, mut lo) = (relative_luminance(a), relative_luminance(b));
    if hi < lo {
        std::mem::swap(&mut hi, &mut lo);
    }
    (hi + 0.05) / (lo + 0.05)
}

fn lab(c: Color32) -> [f64; 3] {
    fn linear(v: u8) -> f64 {
        let c = f64::from(v) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    let (r, g, b) = (linear(c.r()), linear(c.g()), linear(c.b()));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f64| {
        if t > 0.008856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (x, y, z) = (f(x), f(y), f(z));
    [116.0 * y - 16.0, 500.0 * (x - y), 200.0 * (y - z)]
}

/// CIE76 color distance in Lab space.
pub fn delta_e(a: Color32, b: Color32) -> f64 {
    let (la, lb) = (lab(a), lab(b));
    la.iter()
        .zip(lb)
        .map(|(x, y)| (x - y).powi(2))
        .sum::<f64>()
        .sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_query::category::Category;

    fn foregrounds(theme: &Theme) -> [(&str, Color32); 7] {
        [
            ("text", theme.text),
            ("text_dim", theme.text_dim),
            ("accent", theme.accent),
            ("ok", theme.ok),
            ("warn", theme.warn),
            ("danger", theme.danger),
            ("ioc", theme.ioc),
        ]
    }

    #[test]
    fn text_meets_wcag_aa_on_every_background() {
        for theme in Theme::all() {
            let bgs = [("bg0", theme.bg0), ("bg1", theme.bg1), ("bg2", theme.bg2)];
            for (fg_name, fg) in foregrounds(&theme) {
                for (bg_name, bg) in bgs {
                    let ratio = contrast(fg, bg);
                    assert!(
                        ratio >= MIN_CONTRAST,
                        "{:?} {fg_name} on {bg_name}: {ratio:.2} < {MIN_CONTRAST}",
                        theme.name
                    );
                }
            }
        }
    }

    #[test]
    fn category_colors_are_pairwise_distinct() {
        for theme in Theme::all() {
            for (i, a) in theme.categories.iter().enumerate() {
                for b in theme.categories.iter().skip(i + 1) {
                    let distance = delta_e(*a, *b);
                    assert!(
                        distance >= MIN_CATEGORY_DELTA_E,
                        "{:?}: categories {i} and a later one: ΔE {distance:.1} < {MIN_CATEGORY_DELTA_E}",
                        theme.name
                    );
                }
            }
        }
    }

    #[test]
    fn every_category_has_a_distinct_letter() {
        let mut letters: Vec<char> = Category::ALL
            .iter()
            .map(|c| Theme::category_letter(*c))
            .collect();
        let len = letters.len();
        letters.sort_unstable();
        letters.dedup();
        assert_eq!(letters.len(), len, "category letters must be unique");
    }

    #[test]
    fn theme_arrays_cover_every_category() {
        assert_eq!(CATEGORY_LETTERS.len(), Category::ALL.len());
        let theme = Theme::dark();
        for category in Category::ALL {
            let _ = theme.category_color(category);
            let _ = Theme::category_letter(category);
        }
    }
}
