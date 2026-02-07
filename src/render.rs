use crate::libinput_state::{GestureKind, LibinputState};
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

// --- libinput visualization ---

const CROSS_SIZE: f32 = 40.0;
const ACCEL_COLOR: Color32 = MAGENTA;
const UNACCEL_COLOR: Color32 = Color32::from_rgb(180, 180, 180);

/// Draw a cross widget showing a 2D vector.
/// `accel` is drawn as filled bars, `unaccel` as outline bars.
/// `scale_factor` maps raw values to pixels.
fn draw_cross(
    painter: &Painter,
    center: Pos2,
    accel: (f32, f32),
    unaccel: (f32, f32),
    scale_factor: f32,
    bar_width: f32,
) {
    let max = CROSS_SIZE;

    // Draw faint cross lines for reference
    let guide_color = Color32::from_rgb(230, 230, 230);
    painter.line_segment(
        [
            Pos2::new(center.x - max, center.y),
            Pos2::new(center.x + max, center.y),
        ],
        Stroke::new(1.0, guide_color),
    );
    painter.line_segment(
        [
            Pos2::new(center.x, center.y - max),
            Pos2::new(center.x, center.y + max),
        ],
        Stroke::new(1.0, guide_color),
    );

    // Draw unaccelerated (outline) first, then accelerated (filled) on top
    let pairs = [(unaccel, UNACCEL_COLOR, false), (accel, ACCEL_COLOR, true)];

    for &((dx, dy), color, filled) in &pairs {
        let sx = (dx * scale_factor).clamp(-max, max);
        let sy = (dy * scale_factor).clamp(-max, max);

        // Horizontal bar
        if sx.abs() > 0.5 {
            let rect = if sx > 0.0 {
                Rect::from_min_max(
                    Pos2::new(center.x, center.y - bar_width / 2.0),
                    Pos2::new(center.x + sx, center.y + bar_width / 2.0),
                )
            } else {
                Rect::from_min_max(
                    Pos2::new(center.x + sx, center.y - bar_width / 2.0),
                    Pos2::new(center.x, center.y + bar_width / 2.0),
                )
            };
            if filled {
                painter.rect_filled(rect, 0.0, color);
            } else {
                painter.rect_stroke(rect, 0.0, Stroke::new(1.0, color), StrokeKind::Outside);
            }
        }

        // Vertical bar
        if sy.abs() > 0.5 {
            let rect = if sy > 0.0 {
                Rect::from_min_max(
                    Pos2::new(center.x - bar_width / 2.0, center.y),
                    Pos2::new(center.x + bar_width / 2.0, center.y + sy),
                )
            } else {
                Rect::from_min_max(
                    Pos2::new(center.x - bar_width / 2.0, center.y + sy),
                    Pos2::new(center.x + bar_width / 2.0, center.y),
                )
            };
            if filled {
                painter.rect_filled(rect, 0.0, color);
            } else {
                painter.rect_stroke(rect, 0.0, Stroke::new(1.0, color), StrokeKind::Outside);
            }
        }
    }

    // Center dot
    painter.circle_filled(center, 2.0, Color32::BLACK);
}

/// Draw the full libinput visualization panel contents.
pub fn draw_libinput_panel(ui: &mut egui::Ui, state: &LibinputState) {
    let painter = ui.painter();
    let panel_rect = ui.available_rect_before_wrap();
    let panel_width = panel_rect.width();
    let mut y = panel_rect.min.y + 10.0;
    let cx = panel_rect.min.x + panel_width / 2.0;
    let label_font = FontId::proportional(11.0);
    let section_font = FontId::proportional(13.0);

    // --- Pointer Motion ---
    painter.text(
        Pos2::new(cx, y),
        egui::Align2::CENTER_TOP,
        "Pointer Motion",
        section_font.clone(),
        Color32::BLACK,
    );
    y += 18.0;

    let motion_center = Pos2::new(cx, y + CROSS_SIZE);
    draw_cross(
        painter,
        motion_center,
        state.motion_accel,
        state.motion_unaccel,
        4.0,
        6.0,
    );
    y += CROSS_SIZE * 2.0 + 8.0;

    // Legend
    painter.rect_filled(
        Rect::from_min_size(Pos2::new(cx - 50.0, y), Vec2::new(10.0, 10.0)),
        0.0,
        ACCEL_COLOR,
    );
    painter.text(
        Pos2::new(cx - 36.0, y),
        egui::Align2::LEFT_TOP,
        "accel",
        label_font.clone(),
        Color32::DARK_GRAY,
    );
    painter.rect_stroke(
        Rect::from_min_size(Pos2::new(cx + 10.0, y), Vec2::new(10.0, 10.0)),
        0.0,
        Stroke::new(1.0, UNACCEL_COLOR),
        StrokeKind::Outside,
    );
    painter.text(
        Pos2::new(cx + 24.0, y),
        egui::Align2::LEFT_TOP,
        "raw",
        label_font.clone(),
        Color32::DARK_GRAY,
    );
    y += 24.0;

    // --- Buttons ---
    painter.text(
        Pos2::new(cx, y),
        egui::Align2::CENTER_TOP,
        "Buttons",
        section_font.clone(),
        Color32::BLACK,
    );
    y += 18.0;

    {
        let labels = [
            ("L", "1f tap", state.buttons.left),
            ("M", "3f tap", state.buttons.middle),
            ("R", "2f tap", state.buttons.right),
        ];
        let spacing = 56.0;
        let total_width = labels.len() as f32 * spacing - spacing * 0.3;
        let start_x = cx - total_width / 2.0;
        let btn_font = FontId::monospace(16.0);
        let tap_font = FontId::proportional(9.0);

        for (i, (label, tap_label, intensity)) in labels.iter().enumerate() {
            let x = start_x + i as f32 * spacing;
            let color = if *intensity > 0.1 {
                fade(MAGENTA, intensity.clamp(0.0, 1.0))
            } else {
                Color32::from_rgb(200, 200, 200)
            };
            painter.text(
                Pos2::new(x, y),
                egui::Align2::CENTER_TOP,
                *label,
                btn_font.clone(),
                color,
            );
            painter.text(
                Pos2::new(x, y + 18.0),
                egui::Align2::CENTER_TOP,
                *tap_label,
                tap_font.clone(),
                Color32::DARK_GRAY,
            );
        }
    }
    y += 38.0;

    // --- Scroll ---
    painter.text(
        Pos2::new(cx, y),
        egui::Align2::CENTER_TOP,
        format!(
            "Scroll{}",
            if state.scroll_source.is_empty() {
                String::new()
            } else {
                format!(" ({})", state.scroll_source)
            }
        ),
        section_font.clone(),
        Color32::BLACK,
    );
    y += 18.0;

    let scroll_center = Pos2::new(cx, y + CROSS_SIZE);
    draw_cross(
        painter,
        scroll_center,
        (state.scroll_horiz, state.scroll_vert),
        (0.0, 0.0), // no unaccel for scroll
        3.0,
        6.0,
    );
    y += CROSS_SIZE * 2.0 + 16.0;

    // --- Gesture ---
    let gesture_label = match state.gesture.kind {
        GestureKind::Swipe => format!("Swipe ({}f)", state.gesture.fingers),
        GestureKind::Pinch => format!("Pinch ({}f)", state.gesture.fingers),
        GestureKind::Hold => format!("Hold ({}f)", state.gesture.fingers),
        GestureKind::None => "Gesture".to_string(),
    };
    painter.text(
        Pos2::new(cx, y),
        egui::Align2::CENTER_TOP,
        gesture_label,
        section_font,
        Color32::BLACK,
    );
    y += 18.0;

    if state.gesture.active {
        let gesture_center = Pos2::new(cx, y + CROSS_SIZE);

        // Translation cross
        draw_cross(
            painter,
            gesture_center,
            (state.gesture.dx, state.gesture.dy),
            (state.gesture.dx_unaccel, state.gesture.dy_unaccel),
            4.0,
            6.0,
        );

        if state.gesture.kind == GestureKind::Pinch {
            // Scale ring: radius proportional to scale factor
            let base_radius = 20.0;
            let ring_radius = base_radius * state.gesture.scale;
            painter.circle_stroke(
                gesture_center,
                ring_radius.clamp(4.0, CROSS_SIZE * 1.5),
                Stroke::new(2.0, TEAL),
            );

            // Rotation indicator: a line from center at the angle
            if state.gesture.angle.abs() > 0.1 {
                let angle_rad = state.gesture.angle.to_radians();
                let line_len = 30.0;
                let end = Pos2::new(
                    gesture_center.x + angle_rad.sin() * line_len,
                    gesture_center.y - angle_rad.cos() * line_len,
                );
                painter.line_segment([gesture_center, end], Stroke::new(2.0, ORANGE));
            }
        }

        y += CROSS_SIZE * 2.0 + 16.0;
    } else {
        // Show inactive placeholder
        let gesture_center = Pos2::new(cx, y + CROSS_SIZE);
        draw_cross(painter, gesture_center, (0.0, 0.0), (0.0, 0.0), 1.0, 6.0);
        y += CROSS_SIZE * 2.0 + 16.0;
    }

    // --- Small text log at bottom ---
    let log_top = y;
    let log_rect = Rect::from_min_max(Pos2::new(panel_rect.min.x + 4.0, log_top), panel_rect.max);

    ui.allocate_rect(
        Rect::from_min_max(panel_rect.min, Pos2::new(panel_rect.max.x, log_top)),
        egui::Sense::hover(),
    );

    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(log_rect), |ui| {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in &state.log_lines {
                    ui.label(
                        egui::RichText::new(line)
                            .font(FontId::monospace(9.0))
                            .color(Color32::from_rgb(80, 80, 80)),
                    );
                }
            });
    });
}
