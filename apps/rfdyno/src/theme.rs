//! Themes: palettes, fonts and widget style, the same as ChopShop's.
//!
//! - **Fresh** (default): light and crisp. White plates on a soft off-white, near-black text,
//!   green (`#92d050`) for actions.
//! - **Graphite**: the same on a neutral dark.
//! - **Teal**: deep teal ink.
//!
//! All are flat (plates, no borders) in Barlow Semi Condensed (OFL, `assets/fonts`).

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use egui::{
    Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Visuals,
};

/// The colours of one theme.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Background.
    pub ink: Color32,
    /// Plates (cards, plots, buttons) on the background.
    pub plate: Color32,
    /// Lines, pressed plates, grid.
    pub rule: Color32,
    /// Primary text.
    pub text: Color32,
    /// Secondary text and captions.
    pub muted: Color32,
    /// Hints and disabled text.
    pub faint: Color32,
    /// Green, as a fill.
    pub green: Color32,
    /// Text on a green fill.
    pub green_ink: Color32,
    /// Green as text or stroke on the background (plot lines).
    pub accent: Color32,
    /// Attention, as text.
    pub echo: Color32,
    /// Alarm, as text.
    pub alarm: Color32,
    /// Whether this is a dark palette (egui's base visuals).
    pub dark: bool,
}

/// Fresh: the default, light and crisp.
pub const FRESH: Palette = Palette {
    ink: Color32::from_rgb(0xf4, 0xf6, 0xf3),
    plate: Color32::from_rgb(0xff, 0xff, 0xff),
    rule: Color32::from_rgb(0xe2, 0xe7, 0xe1),
    text: Color32::from_rgb(0x1b, 0x22, 0x1f),
    muted: Color32::from_rgb(0x5b, 0x67, 0x61),
    faint: Color32::from_rgb(0x98, 0xa3, 0x9d),
    green: Color32::from_rgb(0x92, 0xd0, 0x50),
    green_ink: Color32::from_rgb(0x17, 0x30, 0x0f),
    accent: Color32::from_rgb(0x46, 0x8a, 0x1c),
    echo: Color32::from_rgb(0xb0, 0x6f, 0x00),
    alarm: Color32::from_rgb(0xd6, 0x45, 0x30),
    dark: false,
};

/// Graphite: neutral dark.
pub const GRAPHITE: Palette = Palette {
    ink: Color32::from_rgb(0x15, 0x17, 0x19),
    plate: Color32::from_rgb(0x20, 0x23, 0x26),
    rule: Color32::from_rgb(0x34, 0x38, 0x3c),
    text: Color32::from_rgb(0xf1, 0xf3, 0xf1),
    muted: Color32::from_rgb(0xa3, 0xab, 0xa7),
    faint: Color32::from_rgb(0x6e, 0x76, 0x72),
    green: Color32::from_rgb(0x92, 0xd0, 0x50),
    green_ink: Color32::from_rgb(0x17, 0x30, 0x0f),
    accent: Color32::from_rgb(0x9f, 0xd8, 0x62),
    echo: Color32::from_rgb(0xf5, 0xc4, 0x51),
    alarm: Color32::from_rgb(0xff, 0x7a, 0x66),
    dark: true,
};

/// Teal: deep teal ink.
pub const TEAL: Palette = Palette {
    ink: Color32::from_rgb(0x0d, 0x2a, 0x30),
    plate: Color32::from_rgb(0x14, 0x37, 0x40),
    rule: Color32::from_rgb(0x2c, 0x56, 0x60),
    text: Color32::from_rgb(0xee, 0xf4, 0xea),
    muted: Color32::from_rgb(0x93, 0xb3, 0xae),
    faint: Color32::from_rgb(0x64, 0x86, 0x82),
    green: Color32::from_rgb(0x92, 0xd0, 0x50),
    green_ink: Color32::from_rgb(0x17, 0x30, 0x0f),
    accent: Color32::from_rgb(0x92, 0xd0, 0x50),
    echo: Color32::from_rgb(0xff, 0xe7, 0xa3),
    alarm: Color32::from_rgb(0xff, 0x7a, 0x66),
    dark: true,
};

/// The themes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Theme {
    /// Light and crisp (default).
    Fresh,
    /// Neutral dark.
    Graphite,
    /// Deep teal.
    Teal,
}

impl Theme {
    /// Every theme, for the selector.
    pub const ALL: [Self; 3] = [Self::Fresh, Self::Graphite, Self::Teal];

    /// Name for the GUI and the settings.
    pub fn name(self) -> &'static str {
        match self {
            Self::Fresh => "Fresh",
            Self::Graphite => "Graphite",
            Self::Teal => "Teal",
        }
    }

    /// The theme with this name; Fresh for anything unknown.
    pub fn from_name(name: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|theme| theme.name().eq_ignore_ascii_case(name))
            .unwrap_or(Self::Fresh)
    }

    /// Its palette.
    pub fn palette(self) -> &'static Palette {
        match self {
            Self::Fresh => &FRESH,
            Self::Graphite => &GRAPHITE,
            Self::Teal => &TEAL,
        }
    }
}

/// Corner radius of plates.
pub const RADIUS: u8 = 6;

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// The current theme.
pub fn current() -> Theme {
    Theme::ALL[usize::from(CURRENT.load(Ordering::Relaxed)) % Theme::ALL.len()]
}

/// The current palette.
pub fn c() -> &'static Palette {
    current().palette()
}

/// Switches theme and restyles the widgets.
pub fn set_theme(ctx: &egui::Context, theme: Theme) {
    let index = Theme::ALL
        .iter()
        .position(|known| *known == theme)
        .unwrap_or(0);
    CURRENT.store(u8::try_from(index).unwrap_or(0), Ordering::Relaxed);
    ctx.set_visuals(visuals(theme.palette()));
}

/// The fill of a toggle that's on (and of selected items): a light tint of the accent.
fn on_fill(palette: &Palette) -> Color32 {
    let tint = if palette.dark { 0.22 } else { 0.14 };
    let accent = palette.accent;
    let mix = |a: u8, b: u8| (f32::from(a) * tint + f32::from(b) * (1.0 - tint)).round() as u8;
    Color32::from_rgb(
        mix(accent.r(), palette.plate.r()),
        mix(accent.g(), palette.plate.g()),
        mix(accent.b(), palette.plate.b()),
    )
}

/// A toggle button: a plain button when off; when on, outlined and lettered in the accent on a
/// light tint of it. Flips `on` when clicked.
pub fn toggle(ui: &mut egui::Ui, on: &mut bool, text: &str) -> egui::Response {
    let palette = c();
    let button = if *on {
        egui::Button::new(egui::RichText::new(text).color(palette.accent))
            .fill(on_fill(palette))
            .stroke(Stroke::new(1.5, palette.accent))
    } else {
        egui::Button::new(text)
    };
    let response = ui.add(button);
    if response.clicked() {
        *on = !*on;
    }
    response
}

/// A [`toggle`] with a short line in `color` before it: a switch for a plot trace.
pub fn trace_toggle(
    ui: &mut egui::Ui,
    on: &mut bool,
    text: &str,
    color: Color32,
) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
    let line = egui::Rect::from_center_size(rect.center(), egui::vec2(14.0, 3.0));
    let color = if *on {
        color
    } else {
        color.gamma_multiply(0.35)
    };
    ui.painter().rect_filled(line, 1.0, color);
    toggle(ui, on, text)
}

/// Barlow Semi Condensed Medium.
pub fn medium() -> FontFamily {
    FontFamily::Name("medium".into())
}

/// Barlow Semi Condensed SemiBold.
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// SemiBold weight at `size`.
pub fn semibold_font(size: f32) -> FontId {
    FontId::new(size, semibold())
}

/// Installs fonts, text styles and `theme`.
pub fn install(ctx: &egui::Context, theme: Theme) {
    ctx.set_fonts(fonts());
    set_theme(ctx, theme);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(14.0, 6.0);
        style.spacing.interact_size.y = 30.0;

        let styles = &mut style.text_styles;
        styles.insert(TextStyle::Body, FontId::proportional(16.0));
        styles.insert(TextStyle::Button, FontId::new(16.0, medium()));
        styles.insert(TextStyle::Small, FontId::proportional(13.0));
        styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
        styles.insert(TextStyle::Heading, FontId::new(24.0, semibold()));
    });
}

fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();

    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    };
    add(
        "barlow",
        include_bytes!("../assets/fonts/BarlowSemiCondensed-Regular.ttf"),
    );
    add(
        "barlow-medium",
        include_bytes!("../assets/fonts/BarlowSemiCondensed-Medium.ttf"),
    );
    add(
        "barlow-semibold",
        include_bytes!("../assets/fonts/BarlowSemiCondensed-SemiBold.ttf"),
    );

    // egui's own fonts stay behind ours for symbols Barlow lacks (µ is in Barlow, ⚠ is not).
    let fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let family = |primary: &str| {
        let mut list = vec![primary.to_owned()];
        list.extend(fallbacks.iter().cloned());
        list
    };
    fonts
        .families
        .insert(FontFamily::Proportional, family("barlow"));
    fonts.families.insert(medium(), family("barlow-medium"));
    fonts.families.insert(semibold(), family("barlow-semibold"));

    fonts
}

fn visuals(palette: &Palette) -> Visuals {
    let mut visuals = if palette.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };

    visuals.panel_fill = palette.ink;
    visuals.window_fill = palette.plate;
    visuals.extreme_bg_color = palette.plate;
    visuals.faint_bg_color = palette.plate;
    visuals.code_bg_color = palette.plate;
    visuals.hyperlink_color = palette.accent;
    visuals.override_text_color = Some(palette.text);
    visuals.weak_text_color = Some(palette.muted);
    visuals.window_stroke = Stroke::NONE;
    visuals.window_corner_radius = CornerRadius::same(RADIUS);
    visuals.window_shadow = egui::Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(90),
    };
    visuals.popup_shadow = egui::Shadow {
        offset: [0, 4],
        blur: 14,
        spread: 0,
        color: Color32::from_black_alpha(60),
    };
    // Selected items (menus, text): the accent tint with normal text, not text on solid green.
    visuals.selection.bg_fill = on_fill(palette);
    visuals.selection.stroke = Stroke::new(1.0, palette.text);

    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = CornerRadius::same(RADIUS);
        // An unseen outline as wide as a toggle's, so every control is the same height.
        widget.bg_stroke = Stroke::new(1.5, Color32::TRANSPARENT);
        widget.expansion = 0.0;
    }

    visuals.widgets.noninteractive.bg_fill = palette.plate;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, palette.muted);
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, palette.rule);

    visuals.widgets.inactive.bg_fill = palette.plate;
    visuals.widgets.inactive.weak_bg_fill = palette.plate;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, palette.text);

    visuals.widgets.hovered.bg_fill = palette.rule;
    visuals.widgets.hovered.weak_bg_fill = palette.rule;
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, palette.text);

    visuals.widgets.active.bg_fill = palette.green;
    visuals.widgets.active.weak_bg_fill = palette.green;
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, palette.green_ink);

    visuals.widgets.open.bg_fill = palette.rule;
    visuals.widgets.open.weak_bg_fill = palette.rule;

    visuals
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for theme in Theme::ALL {
            assert_eq!(Theme::from_name(theme.name()), theme);
        }
        assert_eq!(Theme::from_name("graphite"), Theme::Graphite);
        assert_eq!(Theme::from_name("nonsense"), Theme::Fresh);
    }
}
