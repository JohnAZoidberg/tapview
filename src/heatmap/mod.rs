pub mod backend;
pub mod chips;
pub mod discovery;
#[cfg(target_os = "linux")]
pub mod hidraw;
pub mod protocol;
#[cfg(target_os = "windows")]
pub mod windows_hid;

use std::io;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use std::path::Path;

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
///
/// The methods are `async` because the browser (WebHID) and Android (JNI)
/// transports are promise/worker based and the wasm main thread can never
/// block. Native implementations return ready futures; callers on native
/// threads drive them with `pollster::block_on`, which blocks exactly where
/// the ioctl used to. `async_fn_in_trait` is allowed deliberately: the
/// WebHID futures are `!Send`, so a `Send` bound would be wrong, and every
/// caller is generic over `D: HidDevice` rather than `dyn`.
#[allow(async_fn_in_trait)]
pub trait HidDevice {
    /// Send a SetFeature report. `buf[0]` must be the report ID.
    async fn set_feature(&self, buf: &[u8]) -> io::Result<()>;

    /// Send a GetFeature report. `buf[0]` must be set to the report ID before calling.
    /// Returns the number of bytes actually read.
    async fn get_feature(&self, buf: &mut [u8]) -> io::Result<usize>;
}

/// The native HID device type of this platform.
#[cfg(target_os = "linux")]
pub type PlatformHidDevice = hidraw::HidrawDevice;
#[cfg(target_os = "windows")]
pub type PlatformHidDevice = windows_hid::WinHidDevice;

/// Open the platform's HID device at `path` (a `/dev/hidraw*` node on
/// Linux, a HID device interface path on Windows).
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub fn open_platform_hid_device(path: &Path) -> io::Result<PlatformHidDevice> {
    PlatformHidDevice::open(path)
}
