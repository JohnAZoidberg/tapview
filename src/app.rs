use crate::config::PtpConfig;
use crate::dimensions::Dimensions;
use crate::heatmap::HeatmapFrame;
use crate::input::TouchState;
use crate::libinput_state::LibinputEvent;
use crate::libinput_state::LibinputState;
use crate::multitouch::{ButtonState, TouchData, MAX_TOUCH_POINTS};
use crate::recording::{Recorder, Recording};
use crate::render;
use std::sync::mpsc;
use std::time::Instant;

const HISTORY_MAX: usize = 20;

#[allow(dead_code)]
pub enum GrabCommand {
    Grab,
    Ungrab,
}

pub struct TapviewApp {
    touch_rx: mpsc::Receiver<TouchState>,
    #[allow(dead_code)]
    grab_tx: mpsc::Sender<GrabCommand>,
    libinput_rx: Option<mpsc::Receiver<LibinputEvent>>,
    heatmap_rx: Option<mpsc::Receiver<HeatmapFrame>>,
    heatmap_frame: Option<HeatmapFrame>,
    ptp_config: Option<PtpConfig>,
    dims: Dimensions,
    current_touches: [TouchData; MAX_TOUCH_POINTS],
    buttons: ButtonState,
    touch_history: Vec<[TouchData; MAX_TOUCH_POINTS]>,
    libinput: LibinputState,
    trails: usize,
    #[allow(dead_code)]
    grabbed: bool,
    // Recording
    recorder: Option<Recorder>,
    // Playback
    recording: Option<Recording>,
    playback_time: f64,
    playback_speed: f32,
    playback_playing: bool,
    playback_last_wall: Option<Instant>,
}

impl TapviewApp {
    pub fn new(
        touch_rx: mpsc::Receiver<TouchState>,
        grab_tx: mpsc::Sender<GrabCommand>,
        libinput_rx: Option<mpsc::Receiver<LibinputEvent>>,
        heatmap_rx: Option<mpsc::Receiver<HeatmapFrame>>,
        ptp_config: Option<PtpConfig>,
        evdev_extents: Option<(i32, i32)>,
        trails: usize,
        recorder: Option<Recorder>,
        recording: Option<Recording>,
    ) -> Self {
        Self {
            touch_rx,
            grab_tx,
            libinput_rx,
            heatmap_rx,
            heatmap_frame: None,
            dims: Dimensions::from_extents(evdev_extents),
            ptp_config,
            current_touches: [TouchData::default(); MAX_TOUCH_POINTS],
            buttons: ButtonState::default(),
            touch_history: vec![[TouchData::default(); MAX_TOUCH_POINTS]; HISTORY_MAX],
            libinput: LibinputState::default(),
            trails,
            grabbed: false,
            recorder,
            recording,
            playback_time: 0.0,
            playback_speed: 1.0,
            playback_playing: false,
            playback_last_wall: None,
        }
    }
}

impl eframe::App for TapviewApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let is_playback = self.recording.is_some();

        if is_playback {
            // --- Playback: advance time, look up frame ---
            self.handle_playback_input(ctx);

            let duration = self.recording.as_ref().unwrap().duration_secs();

            if self.playback_playing {
                let now = Instant::now();
                if let Some(last) = self.playback_last_wall {
                    let wall_dt = now.duration_since(last).as_secs_f64();
                    self.playback_time += wall_dt * self.playback_speed as f64;
                }
                self.playback_last_wall = Some(now);

                // Auto-pause at end
                if self.playback_time >= duration {
                    self.playback_time = duration;
                    self.playback_playing = false;
                    self.playback_last_wall = None;
                }
            } else {
                self.playback_last_wall = None;
            }

            self.playback_time = self.playback_time.clamp(0.0, duration);

            // Look up frame
            if let Some(frame) = self
                .recording
                .as_ref()
                .unwrap()
                .frame_at(self.playback_time)
            {
                self.current_touches = frame.state.touches;
                self.buttons = frame.state.buttons;
            }
        } else {
            // --- Live mode: drain touch events ---
            while let Ok(state) = self.touch_rx.try_recv() {
                self.current_touches = state.touches;
                self.buttons = state.buttons;

                // Record each frame
                if let Some(ref mut recorder) = self.recorder {
                    if let Err(e) = recorder.record(&state) {
                        eprintln!("Recording error: {}", e);
                        self.recorder = None;
                    }
                }
            }
        }

        // Drain and apply libinput events
        if let Some(rx) = &self.libinput_rx {
            while let Ok(event) = rx.try_recv() {
                self.libinput.apply_event(&event);
            }
        }

        // Drain heatmap frames, keep only the latest
        if let Some(rx) = &self.heatmap_rx {
            while let Ok(frame) = rx.try_recv() {
                self.heatmap_frame = Some(frame);
            }
        }

        // Handle grab/ungrab keys (Linux only — Windows doesn't support touchpad grab)
        #[cfg(target_os = "linux")]
        if !is_playback {
            ctx.input(|i| {
                if i.key_pressed(egui::Key::Enter) && !self.grabbed {
                    let _ = self.grab_tx.send(GrabCommand::Grab);
                    self.grabbed = true;
                } else if i.key_pressed(egui::Key::Escape) && self.grabbed {
                    let _ = self.grab_tx.send(GrabCommand::Ungrab);
                    self.grabbed = false;
                }
            });
        }

        // Grow touchpad extents from current touches (only when the
        // descriptor didn't provide a logical range).
        if !self.dims.extent_known {
            for touch in &self.current_touches {
                if touch.used {
                    self.dims.maybe_grow_touchpad_extent(
                        touch.position_x as f32,
                        touch.position_y as f32,
                    );
                }
            }
        }

        // Show config left panel if available
        if let Some(config) = &mut self.ptp_config {
            egui::SidePanel::left("config_panel")
                .default_width(200.0)
                .min_width(160.0)
                .show(ctx, |ui| {
                    render::draw_config_panel(ui, config);
                });
        }

        // Show heatmap bottom panel if active
        if let Some(frame) = &self.heatmap_frame {
            egui::TopBottomPanel::bottom("heatmap_panel")
                .default_height(200.0)
                .min_height(100.0)
                .show(ctx, |ui| {
                    render::draw_heatmap_panel(ui, frame);
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

        // Show playback controls panel if in playback mode
        if is_playback {
            self.draw_playback_panel(ctx);
        }

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

                let text = if is_playback {
                    "Space: play/pause, Left/Right: step"
                } else if self.recorder.is_some() {
                    "Recording... (touch the pad)"
                } else {
                    #[cfg(target_os = "linux")]
                    {
                        if self.grabbed {
                            "Press ESC to restore focus"
                        } else {
                            "Press ENTER to grab touchpad"
                        }
                    }
                    #[cfg(target_os = "windows")]
                    {
                        "Touch the touchpad to visualize"
                    }
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

impl TapviewApp {
    fn handle_playback_input(&mut self, ctx: &egui::Context) {
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Space) {
                self.playback_playing = !self.playback_playing;
                // If at end and pressing play, restart
                if self.playback_playing {
                    let duration = self.recording.as_ref().unwrap().duration_secs();
                    if self.playback_time >= duration {
                        self.playback_time = 0.0;
                    }
                }
            }
            if i.key_pressed(egui::Key::ArrowLeft) {
                self.playback_time = (self.playback_time - 0.1).max(0.0);
            }
            if i.key_pressed(egui::Key::ArrowRight) {
                let duration = self.recording.as_ref().unwrap().duration_secs();
                self.playback_time = (self.playback_time + 0.1).min(duration);
            }
        });
    }

    fn draw_playback_panel(&mut self, ctx: &egui::Context) {
        let duration = self.recording.as_ref().unwrap().duration_secs();

        egui::TopBottomPanel::bottom("playback_panel")
            .exact_height(48.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    // Play/Pause button
                    let label = if self.playback_playing {
                        "Pause"
                    } else {
                        "Play"
                    };
                    if ui.button(label).clicked() {
                        self.playback_playing = !self.playback_playing;
                        if self.playback_playing && self.playback_time >= duration {
                            self.playback_time = 0.0;
                        }
                    }

                    ui.separator();

                    // Speed buttons
                    for &speed in &[0.25f32, 0.5, 1.0, 2.0] {
                        let text = format!("{}x", speed);
                        let btn = egui::Button::new(&text)
                            .selected((self.playback_speed - speed).abs() < 0.01);
                        if ui.add(btn).clicked() {
                            self.playback_speed = speed;
                        }
                    }

                    ui.separator();

                    // Timestamp
                    let current = self.playback_time;
                    ui.label(format!("{:.1}s / {:.1}s", current, duration));

                    // Timeline slider (takes remaining width)
                    let mut t = self.playback_time as f32;
                    let slider = egui::Slider::new(&mut t, 0.0..=(duration as f32))
                        .show_value(false)
                        .trailing_fill(true);
                    let response = ui.add(slider);
                    if response.dragged() || response.changed() {
                        self.playback_time = t as f64;
                        // Pause while dragging
                        if response.dragged() {
                            self.playback_playing = false;
                        }
                    }
                });
            });
    }
}
