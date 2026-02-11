use crate::dimensions::Dimensions;
use crate::heatmap::{AlcCommand, HeatmapFrame};
use crate::input::TouchState;
use crate::libinput_backend::LibinputEvent;
use crate::libinput_state::LibinputState;
use crate::multitouch::{ButtonState, TouchData, MAX_TOUCH_POINTS};
use crate::render;
use std::sync::mpsc;

const HISTORY_MAX: usize = 20;
/// Number of heatmap mean values to keep for the time-series plot.
const HEATMAP_STATS_MAX: usize = 600;

pub enum GrabCommand {
    Grab,
    Ungrab,
}

pub struct TapviewApp {
    touch_rx: mpsc::Receiver<TouchState>,
    grab_tx: mpsc::Sender<GrabCommand>,
    libinput_rx: Option<mpsc::Receiver<LibinputEvent>>,
    heatmap_rx: Option<mpsc::Receiver<HeatmapFrame>>,
    alc_tx: Option<mpsc::Sender<AlcCommand>>,
    heatmap_frame: Option<HeatmapFrame>,
    /// Rolling buffer of per-frame raw mean values for time-series plot.
    heatmap_means: Vec<f64>,
    /// Rolling buffer of per-frame smoothed (EMA) means for trend line.
    heatmap_smoothed: Vec<f64>,
    alc_enabled: bool,
    dims: Dimensions,
    current_touches: [TouchData; MAX_TOUCH_POINTS],
    buttons: ButtonState,
    touch_history: Vec<[TouchData; MAX_TOUCH_POINTS]>,
    libinput: LibinputState,
    trails: usize,
    grabbed: bool,
}

impl TapviewApp {
    pub fn new(
        touch_rx: mpsc::Receiver<TouchState>,
        grab_tx: mpsc::Sender<GrabCommand>,
        libinput_rx: Option<mpsc::Receiver<LibinputEvent>>,
        heatmap_rx: Option<mpsc::Receiver<HeatmapFrame>>,
        alc_tx: Option<mpsc::Sender<AlcCommand>>,
        trails: usize,
    ) -> Self {
        Self {
            touch_rx,
            grab_tx,
            libinput_rx,
            heatmap_rx,
            alc_tx,
            heatmap_frame: None,
            heatmap_means: Vec::with_capacity(HEATMAP_STATS_MAX),
            heatmap_smoothed: Vec::with_capacity(HEATMAP_STATS_MAX),
            alc_enabled: true,
            dims: Dimensions::default(),
            current_touches: [TouchData::default(); MAX_TOUCH_POINTS],
            buttons: ButtonState::default(),
            touch_history: vec![[TouchData::default(); MAX_TOUCH_POINTS]; HISTORY_MAX],
            libinput: LibinputState::default(),
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

        // Drain and apply libinput events
        if let Some(rx) = &self.libinput_rx {
            while let Ok(event) = rx.try_recv() {
                self.libinput.apply_event(&event);
            }
        }

        // Drain heatmap frames, accumulate stats, keep only the latest for display
        if let Some(rx) = &self.heatmap_rx {
            while let Ok(frame) = rx.try_recv() {
                // Record stats for time-series
                if self.heatmap_means.len() >= HEATMAP_STATS_MAX {
                    let half = HEATMAP_STATS_MAX / 2;
                    self.heatmap_means.drain(..half);
                    self.heatmap_smoothed.drain(..half);
                }
                self.heatmap_means.push(frame.mean);
                self.heatmap_smoothed.push(frame.smoothed_mean);
                self.heatmap_frame = Some(frame);
            }
        }

        // Handle grab/ungrab keys
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Enter) && !self.grabbed {
                let _ = self.grab_tx.send(GrabCommand::Grab);
                self.grabbed = true;
            } else if i.key_pressed(egui::Key::Escape) && self.grabbed {
                let _ = self.grab_tx.send(GrabCommand::Ungrab);
                self.grabbed = false;
            }

            // ALC commands (only when heatmap is active)
            if let Some(tx) = &self.alc_tx {
                if i.key_pressed(egui::Key::R) {
                    let _ = tx.send(AlcCommand::Reset);
                }
                if i.key_pressed(egui::Key::A) {
                    if self.alc_enabled {
                        let _ = tx.send(AlcCommand::Disable);
                    } else {
                        let _ = tx.send(AlcCommand::Enable);
                    }
                    self.alc_enabled = !self.alc_enabled;
                }
            }
        });

        // Grow touchpad extents from current touches
        for touch in &self.current_touches {
            if touch.used {
                self.dims
                    .maybe_grow_touchpad_extent(touch.position_x as f32, touch.position_y as f32);
            }
        }

        // Show heatmap bottom panel if active
        if let Some(frame) = &self.heatmap_frame {
            let means = &self.heatmap_means;
            let smoothed = &self.heatmap_smoothed;
            let alc_enabled = self.alc_enabled;
            egui::TopBottomPanel::bottom("heatmap_panel")
                .default_height(200.0)
                .min_height(100.0)
                .show(ctx, |ui| {
                    render::draw_heatmap_panel(ui, frame, means, smoothed, alc_enabled);
                });
        }

        // Show libinput side panel if we have a receiver
        if self.libinput_rx.is_some() {
            egui::SidePanel::right("libinput_panel")
                .default_width(200.0)
                .min_width(150.0)
                .show(ctx, |ui| {
                    render::draw_libinput_panel(ui, &self.libinput);
                });
        }

        // Decay libinput values after rendering
        self.libinput.decay();

        // Update dimensions from central panel area
        let central_rect = ctx.available_rect();
        self.dims.screen_width = central_rect.width();
        self.dims.screen_height = central_rect.height();

        let scale = self.dims.get_touchpad_scale();
        let corner = self.dims.get_touchpad_corner(scale);
        let corner = egui::Pos2::new(corner.x + central_rect.min.x, corner.y + central_rect.min.y);
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
                let center = egui::Pos2::new(
                    central_rect.min.x + self.dims.screen_width / 2.0,
                    central_rect.min.y + self.dims.screen_height / 2.0,
                );

                let text = if self.grabbed {
                    "Press ESC to restore focus"
                } else if self.alc_tx.is_some() {
                    "ENTER=grab  R=ALC reset  A=ALC on/off"
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
