//! Palettes, fonts, text styles and `egui::Visuals` for the GUI.
//!
//! Colours are sampled from the design mock-ups (light and dark). Widgets read
//! them through [`Palette::of`] so they follow the active egui theme.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Theme, Visuals,
};

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub page: Color32,
    pub card: Color32,
    pub input: Color32,
    pub divider: Color32,
    pub text: Color32,
    pub weak: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub button: Color32,
    pub button_hover: Color32,
    pub check: Color32,
    pub movies_fg: Color32,
    pub movies_bg: Color32,
    pub tv_fg: Color32,
    pub tv_bg: Color32,
    pub ok: Color32,
    pub dark: bool,
}

impl Palette {
    pub const LIGHT: Palette = Palette {
        page: rgb(0xF6F7F8),
        card: rgb(0xFFFFFF),
        input: rgb(0xF4F5F6),
        divider: rgb(0xE4E7EA),
        text: rgb(0x1C2329),
        weak: rgb(0x6A737D),
        accent: rgb(0x0B5F86),
        on_accent: rgb(0xFFFFFF),
        button: rgb(0xF0F2F4),
        button_hover: rgb(0xE6E9EC),
        check: rgb(0x0B63E5),
        movies_fg: rgb(0x0B6A8F),
        movies_bg: rgb(0xE1F0F5),
        tv_fg: rgb(0x5B6570),
        tv_bg: rgb(0xE9ECEF),
        ok: rgb(0x1E7B34),
        dark: false,
    };

    pub const DARK: Palette = Palette {
        page: rgb(0x0D1115),
        card: rgb(0x1A1F24),
        input: rgb(0x0C1115),
        divider: rgb(0x262C33),
        text: rgb(0xE8ECEF),
        weak: rgb(0x8E98A3),
        accent: rgb(0x7CC4FF),
        on_accent: rgb(0x08273F),
        button: rgb(0x232A31),
        button_hover: rgb(0x2C353D),
        check: rgb(0x1F6FEB),
        movies_fg: rgb(0x7CC4FF),
        movies_bg: rgb(0x17324A),
        tv_fg: rgb(0xBBA9FF),
        tv_bg: rgb(0x2A2650),
        ok: rgb(0x7EE29A),
        dark: true,
    };

    pub fn of(visuals: &Visuals) -> Palette {
        if visuals.dark_mode {
            Palette::DARK
        } else {
            Palette::LIGHT
        }
    }
}

/// Bold Manrope. egui's `RichText::strong` only recolours text, so bold needs
/// its own font family.
pub fn bold_family() -> FontFamily {
    FontFamily::Name("Manrope-Bold".into())
}

pub fn bold_font(size: f32) -> FontId {
    FontId::new(size, bold_family())
}

/// Embeds Manrope and installs the palettes, text styles and spacing.
pub fn install(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "Manrope".to_owned(),
        FontData::from_static(include_bytes!("../../assets/fonts/Manrope-Regular.ttf")).into(),
    );
    fonts.font_data.insert(
        "Manrope-Bold".to_owned(),
        FontData::from_static(include_bytes!("../../assets/fonts/Manrope-Bold.ttf")).into(),
    );

    let proportional = fonts.families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "Manrope".to_owned());
    // Bold falls back to the default fonts for glyphs Manrope lacks.
    let mut bold = vec!["Manrope-Bold".to_owned()];
    bold.extend(proportional.iter().filter(|n| *n != "Manrope").cloned());
    fonts.families.insert(bold_family(), bold);
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, bold_font(18.0)),
            (TextStyle::Body, FontId::proportional(14.0)),
            (TextStyle::Button, FontId::proportional(14.0)),
            (TextStyle::Small, FontId::proportional(12.0)),
            (TextStyle::Monospace, FontId::monospace(13.0)),
        ]
        .into();
        style.spacing.item_spacing = egui::vec2(10.0, 8.0);
        style.spacing.button_padding = egui::vec2(14.0, 7.0);
    });

    ctx.set_visuals_of(Theme::Light, visuals(&Palette::LIGHT));
    ctx.set_visuals_of(Theme::Dark, visuals(&Palette::DARK));
}

fn visuals(p: &Palette) -> Visuals {
    let mut v = if p.dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    v.panel_fill = p.page;
    v.window_fill = p.card;
    v.extreme_bg_color = p.input;
    v.faint_bg_color = p.card;
    v.code_bg_color = p.input;
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent.gamma_multiply(0.35);
    v.selection.stroke = Stroke::new(1.0, p.accent);

    let radius = CornerRadius::same(6);
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.divider);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    for (widget, fill) in [
        (&mut v.widgets.inactive, p.button),
        (&mut v.widgets.hovered, p.button_hover),
        (&mut v.widgets.active, p.button_hover),
        (&mut v.widgets.open, p.button_hover),
    ] {
        widget.bg_fill = fill;
        widget.weak_bg_fill = fill;
        widget.bg_stroke = Stroke::NONE;
        widget.fg_stroke = Stroke::new(1.0, p.text);
    }
    for widget in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        widget.corner_radius = radius;
    }
    v
}
