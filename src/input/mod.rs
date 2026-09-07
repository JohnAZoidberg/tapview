#[cfg(target_os = "linux")]
pub mod evdev_backend;
#[cfg(target_os = "linux")]
pub mod hidraw_backend;
#[cfg(target_os = "windows")]
pub mod windows_backend;

use crate::multitouch::{ButtonState, TouchData, MAX_TOUCH_POINTS};
use std::path::Path;
use std::sync::OnceLock;
use web_time::Instant;

/// One frame of touch data, as every backend produces it.
#[derive(Debug, Clone)]
pub struct TouchState {
    pub touches: [TouchData; MAX_TOUCH_POINTS],
    pub buttons: ButtonState,
    /// When the frame reached the host, in microseconds on a clock that is
    /// only meaningful for differences within one backend: kernel event
    /// stamps over evdev, [`host_now_us`] elsewhere. `None` if unknown.
    pub timestamp_us: Option<u64>,
    /// The pad's own clock for this frame in microseconds (PTP Scan Time,
    /// or the kernel's MSC_TIMESTAMP derived from it), unwrapped so it only
    /// grows while frames keep coming. `None` if the device reports none.
    pub scan_time_us: Option<u64>,
}

impl Default for TouchState {
    fn default() -> Self {
        Self {
            touches: [TouchData::default(); MAX_TOUCH_POINTS],
            buttons: ButtonState::default(),
            timestamp_us: None,
            scan_time_us: None,
        }
    }
}

/// Microseconds since the first call, on the process-wide monotonic clock
/// (`performance.now()` in the browser). For stamping frames as backends
/// receive them.
pub fn host_now_us() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
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
