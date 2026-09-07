//! An open touchpad, bundled: everything the visualizer reads from one device.
//!
//! The CLI builds a [`Session`] before eframe starts; the browser and Android
//! front ends build one when the user connects a device, and the app can run
//! without one (showing a connect screen) or drop it when the device goes
//! away. The libinput/pointer panel and recording playback live outside the
//! session: they are not tied to a device.

use crate::config::ConfigHandle;
use crate::heatmap::HeatmapFrame;
use crate::input::TouchState;
use crate::recording::Recorder;
use std::sync::mpsc;

/// Ask the input backend to take or release exclusive hold of the device.
pub enum GrabCommand {
    Grab,
    Ungrab,
}

pub struct Session {
    /// Touch frames from the input backend.
    pub touch_rx: mpsc::Receiver<TouchState>,
    /// `Some` where the backend can grab the device (Linux evdev); `None`
    /// where it cannot (Windows RawInput, raw HID transports), which also
    /// hides the grab UI.
    pub grab_tx: Option<mpsc::Sender<GrabCommand>>,
    /// Raw capacitive frames, when the heatmap backend is running.
    pub heatmap_rx: Option<mpsc::Receiver<HeatmapFrame>>,
    /// PTP configuration, when the device exposes it.
    pub config: Option<ConfigHandle>,
    /// Axis extents (x_max, y_max), when known from the device; otherwise the
    /// visualizer grows them from the touches it sees.
    pub extents: Option<(i32, i32)>,
    /// Records every touch frame while set.
    pub recorder: Option<Recorder>,
    /// Backends that can notice the device going away (a USB unplug) report
    /// it here; the app then drops the session and says why.
    pub lost: Option<mpsc::Receiver<String>>,
}
