use crate::dimensions::Dimensions;
use crate::heatmap::HeatmapFrame;
use crate::libinput_state::LibinputEvent;
use crate::libinput_state::LibinputState;
use crate::multitouch::{ButtonState, TouchData, MAX_TOUCH_POINTS};
use crate::recording::Recording;
use crate::render;
use crate::session::{GrabCommand, Session};
use std::sync::mpsc;
use web_time::Instant;

const HISTORY_MAX: usize = 20;

/// Something the no-session screen's controls produced.
pub enum SessionRequest {
    /// A device was opened: attach it.
    Attach(Box<Session>),
    /// Play a recording instead.
    Playback(Box<Recording>),
}

/// Platform controls drawn on the no-session screen; see [`NoSessionUi`].
pub type NoSessionControls = Box<dyn FnMut(&mut egui::Ui) -> Option<SessionRequest>>;

/// Runs at the start of every frame, before any panel: for platform work
/// such as reserving the system-bar insets on Android.
pub type FrameHook = Box<dyn FnMut(&egui::Context, &mut TapviewApp)>;

/// What the central panel shows while no device is open.
///
/// Natively a session always exists before the window opens, so this is only
/// a message. Front ends that connect devices at runtime (browser, Android)
/// supply `controls`: drawn under the message every frame, they hold their
/// own state (device lists, pending permission dialogs) and hand back a
/// [`SessionRequest`] once something is ready.
pub struct NoSessionUi {
    pub message: String,
    pub controls: Option<NoSessionControls>,
}

impl Default for NoSessionUi {
    fn default() -> Self {
        Self {
            message: "No touchpad".to_string(),
            controls: None,
        }
    }
}

/// Playback of a loaded recording.
struct Playback {
    recording: Recording,
    time: f64,
    speed: f32,
    playing: bool,
    last_wall: Option<Instant>,
}

impl Playback {
    fn new(recording: Recording) -> Self {
        Self {
            recording,
            time: 0.0,
            speed: 1.0,
            playing: false,
            last_wall: None,
        }
    }
}

pub struct TapviewApp {
    session: Option<Session>,
    no_session: NoSessionUi,
    libinput_rx: Option<mpsc::Receiver<LibinputEvent>>,
    libinput: LibinputState,
    heatmap_frame: Option<HeatmapFrame>,
    playback: Option<Playback>,
    dims: Dimensions,
    current_touches: [TouchData; MAX_TOUCH_POINTS],
    buttons: ButtonState,
    touch_history: Vec<[TouchData; MAX_TOUCH_POINTS]>,
    trails: usize,
    grabbed: bool,
    frame_hook: Option<FrameHook>,
}

impl TapviewApp {
    /// An app with no device, no playback and no pointer panel; add those
    /// with the `with_*` builders or later with [`attach_session`].
    ///
    /// [`attach_session`]: TapviewApp::attach_session
    pub fn new(trails: usize) -> Self {
        Self {
            session: None,
            no_session: NoSessionUi::default(),
            libinput_rx: None,
            libinput: LibinputState::default(),
            heatmap_frame: None,
            playback: None,
            dims: Dimensions::from_extents(None),
            current_touches: [TouchData::default(); MAX_TOUCH_POINTS],
            buttons: ButtonState::default(),
            touch_history: vec![[TouchData::default(); MAX_TOUCH_POINTS]; HISTORY_MAX],
            trails,
            grabbed: false,
            frame_hook: None,
        }
    }

    pub fn with_session(mut self, session: Session) -> Self {
        self.attach_session(session);
        self
    }

    pub fn with_playback(mut self, recording: Recording) -> Self {
        self.start_playback(recording);
        self
    }

    pub fn with_libinput(mut self, rx: Option<mpsc::Receiver<LibinputEvent>>) -> Self {
        self.libinput_rx = rx;
        self
    }

    pub fn with_no_session_ui(mut self, ui: NoSessionUi) -> Self {
        self.no_session = ui;
        self
    }

    pub fn with_frame_hook(mut self, hook: FrameHook) -> Self {
        self.frame_hook = Some(hook);
        self
    }

    /// Change what the no-session screen says.
    pub fn set_no_session_message(&mut self, message: impl Into<String>) {
        self.no_session.message = message.into();
    }

    pub fn has_session(&self) -> bool {
        self.session.is_some()
    }

    /// Start visualizing a device. Replaces any current session and resets
    /// the touchpad geometry to the new device's extents.
    pub fn attach_session(&mut self, session: Session) {
        self.dims = Dimensions::from_extents(session.extents);
        self.reset_touches();
        self.session = Some(session);
        self.grabbed = false;
    }

    /// Stop visualizing the current device. Dropping the session closes its
    /// channels, which ends the backend threads.
    pub fn detach_session(&mut self) -> Option<Session> {
        self.reset_touches();
        self.grabbed = false;
        self.session.take()
    }

    /// Play a recording (takes precedence over a live session while set).
    pub fn start_playback(&mut self, recording: Recording) {
        let extents = if recording.extent_x > 0 && recording.extent_y > 0 {
            Some((recording.extent_x, recording.extent_y))
        } else {
            None
        };
        self.dims = Dimensions::from_extents(extents);
        self.reset_touches();
        self.playback = Some(Playback::new(recording));
    }

    fn reset_touches(&mut self) {
        self.current_touches = [TouchData::default(); MAX_TOUCH_POINTS];
        self.buttons = ButtonState::default();
        for h in &mut self.touch_history {
            *h = [TouchData::default(); MAX_TOUCH_POINTS];
        }
        self.heatmap_frame = None;
    }
}

impl eframe::App for TapviewApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(mut hook) = self.frame_hook.take() {
            hook(ctx, self);
            self.frame_hook = Some(hook);
        }

        // A backend noticed the device going away: drop the session and say so.
        let lost = self
            .session
            .as_ref()
            .and_then(|s| s.lost.as_ref())
            .and_then(|rx| rx.try_recv().ok());
        if let Some(reason) = lost {
            log::error!("touchpad lost: {}", reason);
            self.detach_session();
            self.no_session.message = format!("Touchpad disconnected: {}", reason);
        }

        let is_playback = self.playback.is_some();

        if let Some(pb) = &mut self.playback {
            // --- Playback: advance time, look up frame ---
            Self::handle_playback_input(pb, ctx);

            let duration = pb.recording.duration_secs();

            if pb.playing {
                let now = Instant::now();
                if let Some(last) = pb.last_wall {
                    let wall_dt = now.duration_since(last).as_secs_f64();
                    pb.time += wall_dt * pb.speed as f64;
                }
                pb.last_wall = Some(now);

                // Auto-pause at end
                if pb.time >= duration {
                    pb.time = duration;
                    pb.playing = false;
                    pb.last_wall = None;
                }
            } else {
                pb.last_wall = None;
            }

            pb.time = pb.time.clamp(0.0, duration);

            // Look up frame
            if let Some(frame) = pb.recording.frame_at(pb.time) {
                self.current_touches = frame.state.touches;
                self.buttons = frame.state.buttons;
            }
        } else if let Some(session) = &mut self.session {
            // --- Live mode: drain touch events ---
            while let Ok(state) = session.touch_rx.try_recv() {
                self.current_touches = state.touches;
                self.buttons = state.buttons;

                // Record each frame
                if let Some(recorder) = &mut session.recorder {
                    if let Err(e) = recorder.record(&state) {
                        log::error!("Recording error: {}", e);
                        session.recorder = None;
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
        if let Some(rx) = self.session.as_ref().and_then(|s| s.heatmap_rx.as_ref()) {
            while let Ok(frame) = rx.try_recv() {
                self.heatmap_frame = Some(frame);
            }
        }

        // Grab/ungrab keys, where the backend supports grabbing
        if !is_playback {
            if let Some(grab_tx) = self.session.as_ref().and_then(|s| s.grab_tx.as_ref()) {
                ctx.input(|i| {
                    if i.key_pressed(egui::Key::Enter) && !self.grabbed {
                        let _ = grab_tx.send(GrabCommand::Grab);
                        self.grabbed = true;
                    } else if i.key_pressed(egui::Key::Escape) && self.grabbed {
                        let _ = grab_tx.send(GrabCommand::Ungrab);
                        self.grabbed = false;
                    }
                });
            }
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

        // A portrait viewport (a phone) stacks everything vertically: the
        // touchpad on top, the heatmap under it at its own aspect ratio, and
        // the config panel as a scrollable strip at the bottom. Side panels
        // would leave the pad a sliver.
        let screen = ctx.screen_rect();
        let portrait = screen.height() > screen.width();

        // Show config panel if available (bottom panels added first sit
        // outermost, so in portrait this is the very bottom)
        if let Some(config) = self.session.as_mut().and_then(|s| s.config.as_mut()) {
            // Apply results (and reverts) of writes the worker finished
            config.pump();
            if portrait {
                egui::TopBottomPanel::bottom("config_panel")
                    .resizable(true)
                    .default_height((screen.height() * 0.22).clamp(120.0, 260.0))
                    .max_height(screen.height() * 0.5)
                    .show(ctx, |ui| {
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            render::draw_config_panel(ui, config);
                        });
                    });
            } else {
                egui::SidePanel::left("config_panel")
                    .default_width(200.0)
                    .min_width(160.0)
                    .show(ctx, |ui| {
                        render::draw_config_panel(ui, config);
                    });
            }
        }

        // Show heatmap bottom panel if active
        if let Some(frame) = &self.heatmap_frame {
            let mut panel = egui::TopBottomPanel::bottom("heatmap_panel")
                .default_height(200.0)
                .min_height(100.0);
            if portrait && frame.cols > 0 {
                // Full width at the sensor matrix's aspect ratio, capped so
                // the touchpad keeps at least half the screen
                let height = screen.width() * frame.rows as f32 / frame.cols as f32 + 30.0;
                panel = panel.exact_height(height.min(screen.height() * 0.35));
            }
            panel.show(ctx, |ui| {
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
        if let Some(pb) = &mut self.playback {
            Self::draw_playback_panel(pb, ctx);
        }

        // Nothing to visualize: the connect screen
        if !is_playback && self.session.is_none() {
            let mut request = None;
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE.fill(egui::Color32::WHITE))
                .show(ctx, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(ui.available_height() * 0.3);
                        ui.label(
                            egui::RichText::new(&self.no_session.message)
                                .size(24.0)
                                .color(egui::Color32::GRAY),
                        );
                        ui.add_space(16.0);
                        if let Some(controls) = &mut self.no_session.controls {
                            request = controls(ui);
                        }
                    });
                });
            match request {
                Some(SessionRequest::Attach(session)) => self.attach_session(*session),
                Some(SessionRequest::Playback(rec)) => self.start_playback(*rec),
                None => {}
            }
            ctx.request_repaint();
            return;
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

                // Which device this is, in the corner
                if let Some(session) = &self.session {
                    painter.text(
                        central_rect.min + egui::vec2(6.0, 4.0),
                        egui::Align2::LEFT_TOP,
                        &session.name,
                        egui::FontId::proportional(12.0),
                        egui::Color32::GRAY,
                    );
                }

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
                } else {
                    match &self.session {
                        Some(s) if s.recorder.is_some() => "Recording... (touch the pad)",
                        Some(s) if s.grab_tx.is_some() => {
                            if self.grabbed {
                                "Press ESC to restore focus"
                            } else {
                                "Press ENTER to grab touchpad"
                            }
                        }
                        Some(_) => "Touch the touchpad to visualize",
                        None => unreachable!("no-session screen returned above"),
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
    fn handle_playback_input(pb: &mut Playback, ctx: &egui::Context) {
        ctx.input(|i| {
            if i.key_pressed(egui::Key::Space) {
                pb.playing = !pb.playing;
                // If at end and pressing play, restart
                if pb.playing {
                    let duration = pb.recording.duration_secs();
                    if pb.time >= duration {
                        pb.time = 0.0;
                    }
                }
            }
            if i.key_pressed(egui::Key::ArrowLeft) {
                pb.time = (pb.time - 0.1).max(0.0);
            }
            if i.key_pressed(egui::Key::ArrowRight) {
                let duration = pb.recording.duration_secs();
                pb.time = (pb.time + 0.1).min(duration);
            }
        });
    }

    fn draw_playback_panel(pb: &mut Playback, ctx: &egui::Context) {
        let duration = pb.recording.duration_secs();

        egui::TopBottomPanel::bottom("playback_panel")
            .exact_height(48.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    // Play/Pause button
                    let label = if pb.playing { "Pause" } else { "Play" };
                    if ui.button(label).clicked() {
                        pb.playing = !pb.playing;
                        if pb.playing && pb.time >= duration {
                            pb.time = 0.0;
                        }
                    }

                    ui.separator();

                    // Speed buttons
                    for &speed in &[0.25f32, 0.5, 1.0, 2.0] {
                        let text = format!("{}x", speed);
                        let btn =
                            egui::Button::new(&text).selected((pb.speed - speed).abs() < 0.01);
                        if ui.add(btn).clicked() {
                            pb.speed = speed;
                        }
                    }

                    ui.separator();

                    // Timestamp
                    ui.label(format!("{:.1}s / {:.1}s", pb.time, duration));

                    // Timeline slider (takes remaining width)
                    let mut t = pb.time as f32;
                    let slider = egui::Slider::new(&mut t, 0.0..=(duration as f32))
                        .show_value(false)
                        .trailing_fill(true);
                    let response = ui.add(slider);
                    if response.dragged() || response.changed() {
                        pb.time = t as f64;
                        // Pause while dragging
                        if response.dragged() {
                            pb.playing = false;
                        }
                    }
                });
            });
    }
}
