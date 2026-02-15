//! State from libinput/interpreted input events for visualization.
//! Values decay each frame so visualizations fade when idle.
//!
//! The event types are defined here (rather than in the backend modules) so
//! they can be shared across Linux (libinput) and Windows (RawInput mouse)
//! backends.

/// Structured input event data, safe to send across threads.
/// On Linux these come from libinput; on Windows from RawInput mouse data.
#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum LibinputEvent {
    PointerMotion {
        dx: f64,
        dy: f64,
        dx_unaccel: f64,
        dy_unaccel: f64,
    },
    PointerButton {
        button: u32,
        pressed: bool,
    },
    Scroll {
        source: ScrollSource,
        vert: f64,
        horiz: f64,
    },
    GestureSwipeBegin {
        fingers: i32,
    },
    GestureSwipeUpdate {
        fingers: i32,
        dx: f64,
        dy: f64,
        dx_unaccel: f64,
        dy_unaccel: f64,
    },
    GestureSwipeEnd,
    GesturePinchBegin {
        fingers: i32,
    },
    GesturePinchUpdate {
        fingers: i32,
        dx: f64,
        dy: f64,
        dx_unaccel: f64,
        dy_unaccel: f64,
        scale: f64,
        angle: f64,
    },
    GesturePinchEnd,
    GestureHoldBegin {
        fingers: i32,
    },
    GestureHoldEnd {
        cancelled: bool,
    },
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub enum ScrollSource {
    Wheel,
    Finger,
    Continuous,
}

const DECAY: f32 = 0.85;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

#[derive(Clone, Debug, Default)]
pub struct LibinputState {
    /// Accelerated pointer motion dx/dy
    pub motion_accel: (f32, f32),
    /// Unaccelerated pointer motion dx/dy
    pub motion_unaccel: (f32, f32),

    /// Button state (from taps and physical clicks)
    pub buttons: LibinputButtons,

    /// Scroll vertical/horizontal
    pub scroll_vert: f32,
    pub scroll_horiz: f32,
    /// Scroll source: "finger", "wheel", "continuous"
    pub scroll_source: String,

    /// Gesture type currently active
    pub gesture: GestureState,

    /// Recent log lines (kept for small text log)
    pub log_lines: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct LibinputButtons {
    pub left: f32,
    pub middle: f32,
    pub right: f32,
}

#[derive(Clone, Debug, Default)]
pub struct GestureState {
    pub active: bool,
    pub kind: GestureKind,
    pub fingers: u32,
    pub dx: f32,
    pub dy: f32,
    pub dx_unaccel: f32,
    pub dy_unaccel: f32,
    /// Pinch scale factor (1.0 = no change)
    pub scale: f32,
    /// Pinch angle delta
    pub angle: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum GestureKind {
    #[default]
    None,
    Swipe,
    Pinch,
    Hold,
}

const LOG_MAX: usize = 50;

impl LibinputState {
    /// Apply per-frame decay to all values
    pub fn decay(&mut self) {
        self.motion_accel.0 *= DECAY;
        self.motion_accel.1 *= DECAY;
        self.motion_unaccel.0 *= DECAY;
        self.motion_unaccel.1 *= DECAY;
        self.buttons.left *= DECAY;
        self.buttons.middle *= DECAY;
        self.buttons.right *= DECAY;
        self.scroll_vert *= DECAY;
        self.scroll_horiz *= DECAY;

        if self.gesture.active {
            self.gesture.dx *= DECAY;
            self.gesture.dy *= DECAY;
            self.gesture.dx_unaccel *= DECAY;
            self.gesture.dy_unaccel *= DECAY;
            self.gesture.scale = 1.0 + (self.gesture.scale - 1.0) * DECAY;
            self.gesture.angle *= DECAY;
        }
    }

    /// Apply a structured libinput event to the state.
    pub fn apply_event(&mut self, event: &LibinputEvent) {
        self.push_log(format_event(event));

        match event {
            LibinputEvent::PointerMotion {
                dx,
                dy,
                dx_unaccel,
                dy_unaccel,
            } => {
                self.motion_accel = (*dx as f32, *dy as f32);
                self.motion_unaccel = (*dx_unaccel as f32, *dy_unaccel as f32);
            }
            LibinputEvent::PointerButton { button, pressed } => {
                let val = if *pressed { 1.0 } else { 0.0 };
                match *button {
                    BTN_LEFT => self.buttons.left = val,
                    BTN_RIGHT => self.buttons.right = val,
                    BTN_MIDDLE => self.buttons.middle = val,
                    _ => {}
                }
            }
            LibinputEvent::Scroll {
                source,
                vert,
                horiz,
            } => {
                self.scroll_source = match source {
                    ScrollSource::Wheel => "wheel".to_string(),
                    ScrollSource::Finger => "finger".to_string(),
                    ScrollSource::Continuous => "continuous".to_string(),
                };
                self.scroll_vert = *vert as f32;
                self.scroll_horiz = *horiz as f32;
            }
            LibinputEvent::GestureSwipeBegin { fingers } => {
                self.gesture.active = true;
                self.gesture.kind = GestureKind::Swipe;
                self.gesture.fingers = *fingers as u32;
                self.gesture.scale = 1.0;
                self.gesture.angle = 0.0;
            }
            LibinputEvent::GestureSwipeUpdate {
                fingers,
                dx,
                dy,
                dx_unaccel,
                dy_unaccel,
            } => {
                self.gesture.fingers = *fingers as u32;
                self.gesture.dx = *dx as f32;
                self.gesture.dy = *dy as f32;
                self.gesture.dx_unaccel = *dx_unaccel as f32;
                self.gesture.dy_unaccel = *dy_unaccel as f32;
            }
            LibinputEvent::GestureSwipeEnd => {
                self.gesture.active = false;
                self.gesture.kind = GestureKind::None;
            }
            LibinputEvent::GesturePinchBegin { fingers } => {
                self.gesture.active = true;
                self.gesture.kind = GestureKind::Pinch;
                self.gesture.fingers = *fingers as u32;
                self.gesture.scale = 1.0;
                self.gesture.angle = 0.0;
            }
            LibinputEvent::GesturePinchUpdate {
                fingers,
                dx,
                dy,
                dx_unaccel,
                dy_unaccel,
                scale,
                angle,
            } => {
                self.gesture.fingers = *fingers as u32;
                self.gesture.dx = *dx as f32;
                self.gesture.dy = *dy as f32;
                self.gesture.dx_unaccel = *dx_unaccel as f32;
                self.gesture.dy_unaccel = *dy_unaccel as f32;
                self.gesture.scale = *scale as f32;
                self.gesture.angle = *angle as f32;
            }
            LibinputEvent::GesturePinchEnd => {
                self.gesture.active = false;
                self.gesture.kind = GestureKind::None;
            }
            LibinputEvent::GestureHoldBegin { fingers } => {
                self.gesture.active = true;
                self.gesture.kind = GestureKind::Hold;
                self.gesture.fingers = *fingers as u32;
            }
            LibinputEvent::GestureHoldEnd { .. } => {
                self.gesture.active = false;
                self.gesture.kind = GestureKind::None;
            }
        }
    }

    fn push_log(&mut self, line: String) {
        self.log_lines.push(line);
        if self.log_lines.len() > LOG_MAX {
            self.log_lines.remove(0);
        }
    }
}

/// Format a LibinputEvent into a human-readable log line.
fn format_event(event: &LibinputEvent) -> String {
    match event {
        LibinputEvent::PointerMotion {
            dx,
            dy,
            dx_unaccel,
            dy_unaccel,
        } => {
            format!(
                "MOTION {:.2}/{:.2} ({:+.2}/{:+.2})",
                dx, dy, dx_unaccel, dy_unaccel
            )
        }
        LibinputEvent::PointerButton { button, pressed } => {
            let name = match *button {
                BTN_LEFT => "LEFT",
                BTN_RIGHT => "RIGHT",
                BTN_MIDDLE => "MIDDLE",
                _ => "?",
            };
            let state = if *pressed { "pressed" } else { "released" };
            format!("BUTTON {} {}", name, state)
        }
        LibinputEvent::Scroll {
            source,
            vert,
            horiz,
        } => {
            let src = match source {
                ScrollSource::Wheel => "wheel",
                ScrollSource::Finger => "finger",
                ScrollSource::Continuous => "continuous",
            };
            format!("SCROLL_{} v:{:.2} h:{:.2}", src, vert, horiz)
        }
        LibinputEvent::GestureSwipeBegin { fingers } => {
            format!("SWIPE_BEGIN {}f", fingers)
        }
        LibinputEvent::GestureSwipeUpdate {
            fingers,
            dx,
            dy,
            dx_unaccel,
            dy_unaccel,
        } => {
            format!(
                "SWIPE_UPDATE {}f {:.2}/{:.2} ({:+.2}/{:+.2})",
                fingers, dx, dy, dx_unaccel, dy_unaccel
            )
        }
        LibinputEvent::GestureSwipeEnd => "SWIPE_END".to_string(),
        LibinputEvent::GesturePinchBegin { fingers } => {
            format!("PINCH_BEGIN {}f", fingers)
        }
        LibinputEvent::GesturePinchUpdate {
            fingers,
            dx,
            dy,
            dx_unaccel,
            dy_unaccel,
            scale,
            angle,
        } => {
            format!(
                "PINCH_UPDATE {}f {:.2}/{:.2} ({:+.2}/{:+.2}) s:{:.2} a:{:.1}",
                fingers, dx, dy, dx_unaccel, dy_unaccel, scale, angle
            )
        }
        LibinputEvent::GesturePinchEnd => "PINCH_END".to_string(),
        LibinputEvent::GestureHoldBegin { fingers } => {
            format!("HOLD_BEGIN {}f", fingers)
        }
        LibinputEvent::GestureHoldEnd { cancelled } => {
            if *cancelled {
                "HOLD_END (cancelled)".to_string()
            } else {
                "HOLD_END".to_string()
            }
        }
    }
}
