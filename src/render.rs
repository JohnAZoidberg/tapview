use crate::multitouch::{ButtonState, TouchData};
use egui::{Color32, FontId, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2};

pub const MAGENTA: Color32 = Color32::from_rgb(255, 0, 182);
pub const TEAL: Color32 = Color32::from_rgb(0, 213, 255);
pub const ORANGE: Color32 = Color32::from_rgb(255, 101, 0);
pub const PALM_GRAY: Color32 = Color32::from_rgb(160, 160, 160);

const MT_TOOL_PALM: i32 = 0x02;

fn fade(color: Color32, alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), (255.0 * alpha) as u8)
}

fn touch_color_for_slot(slot: usize, touch: &TouchData) -> Color32 {
    if touch.tool_type == MT_TOOL_PALM {
        PALM_GRAY
    } else if slot == 0 {
        MAGENTA
    } else {
        TEAL
    }
}

pub fn draw_touchpad_boundary(painter: &Painter, corner: Pos2, width: f32, height: f32) {
    painter.rect_stroke(
        Rect::from_min_size(corner, Vec2::new(width, height)),
        0.0,
        Stroke::new(1.0, ORANGE),
        StrokeKind::Outside,
    );
}

pub fn draw_ring(
    painter: &Painter,
    center: Pos2,
    inner_radius: f32,
    outer_radius: f32,
    color: Color32,
) {
    let mid_radius = (inner_radius + outer_radius) / 2.0;
    let thickness = outer_radius - inner_radius;
    painter.circle_stroke(center, mid_radius, Stroke::new(thickness, color));
}

pub fn draw_trail(
    painter: &Painter,
    touch: &TouchData,
    slot: usize,
    corner: Pos2,
    scale: f32,
    cscale: f32,
) {
    let pos = touch_to_screen(touch, corner, scale);
    let color = fade(touch_color_for_slot(slot, touch), 0.2);
    draw_ring(painter, pos, 1.0, 36.0 * cscale, color);
}

pub fn draw_touch(
    painter: &Painter,
    touch: &TouchData,
    slot: usize,
    corner: Pos2,
    scale: f32,
    cscale: f32,
) {
    let pos = touch_to_screen(touch, corner, scale);
    let color = touch_color_for_slot(slot, touch);

    // Main circle
    painter.circle_filled(pos, 34.0 * cscale, color);

    // Double-tap ring
    if touch.pressed_double {
        draw_ring(painter, pos, 14.0 * cscale, 20.0 * cscale, Color32::BLACK);
    }

    // Pressed dot
    if touch.pressed {
        painter.circle_filled(pos, 8.0 * cscale, Color32::BLACK);
    }

    // Slot number label
    let label_pos = Pos2::new(pos.x - 10.0 * cscale, pos.y - 70.0 * cscale);
    painter.text(
        label_pos,
        egui::Align2::LEFT_TOP,
        format!("{}", slot),
        FontId::monospace(40.0 * cscale),
        Color32::BLACK,
    );
}

pub fn draw_button_indicators(
    painter: &Painter,
    buttons: &ButtonState,
    corner: Pos2,
    boundary_width: f32,
    boundary_height: f32,
) {
    let y = corner.y + boundary_height + 8.0;
    let font = FontId::monospace(14.0);
    let labels = [
        ("L", buttons.left),
        ("M", buttons.middle),
        ("R", buttons.right),
    ];

    let total_width = labels.len() as f32 * 24.0 - 8.0;
    let start_x = corner.x + boundary_width / 2.0 - total_width / 2.0;

    for (i, (label, active)) in labels.iter().enumerate() {
        let x = start_x + i as f32 * 24.0;
        let center = Pos2::new(x, y);
        let color = if *active {
            MAGENTA
        } else {
            Color32::from_rgb(200, 200, 200)
        };
        painter.text(
            center,
            egui::Align2::CENTER_TOP,
            *label,
            font.clone(),
            color,
        );
    }
}

fn touch_to_screen(touch: &TouchData, corner: Pos2, scale: f32) -> Pos2 {
    Pos2::new(
        corner.x + touch.position_x as f32 * scale,
        corner.y + touch.position_y as f32 * scale,
    )
}
