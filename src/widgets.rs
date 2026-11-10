//! Colours and the hand-drawn controls (transport buttons, slider bars).

use eframe::egui::{
    self, Color32, CornerRadius, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2, pos2, vec2,
};

pub const BG: Color32 = Color32::from_rgb(0x15, 0x17, 0x1c);
pub const PANEL: Color32 = Color32::from_rgb(0x1d, 0x20, 0x27);
pub const CARD: Color32 = Color32::from_rgb(0x24, 0x28, 0x31);
pub const ROW_HOVER: Color32 = Color32::from_rgb(0x2a, 0x2e, 0x38);
pub const ROW_SELECTED: Color32 = Color32::from_rgb(0x33, 0x38, 0x44);
pub const TEXT: Color32 = Color32::from_rgb(0xe8, 0xea, 0xef);
pub const TEXT_DIM: Color32 = Color32::from_rgb(0x8d, 0x93, 0xa1);
pub const TRACK: Color32 = Color32::from_rgb(0x3a, 0x3f, 0x4b);
pub const ACCENT: Color32 = Color32::from_rgb(0xff, 0xae, 0x52);
pub const ACCENT_DEEP: Color32 = Color32::from_rgb(0xf0, 0x74, 0x3e);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x1b, 0x1d, 0x23);
pub const ERROR: Color32 = Color32::from_rgb(0xf2, 0x6d, 0x6d);

#[derive(Clone, Copy)]
pub enum Glyph {
    Play,
    Pause,
    Prev,
    Next,
}

fn paint_glyph(painter: &egui::Painter, glyph: Glyph, center: Pos2, size: f32, color: Color32) {
    let h = size / 2.0;
    match glyph {
        Glyph::Play => {
            let points = vec![
                pos2(center.x - h * 0.7, center.y - h),
                pos2(center.x + h, center.y),
                pos2(center.x - h * 0.7, center.y + h),
            ];
            painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
        }
        Glyph::Pause => {
            let bar = vec2(h * 0.55, size);
            for dx in [-h * 0.5, h * 0.5] {
                let rect = Rect::from_center_size(pos2(center.x + dx, center.y), bar);
                painter.rect_filled(rect, CornerRadius::same(2), color);
            }
        }
        Glyph::Prev | Glyph::Next => {
            let dir = if matches!(glyph, Glyph::Next) { 1.0 } else { -1.0 };
            let tip = center.x + dir * h * 0.55;
            let base = center.x - dir * h * 0.75;
            painter.add(Shape::convex_polygon(
                vec![pos2(base, center.y - h * 0.8), pos2(tip, center.y), pos2(base, center.y + h * 0.8)],
                color,
                Stroke::NONE,
            ));
            let bar = Rect::from_center_size(pos2(tip + dir * h * 0.15, center.y), vec2(h * 0.3, h * 1.6));
            painter.rect_filled(bar, CornerRadius::same(1), color);
        }
    }
}

/// Round transport button. `primary` is the big filled play/pause button.
pub fn transport_button(ui: &mut Ui, glyph: Glyph, diameter: f32, primary: bool, enabled: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(diameter), Sense::click());
    let response = if enabled { response } else { response.on_hover_cursor(egui::CursorIcon::NotAllowed) };
    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let hovered = enabled && response.hovered();
        let (fill, icon) = if primary {
            let fill = match (enabled, hovered) {
                (false, _) => TRACK,
                (true, true) => Color32::from_rgb(0xff, 0xc0, 0x70),
                (true, false) => ACCENT,
            };
            (fill, ON_ACCENT)
        } else {
            let fill = if hovered { ROW_HOVER } else { Color32::TRANSPARENT };
            (fill, if enabled { TEXT } else { TRACK })
        };
        painter.circle_filled(rect.center(), diameter / 2.0, fill);
        let glyph_size = diameter * if primary { 0.36 } else { 0.42 };
        // A play triangle looks off-centre unless nudged right a little.
        let nudge = if matches!(glyph, Glyph::Play) { vec2(diameter * 0.03, 0.0) } else { Vec2::ZERO };
        paint_glyph(painter, glyph, rect.center() + nudge, glyph_size, icon);
    }
    if enabled { response.on_hover_cursor(egui::CursorIcon::PointingHand) } else { response }
}

/// A thin horizontal bar with a knob. Returns the fraction the person
/// clicked or dragged to, if they did.
pub fn slider_bar(ui: &mut Ui, fraction: f32, width: f32, enabled: bool) -> (Response, Option<f32>) {
    let height = 18.0;
    let sense = if enabled { Sense::click_and_drag() } else { Sense::hover() };
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), sense);
    let line = Rect::from_center_size(rect.center(), vec2(rect.width() - 8.0, 4.0));

    let mut picked = None;
    if enabled && (response.dragged() || response.clicked()) {
        if let Some(pointer) = response.interact_pointer_pos() {
            picked = Some(((pointer.x - line.left()) / line.width()).clamp(0.0, 1.0));
        }
    }

    if ui.is_rect_visible(rect) {
        let shown = picked.unwrap_or(fraction).clamp(0.0, 1.0);
        let painter = ui.painter();
        painter.rect_filled(line, CornerRadius::same(2), TRACK);
        if enabled {
            let filled = Rect::from_min_max(line.min, pos2(line.left() + line.width() * shown, line.max.y));
            painter.rect_filled(filled, CornerRadius::same(2), ACCENT);
            let active = response.hovered() || response.dragged();
            let knob = if active { 7.0 } else { 5.0 };
            painter.circle_filled(pos2(filled.right(), line.center().y), knob, if active { TEXT } else { ACCENT });
        }
    }
    let response = if enabled { response.on_hover_cursor(egui::CursorIcon::PointingHand) } else { response };
    (response, picked)
}

/// Placeholder cover: a coloured tile with a music note. `seed` picks the
/// colour, so songs from one folder share a tile.
pub fn cover(ui: &mut Ui, size: f32, seed: Option<u64>) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let painter = ui.painter_at(rect);
    let fill = match seed {
        Some(seed) => Color32::from(egui::ecolor::Hsva::new((seed % 360) as f32 / 360.0, 0.55, 0.55, 1.0)),
        None => TRACK,
    };
    painter.rect_filled(rect, CornerRadius::same(12), fill);
    // Soft highlight kept clear of the rounded corners.
    painter.circle_filled(
        pos2(rect.center().x + size * 0.12, rect.top() + size * 0.34),
        size * 0.34,
        Color32::from_white_alpha(18),
    );

    let note = Color32::from_white_alpha(220);
    let c = rect.center();
    let s = size / 100.0;
    painter.circle_filled(pos2(c.x - 12.0 * s, c.y + 16.0 * s), 9.0 * s, note);
    painter.circle_filled(pos2(c.x + 16.0 * s, c.y + 10.0 * s), 9.0 * s, note);
    painter.line_segment([pos2(c.x - 4.0 * s, c.y + 16.0 * s), pos2(c.x - 4.0 * s, c.y - 20.0 * s)], Stroke::new(4.0 * s, note));
    painter.line_segment([pos2(c.x + 24.0 * s, c.y + 10.0 * s), pos2(c.x + 24.0 * s, c.y - 26.0 * s)], Stroke::new(4.0 * s, note));
    painter.add(Shape::convex_polygon(
        vec![
            pos2(c.x - 6.0 * s, c.y - 20.0 * s),
            pos2(c.x + 26.0 * s, c.y - 26.0 * s),
            pos2(c.x + 26.0 * s, c.y - 18.0 * s),
            pos2(c.x - 6.0 * s, c.y - 12.0 * s),
        ],
        note,
        Stroke::NONE,
    ));
}

/// Small animated bars that mark the playing row.
pub fn playing_bars(ui: &Ui, center: Pos2, animate: bool) {
    let time = ui.input(|i| i.time) as f32;
    let painter = ui.painter();
    for k in 0..3 {
        let phase = time * 6.0 + k as f32 * 1.7;
        let height = if animate { 4.0 + 7.0 * (0.5 + 0.5 * phase.sin()) } else { 5.0 + k as f32 * 2.0 };
        let x = center.x + (k as f32 - 1.0) * 4.5;
        let rect = Rect::from_min_max(pos2(x - 1.5, center.y + 6.0 - height), pos2(x + 1.5, center.y + 6.0));
        painter.rect_filled(rect, CornerRadius::same(1), ACCENT);
    }
}

pub fn format_time(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}
