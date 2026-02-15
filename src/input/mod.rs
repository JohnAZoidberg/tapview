#[cfg(target_os = "linux")]
pub mod evdev_backend;
#[cfg(target_os = "windows")]
pub mod windows_backend;

use crate::multitouch::{ButtonState, TouchData, MAX_TOUCH_POINTS};
use std::path::Path;

#[derive(Debug, Clone)]
pub struct TouchState {
    pub touches: [TouchData; MAX_TOUCH_POINTS],
    pub buttons: ButtonState,
}

impl Default for TouchState {
    fn default() -> Self {
        Self {
            touches: [TouchData::default(); MAX_TOUCH_POINTS],
            buttons: ButtonState::default(),
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum InputError {
    OpenFailed(String),
    GrabFailed(String),
    ReadError(String),
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InputError::OpenFailed(msg) => write!(f, "open failed: {}", msg),
            InputError::GrabFailed(msg) => write!(f, "grab failed: {}", msg),
            InputError::ReadError(msg) => write!(f, "read error: {}", msg),
        }
    }
}

impl std::error::Error for InputError {}

#[allow(dead_code)]
pub trait InputBackend: Send + 'static {
    fn open(device_path: &Path) -> Result<Self, InputError>
    where
        Self: Sized;
    fn grab(&mut self) -> Result<(), InputError>;
    fn ungrab(&mut self) -> Result<(), InputError>;
    fn poll_events(&mut self) -> Result<Option<TouchState>, InputError>;
}
