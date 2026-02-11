pub mod backend;
pub mod chips;
pub mod discovery;
#[cfg(target_os = "linux")]
pub mod hidraw;
pub mod protocol;
#[cfg(target_os = "windows")]
pub mod windows_hid;

use std::io;

/// A single frame of raw capacitive heatmap data.
#[derive(Clone)]
pub struct HeatmapFrame {
    pub rows: usize,
    pub cols: usize,
    /// Row-major signed 16-bit capacitance values (rows * cols elements).
    pub data: Vec<i16>,
}

/// Platform-independent trait for HID feature report I/O.
/// Implemented by `HidrawDevice` on Linux and `WinHidDevice` on Windows.
pub trait HidDevice {
    /// Send a SetFeature report. `buf[0]` must be the report ID.
    fn set_feature(&self, buf: &[u8]) -> io::Result<()>;

    /// Send a GetFeature report. `buf[0]` must be set to the report ID before calling.
    /// Returns the number of bytes actually read.
    fn get_feature(&self, buf: &mut [u8]) -> io::Result<usize>;
}
