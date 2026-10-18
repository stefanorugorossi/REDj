//! Club look: red background, black neon-edged panels, high-contrast
//! controls, and a glowing red REDj logo.

use egui::epaint::Shadow;
use egui::{
    Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Margin, Stroke, TextStyle, Ui, Vec2,
    vec2,
};

/// Background red.
pub const RED_BG: Color32 = Color32::from_rgb(168, 0, 14);
/// Logo / accent red (brighter than the background).
pub const RED: Color32 = Color32::from_rgb(255, 18, 40);
/// Panel black.
pub const BLACK: Color32 = Color32::from_rgb(8, 8, 10);
/// Neon yellow for active states.
pub const NEON: Color32 = Color32::from_rgb(255, 230, 0);

/// Applies the global visuals and text sizes.
pub fn apply(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = RED_BG;
    v.window_fill = BLACK;
    v.window_stroke = Stroke::new(2.0, RED);
    v.extreme_bg_color = Color32::from_rgb(20, 0, 2);
    v.faint_bg_color = Color32::from_rgb(28, 4, 6);
    v.code_bg_color = BLACK;
    v.selection.bg_fill = NEON;
    v.selection.stroke = Stroke::new(1.0, Color32::BLACK);
    v.override_text_color = Some(Color32::WHITE);
    let r = CornerRadius::same(3);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = BLACK;
    w.noninteractive.weak_bg_fill = BLACK;
    w.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(255, 60, 70));
    w.noninteractive.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    w.noninteractive.corner_radius = r;
    w.inactive.bg_fill = Color32::from_rgb(22, 22, 24);
    w.inactive.weak_bg_fill = Color32::from_rgb(22, 22, 24);
    w.inactive.bg_stroke = Stroke::new(1.0, Color32::from_rgb(90, 90, 95));
    w.inactive.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    w.inactive.corner_radius = r;
    w.hovered.bg_fill = Color32::from_rgb(70, 0, 8);
    w.hovered.weak_bg_fill = Color32::from_rgb(70, 0, 8);
    w.hovered.bg_stroke = Stroke::new(1.5, NEON);
    w.hovered.fg_stroke = Stroke::new(2.0, NEON);
    w.hovered.corner_radius = r;
    // Note: egui derives "strong" text from the active foreground, so it
    // must stay light.
    w.active.bg_fill = RED;
    w.active.weak_bg_fill = RED;
    w.active.bg_stroke = Stroke::new(2.0, NEON);
    w.active.fg_stroke = Stroke::new(2.0, Color32::WHITE);
    w.active.corner_radius = r;
    w.open = w.active;
    ctx.set_visuals(v);
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (
                TextStyle::Small,
                FontId::new(11.0, FontFamily::Proportional),
            ),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (
                TextStyle::Button,
                FontId::new(15.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Heading,
                FontId::new(22.0, FontFamily::Proportional),
            ),
            (
                TextStyle::Monospace,
                FontId::new(14.0, FontFamily::Monospace),
            ),
        ]
        .into();
        s.spacing.button_padding = vec2(8.0, 4.0);
        s.spacing.item_spacing = vec2(6.0, 5.0);
    });
}

/// Black panel with a red neon edge and glow.
pub fn neon_frame() -> Frame {
    Frame::new()
        .fill(BLACK)
        .stroke(Stroke::new(2.0, RED))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::same(8))
        .shadow(Shadow {
            offset: [0, 0],
            blur: 18,
            spread: 2,
            color: Color32::from_rgba_unmultiplied(255, 30, 30, 110),
        })
}

/// Black bar frame (top bar, status bar, library).
pub fn bar_frame() -> Frame {
    Frame::new()
        .fill(BLACK)
        .inner_margin(Margin::symmetric(10, 6))
}

/// Draws `text` large and glowing in red, faux-bold. Returns its width.
pub fn glow_text(ui: &mut Ui, text: &str, size: f32, color: Color32) -> f32 {
    let font = FontId::new(size, FontFamily::Proportional);
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font.clone(), color);
    let sz = galley.size() + vec2(8.0, 4.0);
    let (rect, _) = ui.allocate_exact_size(sz, egui::Sense::hover());
    let p = ui.painter();
    let pos = rect.left_center() + vec2(4.0, 0.0);
    // Glow: concentric translucent copies.
    for (r, a) in [(6.0f32, 25u8), (4.0, 45), (2.0, 80)] {
        for k in 0..8 {
            let ang = k as f32 * std::f32::consts::FRAC_PI_4;
            let o = Vec2::angled(ang) * r;
            p.text(
                pos + o,
                Align2::LEFT_CENTER,
                text,
                font.clone(),
                Color32::from_rgba_unmultiplied(color.r(), color.g() / 2, color.b() / 2, a),
            );
        }
    }
    // Faux bold with a thin white rim on top.
    for dx in [0.0, 0.7, 1.4] {
        p.text(
            pos + vec2(dx, 0.0),
            Align2::LEFT_CENTER,
            text,
            font.clone(),
            color,
        );
    }
    p.text(
        pos + vec2(0.7, -0.6),
        Align2::LEFT_CENTER,
        text,
        font,
        Color32::from_rgba_unmultiplied(255, 255, 255, 40),
    );
    sz.x
}

/// The REDj logo.
pub fn logo(ui: &mut Ui) {
    glow_text(ui, "REDj", 34.0, RED);
}

/// Big embossed "REDj" filling the free space left in `ui`, if any.
pub fn watermark(ui: &mut Ui) {
    let r = ui.available_rect_before_wrap();
    if r.height() < 120.0 {
        return;
    }
    let size = (r.height() * 0.75).min(r.width() * 0.32);
    let at = r.center();
    let font = FontId::proportional(size);
    let p = ui.painter();
    p.text(
        at + vec2(5.0, 5.0),
        Align2::CENTER_CENTER,
        "REDj",
        font.clone(),
        Color32::from_rgb(80, 0, 6),
    );
    p.text(
        at,
        Align2::CENTER_CENTER,
        "REDj",
        font,
        Color32::from_rgb(205, 12, 30),
    );
}
