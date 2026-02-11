#[cfg(target_os = "linux")]
use evdev::{AbsoluteAxisType, EventType, InputEvent, Key};

pub const MAX_TOUCH_POINTS: usize = 10;

#[derive(Clone, Copy, Debug, Default)]
pub struct ButtonState {
    pub left: bool,
    pub right: bool,
    pub middle: bool,
}

#[derive(Clone, Copy, Debug, Default)]
#[allow(dead_code)]
pub struct TouchData {
    pub used: bool,
    pub pressed: bool,
    pub pressed_double: bool,
    pub tracking_id: i32,
    pub position_x: i32,
    pub position_y: i32,
    pub pressure: i32,
    pub distance: i32,
    pub touch_major: i32,
    pub touch_minor: i32,
    pub width_major: i32,
    pub width_minor: i32,
    pub orientation: i32,
    pub tool_x: i32,
    pub tool_y: i32,
    pub tool_type: i32,
}

impl TouchData {
    #[allow(dead_code)]
    pub fn reset(&mut self) {
        *self = TouchData::default();
    }

    #[cfg(target_os = "linux")]
    fn set_used(&mut self) {
        self.used = true;
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum MTState {
    Loading,
    ReadReady,
    NeedsReset,
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct MTStateMachine {
    state: MTState,
    slot: Option<usize>,
    pub touches: [TouchData; MAX_TOUCH_POINTS],
    pub buttons: ButtonState,
}

#[cfg(target_os = "linux")]
impl Default for MTStateMachine {
    fn default() -> Self {
        Self {
            state: MTState::Loading,
            slot: None,
            touches: [TouchData::default(); MAX_TOUCH_POINTS],
            buttons: ButtonState::default(),
        }
    }
}

#[cfg(target_os = "linux")]
impl MTStateMachine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.state = MTState::Loading;
        self.slot = None;
        for t in &mut self.touches {
            t.used = false;
        }
    }

    pub fn process(&mut self, event: &InputEvent) {
        match event.event_type() {
            EventType::KEY => {
                let code = Key(event.code());
                match code {
                    Key::BTN_TOUCH => {
                        self.touches[0].pressed = event.value() == 1;
                    }
                    Key::BTN_TOOL_DOUBLETAP => {
                        self.touches[0].pressed_double = event.value() == 1;
                    }
                    Key::BTN_LEFT => {
                        self.buttons.left = event.value() == 1;
                    }
                    Key::BTN_RIGHT => {
                        self.buttons.right = event.value() == 1;
                    }
                    Key::BTN_MIDDLE => {
                        self.buttons.middle = event.value() == 1;
                    }
                    _ => {}
                }
            }
            EventType::ABSOLUTE => {
                match self.state {
                    MTState::Loading => {}
                    MTState::NeedsReset => self.reset(),
                    MTState::ReadReady => {}
                }

                let slot = self.slot.unwrap_or(0);
                let code = AbsoluteAxisType(event.code());
                let value = event.value();

                match code {
                    AbsoluteAxisType::ABS_MT_SLOT => {
                        if value >= 0 && (value as usize) < MAX_TOUCH_POINTS {
                            self.slot = Some(value as usize);
                            self.touches[value as usize].used = true;
                        }
                    }
                    AbsoluteAxisType::ABS_MT_TRACKING_ID => {
                        if value < 0 {
                            self.touches[slot].used = false;
                        } else {
                            self.touches[slot].tracking_id = value;
                        }
                    }
                    AbsoluteAxisType::ABS_MT_POSITION_X => {
                        self.touches[slot].set_used();
                        self.touches[slot].position_x = value;
                    }
                    AbsoluteAxisType::ABS_MT_POSITION_Y => {
                        self.touches[slot].set_used();
                        self.touches[slot].position_y = value;
                    }
                    AbsoluteAxisType::ABS_MT_PRESSURE => {
                        self.touches[slot].set_used();
                        self.touches[slot].pressure = value;
                    }
                    AbsoluteAxisType::ABS_MT_DISTANCE => {
                        self.touches[slot].set_used();
                        self.touches[slot].distance = value;
                    }
                    AbsoluteAxisType::ABS_MT_TOUCH_MAJOR => {
                        self.touches[slot].set_used();
                        self.touches[slot].touch_major = value;
                    }
                    AbsoluteAxisType::ABS_MT_TOUCH_MINOR => {
                        self.touches[slot].set_used();
                        self.touches[slot].touch_minor = value;
                    }
                    AbsoluteAxisType::ABS_MT_WIDTH_MAJOR => {
                        self.touches[slot].set_used();
                        self.touches[slot].width_major = value;
                    }
                    AbsoluteAxisType::ABS_MT_WIDTH_MINOR => {
                        self.touches[slot].set_used();
                        self.touches[slot].width_minor = value;
                    }
                    AbsoluteAxisType::ABS_MT_ORIENTATION => {
                        self.touches[slot].set_used();
                        self.touches[slot].orientation = value;
                    }
                    AbsoluteAxisType::ABS_MT_TOOL_X => {
                        self.touches[slot].set_used();
                        self.touches[slot].tool_x = value;
                    }
                    AbsoluteAxisType::ABS_MT_TOOL_Y => {
                        self.touches[slot].set_used();
                        self.touches[slot].tool_y = value;
                    }
                    AbsoluteAxisType::ABS_MT_TOOL_TYPE => {
                        self.touches[slot].set_used();
                        self.touches[slot].tool_type = value;
                    }
                    _ => {}
                }
            }
            EventType::MISC => {}
            EventType::SYNCHRONIZATION => {
                self.state = MTState::ReadReady;
            }
            _ => {}
        }
    }

    #[allow(dead_code)]
    pub fn is_read_ready(&self) -> bool {
        self.state == MTState::ReadReady
    }
}

#[cfg(target_os = "linux")]
pub fn print_event(event: &InputEvent) {
    let type_name = match event.event_type() {
        EventType::KEY => "EV_KEY",
        EventType::ABSOLUTE => "EV_ABS",
        EventType::MISC => "EV_MSC",
        EventType::SYNCHRONIZATION => "EV_SYN",
        _ => "EV_???",
    };
    let code_name = code_lookup(event.code());
    match code_name {
        Some(name) => eprintln!("  {}({}, {})", type_name, name, event.value()),
        None => eprintln!("  {}(0x{:X}, {})", type_name, event.code(), event.value()),
    }
}

#[cfg(target_os = "linux")]
fn code_lookup(code: u16) -> Option<&'static str> {
    match code {
        0x00 => Some("X"),
        0x01 => Some("Y"),
        0x2f => Some("SLOT"),
        0x30 => Some("TOUCH_MAJOR"),
        0x31 => Some("TOUCH_MINOR"),
        0x32 => Some("WIDTH_MAJOR"),
        0x33 => Some("WIDTH_MINOR"),
        0x34 => Some("ORIENTATION"),
        0x35 => Some("POSITION_X"),
        0x36 => Some("POSITION_Y"),
        0x37 => Some("TOOL_TYPE"),
        0x38 => Some("BLOB_ID"),
        0x39 => Some("TRACKING_ID"),
        0x3a => Some("PRESSURE"),
        0x3b => Some("DISTANCE"),
        0x3c => Some("TOOL_X"),
        0x3d => Some("TOOL_Y"),
        0x110 => Some("BTN_LEFT"),
        0x111 => Some("BTN_RIGHT"),
        0x112 => Some("BTN_MIDDLE"),
        0x145 => Some("BTN_TOOL_FINGER"),
        0x14a => Some("BTN_TOUCH"),
        0x14d => Some("BTN_TOOL_DOUBLETAP"),
        _ => None,
    }
}
