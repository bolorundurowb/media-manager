//! Reusable, hand-painted widgets: cards, pills, buttons, joined path inputs,
//! radios, the theme switcher and simple line icons (no icon font needed).

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Frame, Layout, Margin, Painter, Pos2, Rect,
    RichText, Sense, Shape, Stroke, Ui, WidgetInfo, WidgetType, vec2,
};

use super::theme::{bold_font, Palette};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Clapper,
    Monitor,
    Folder,
    Refresh,
    Play,
}

fn line(painter: &Painter, points: Vec<Pos2>, stroke: Stroke) {
    painter.add(Shape::line(points, stroke));
}

fn closed(painter: &Painter, points: Vec<Pos2>, stroke: Stroke) {
    painter.add(Shape::closed_line(points, stroke));
}

/// Paints a ~16px line icon centred in `rect`.
pub fn paint_icon(painter: &Painter, rect: Rect, icon: Icon, color: Color32) {
    let s = Stroke::new(1.4, color);
    let r = Rect::from_center_size(rect.center(), vec2(15.0, 15.0));
    let p = |x: f32, y: f32| Pos2::new(r.left() + x * r.width(), r.top() + y * r.height());
    match icon {
        Icon::Clapper => {
            // Body and the clapper bar with two slanted stripes.
            closed(
                painter,
                vec![p(0.05, 0.42), p(0.95, 0.42), p(0.95, 0.92), p(0.05, 0.92)],
                s,
            );
            closed(
                painter,
                vec![p(0.05, 0.14), p(0.95, 0.14), p(0.95, 0.42), p(0.05, 0.42)],
                s,
            );
            line(painter, vec![p(0.35, 0.14), p(0.2, 0.42)], s);
            line(painter, vec![p(0.65, 0.14), p(0.5, 0.42)], s);
        }
        Icon::Monitor => {
            closed(
                painter,
                vec![p(0.05, 0.1), p(0.95, 0.1), p(0.95, 0.7), p(0.05, 0.7)],
                s,
            );
            line(painter, vec![p(0.5, 0.7), p(0.5, 0.9)], s);
            line(painter, vec![p(0.28, 0.9), p(0.72, 0.9)], s);
        }
        Icon::Folder => {
            closed(
                painter,
                vec![
                    p(0.04, 0.16),
                    p(0.38, 0.16),
                    p(0.5, 0.3),
                    p(0.96, 0.3),
                    p(0.96, 0.86),
                    p(0.04, 0.86),
                ],
                s,
            );
        }
        Icon::Refresh => {
            let c = r.center();
            let radius = r.width() * 0.4;
            let pts: Vec<Pos2> = (0..=22)
                .map(|i| {
                    let a = -0.5 + (i as f32 / 22.0) * 5.0;
                    c + radius * vec2(a.cos(), a.sin())
                })
                .collect();
            let end = *pts.last().unwrap_or(&c);
            line(painter, pts, s);
            line(painter, vec![end + vec2(-3.5, -0.5), end, end + vec2(0.5, -3.8)], s);
        }
        Icon::Play => {
            painter.add(Shape::convex_polygon(
                vec![p(0.22, 0.1), p(0.88, 0.5), p(0.22, 0.9)],
                color,
                Stroke::NONE,
            ));
        }
    }
}

/// Bold section title such as "1  Choose folders".
pub fn section_title(ui: &mut Ui, number: u32, text: &str) {
    ui.label(RichText::new(format!("{number}  {text}")).font(bold_font(16.0)));
}

/// Borderless, rounded card on the page background.
pub fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let p = Palette::of(ui.visuals());
    Frame::new()
        .fill(p.card)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Measures a pill so callers can right-align it.
pub fn pill_size(painter: &Painter, text: &str, font_size: f32) -> egui::Vec2 {
    let galley = painter.layout_no_wrap(text.to_owned(), bold_font(font_size), Color32::WHITE);
    vec2(galley.size().x + 20.0, galley.size().y + 8.0)
}

/// Paints a pill with `rect` already decided; returns nothing.
pub fn paint_pill(painter: &Painter, rect: Rect, text: &str, font_size: f32, fg: Color32, bg: Color32, outline: bool) {
    let radius = CornerRadius::same(5);
    painter.rect_filled(rect, radius, bg);
    if outline {
        painter.add(Shape::closed_line(
            round_rect_points(rect, 5.0),
            Stroke::new(1.0, fg.gamma_multiply(0.35)),
        ));
    }
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        bold_font(font_size),
        fg,
    );
}

fn round_rect_points(rect: Rect, r: f32) -> Vec<Pos2> {
    let mut pts = Vec::with_capacity(36);
    let corners = [
        (rect.right() - r, rect.top() + r, -90.0_f32),
        (rect.right() - r, rect.bottom() - r, 0.0),
        (rect.left() + r, rect.bottom() - r, 90.0),
        (rect.left() + r, rect.top() + r, 180.0),
    ];
    for (cx, cy, start) in corners {
        for i in 0..=6 {
            let a = (start + 90.0 * i as f32 / 6.0).to_radians();
            pts.push(Pos2::new(cx + r * a.cos(), cy + r * a.sin()));
        }
    }
    pts
}

/// Inline pill (used for the "N events" counter).
pub fn pill(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) {
    let size = pill_size(ui.painter(), text, 11.5);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    paint_pill(ui.painter(), rect, text, 11.5, fg, bg, false);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BtnStyle {
    Secondary,
    Primary,
    /// Text-only, no background.
    Link,
    /// Option inside the theme switcher.
    Segment { active: bool },
}

pub struct Btn<'a> {
    label: &'a str,
    style: BtnStyle,
    icon: Option<Icon>,
    enabled: bool,
    radius: CornerRadius,
    min_width: f32,
    height: f32,
}

impl<'a> Btn<'a> {
    pub fn new(label: &'a str, style: BtnStyle) -> Self {
        Btn {
            label,
            style,
            icon: None,
            enabled: true,
            radius: CornerRadius::same(6),
            min_width: 0.0,
            height: 34.0,
        }
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn radius(mut self, radius: CornerRadius) -> Self {
        self.radius = radius;
        self
    }

    pub fn min_width(mut self, min_width: f32) -> Self {
        self.min_width = min_width;
        self
    }

    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    pub fn small(self) -> Self {
        self.height(30.0)
    }

    pub fn show(self, ui: &mut Ui) -> egui::Response {
        let p = Palette::of(ui.visuals());
        let font = match self.style {
            BtnStyle::Segment { active: true } | BtnStyle::Primary => bold_font(14.0),
            _ => FontId::proportional(14.0),
        };
        let dim = |c: Color32, enabled: bool| if enabled { c } else { c.gamma_multiply(0.45) };

        // Measure with a placeholder colour; the real colour depends on hover.
        let measure = ui
            .painter()
            .layout_no_wrap(self.label.to_owned(), font.clone(), Color32::WHITE);
        let icon_w = if self.icon.is_some() { 16.0 + 7.0 } else { 0.0 };
        let pad_x = match self.style {
            BtnStyle::Link => 4.0,
            BtnStyle::Segment { .. } => 12.0,
            _ => 14.0,
        };
        let width = (measure.size().x + icon_w + pad_x * 2.0).max(self.min_width);

        let sense = if self.enabled { Sense::click() } else { Sense::hover() };
        let (rect, response) = ui.allocate_exact_size(vec2(width, self.height), sense);
        let hovered = self.enabled && response.hovered();
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }

        let (bg, fg) = match self.style {
            BtnStyle::Secondary => (
                Some(if hovered { p.button_hover } else { p.button }),
                p.text,
            ),
            BtnStyle::Primary => (
                Some(if hovered {
                    p.accent.gamma_multiply(0.88)
                } else {
                    p.accent
                }),
                p.on_accent,
            ),
            BtnStyle::Link => (None, if hovered { p.text } else { p.weak }),
            BtnStyle::Segment { active } => {
                if active {
                    (Some(p.accent), p.on_accent)
                } else {
                    (
                        if hovered { Some(p.button_hover) } else { None },
                        p.weak,
                    )
                }
            }
        };
        let fg = dim(fg, self.enabled);

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            if self.style == BtnStyle::Primary && p.dark && self.enabled {
                // Soft glow behind the primary action in dark mode.
                painter.rect_filled(
                    rect.expand(4.0),
                    CornerRadius::same(10),
                    p.accent.gamma_multiply(0.16),
                );
            }
            if let Some(bg) = bg {
                painter.rect_filled(rect, self.radius, dim(bg, self.enabled));
            }
            let galley = painter.layout_no_wrap(self.label.to_owned(), font, fg);
            let content_w = icon_w + galley.size().x;
            let mut x = rect.center().x - content_w / 2.0;
            if let Some(icon) = self.icon {
                let icon_rect = Rect::from_min_size(
                    Pos2::new(x, rect.center().y - 8.0),
                    vec2(16.0, 16.0),
                );
                paint_icon(painter, icon_rect, icon, fg);
                x += icon_w;
            }
            painter.galley(
                Pos2::new(x, rect.center().y - galley.size().y / 2.0),
                galley,
                fg,
            );
        }

        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, self.enabled, self.label));
        response
    }
}

/// Text input joined to a "Browse" button. Returns the text response and
/// whether Browse was clicked. `width` is the total width of the group.
pub fn path_input(
    ui: &mut Ui,
    width: f32,
    text: &mut String,
    enabled: bool,
    hint: &str,
) -> (egui::Response, bool) {
    const H: f32 = 38.0;
    const BROWSE_W: f32 = 88.0;
    let p = Palette::of(ui.visuals());
    let mut browse = false;
    let mut response = None;
    // `scope` keeps the parent's left-to-right layout; a nested `horizontal`
    // gets its own row height and ends up misaligned with its siblings.
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let input_w = (width - BROWSE_W).max(60.0);
        Frame::new()
            .fill(p.input)
            .corner_radius(CornerRadius {
                nw: 6,
                sw: 6,
                ne: 0,
                se: 0,
            })
            .inner_margin(Margin::symmetric(12, 0))
            .show(ui, |ui| {
                let inner_w = input_w - 24.0;
                ui.set_width(inner_w);
                ui.set_height(H);
                response = Some(ui.add_enabled(
                    enabled,
                    egui::TextEdit::singleline(text)
                        .frame(Frame::NONE)
                        .desired_width(inner_w)
                        .min_size(vec2(inner_w, H))
                        .vertical_align(Align::Center)
                        .hint_text(hint),
                ));
            });
        browse = Btn::new("Browse", BtnStyle::Secondary)
            .enabled(enabled)
            .height(H)
            .min_width(BROWSE_W)
            .radius(CornerRadius {
                nw: 0,
                sw: 0,
                ne: 6,
                se: 6,
            })
            .show(ui)
            .clicked();
    });
    (response.expect("text edit is always added"), browse)
}

/// Radio with an accent ring and dot.
pub fn radio(ui: &mut Ui, selected: bool, label: &str, enabled: bool) -> egui::Response {
    let p = Palette::of(ui.visuals());
    let color = if enabled { p.text } else { p.text.gamma_multiply(0.45) };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), FontId::proportional(14.0), color);
    let size = vec2(18.0 + 8.0 + galley.size().x, 24.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    if enabled && response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let painter = ui.painter();
    let center = Pos2::new(rect.left() + 9.0, rect.center().y);
    let ring = if selected { p.accent } else { p.weak };
    let ring = if enabled { ring } else { ring.gamma_multiply(0.45) };
    painter.circle_stroke(center, 8.0, Stroke::new(1.6, ring));
    if selected {
        painter.circle_filled(center, 4.0, ring);
    }
    painter.galley(
        Pos2::new(rect.left() + 26.0, rect.center().y - galley.size().y / 2.0),
        galley,
        color,
    );
    response.widget_info(|| WidgetInfo::selected(WidgetType::RadioButton, enabled, selected, label));
    response
}

/// Light / Dark / System segmented control bound to egui's theme preference.
pub fn theme_switcher(ui: &mut Ui) {
    let p = Palette::of(ui.visuals());
    let current = ui.ctx().options(|o| o.theme_preference);
    Frame::new()
        .fill(p.button)
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::same(3))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (label, pref) in [
                    ("Light", egui::ThemePreference::Light),
                    ("Dark", egui::ThemePreference::Dark),
                    ("System", egui::ThemePreference::System),
                ] {
                    let clicked = Btn::new(label, BtnStyle::Segment { active: current == pref })
                        .height(26.0)
                        .show(ui)
                        .clicked();
                    if clicked {
                        ui.ctx().set_theme(pref);
                    }
                }
            });
        });
}

/// Right-aligned helper for header rows.
pub fn right<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.with_layout(Layout::right_to_left(Align::Center), add).inner
}

/// Painted checkbox used by list rows (the whole row is the click target).
pub fn paint_checkbox(painter: &Painter, rect: Rect, checked: bool, enabled: bool, p: &Palette) {
    let radius = CornerRadius::same(4);
    let dim = |c: Color32| if enabled { c } else { c.gamma_multiply(0.45) };
    if checked {
        painter.rect_filled(rect, radius, dim(p.check));
        let pt = |x: f32, y: f32| Pos2::new(rect.left() + x * rect.width(), rect.top() + y * rect.height());
        painter.add(Shape::line(
            vec![pt(0.24, 0.52), pt(0.43, 0.70), pt(0.77, 0.32)],
            Stroke::new(1.8, Color32::WHITE),
        ));
    } else {
        painter.rect_filled(rect, radius, dim(p.weak));
        painter.rect_filled(rect.shrink(1.5), CornerRadius::same(3), p.card);
    }
}
