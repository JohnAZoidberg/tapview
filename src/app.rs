use crate::dimensions::Dimensions;
use crate::input::TouchState;
use crate::multitouch::{ButtonState, TouchData, MAX_TOUCH_POINTS};
use crate::render;
use std::sync::mpsc;

const HISTORY_MAX: usize = 20;

pub enum GrabCommand {
    Grab,
    Ungrab,
}

pub struct TapviewApp {
    touch_rx: mpsc::Receiver<TouchState>,
    grab_tx: mpsc::Sender<GrabCommand>,
    dims: Dimensions,
    current_touches: [TouchData; MAX_TOUCH_POINTS],
    buttons: ButtonState,
    touch_history: Vec<[TouchData; MAX_TOUCH_POINTS]>,
    trails: usize,
    grabbed: bool,
}

impl TapviewApp {
    pub fn new(
        touch_rx: mpsc::Receiver<TouchState>,
        grab_tx: mpsc::Sender<GrabCommand>,
        trails: usize,
    ) -> Self {
        Self {
            touch_rx,
            grab_tx,
            dims: Dimensions::default(),
            current_touches: [TouchData::default(); MAX_TOUCH_POINTS],
            buttons: ButtonState::default(),
            touch_history: vec![[TouchData::default(); MAX_TOUCH_POINTS]; HISTORY_MAX],
            trails,
            grabbed: false,
        }
    }
}

impl eframe::App for TapviewApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain all pending touch states from the input thread
        while let Ok(state) = self.touch_rx.try_recv() {
            self.current_touches = state.touches;
            self.buttons = state.buttons;
        }

        // Update screen dimensions
        let screen_rect = ctx.screen_rect();
        self.dims.screen_width = screen_rect.width();
        self.dims.screen_height = screen_rect.height();

        // Handle grab/ungrab keys
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Enter) && !self.grabbed {
                let _ = self.grab_tx.send(GrabCommand::Grab);
                self.grabbed = true;
            } else if i.key_pressed(egui::Key::Escape) && self.grabbed {
                let _ = self.grab_tx.send(GrabCommand::Ungrab);
                self.grabbed = false;
            }
        });

        // Grow touchpad extents from current touches
        for touch in &self.current_touches {
            if touch.used {
                self.dims
                    .maybe_grow_touchpad_extent(touch.position_x as f32, touch.position_y as f32);
            }
        }

        let scale = self.dims.get_touchpad_scale();
        let corner = self.dims.get_touchpad_corner(scale);
        let cscale = scale.clamp(0.5, 2.0);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::WHITE))
            .show(ctx, |ui| {
                let painter = ui.painter();

                // Draw touchpad boundary
                let boundary_width = self.dims.touchpad_max_extent_x * scale;
                let boundary_height = self.dims.touchpad_max_extent_y * scale;
                render::draw_touchpad_boundary(painter, corner, boundary_width, boundary_height);

                // Draw button indicators
                render::draw_button_indicators(
                    painter,
                    &self.buttons,
                    corner,
                    boundary_width,
                    boundary_height,
                );

                // Draw historical touch data (trails)
                for h in 0..self.trails.min(HISTORY_MAX) {
                    for (i, touch) in self.touch_history[h].iter().enumerate() {
                        if !touch.used {
                            continue;
                        }
                        render::draw_trail(painter, touch, i, corner, scale, cscale);
                    }
                }

                // Draw current touch data
                for (i, touch) in self.current_touches.iter().enumerate() {
                    if !touch.used {
                        continue;
                    }
                    render::draw_touch(painter, touch, i, corner, scale, cscale);
                }

                // Pump history: shift everything down by one, newest at [0]
                for h in (1..HISTORY_MAX).rev() {
                    self.touch_history[h] = self.touch_history[h - 1];
                }
                self.touch_history[0] = self.current_touches;

                // Draw status text
                let center =
                    egui::Pos2::new(self.dims.screen_width / 2.0, self.dims.screen_height / 2.0);

                let text = if self.grabbed {
                    "Press ESC to restore focus"
                } else {
                    "Press ENTER to grab touchpad"
                };

                // Choose font size based on available space
                let font_size = {
                    let large_font = egui::FontId::proportional(30.0);
                    let galley =
                        painter.layout_no_wrap(text.to_string(), large_font, egui::Color32::GRAY);
                    if galley.size().x + self.dims.margin * 2.0
                        > self.dims.touchpad_max_extent_x * scale
                    {
                        10.0
                    } else {
                        30.0
                    }
                };

                painter.text(
                    center,
                    egui::Align2::CENTER_CENTER,
                    text,
                    egui::FontId::proportional(font_size),
                    egui::Color32::GRAY,
                );
            });

        // Request continuous repaint for animation
        ctx.request_repaint();
    }
}
