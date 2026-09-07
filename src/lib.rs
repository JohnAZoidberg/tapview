//! Tapview - touchpad visualizer.
//!
//! Everything lives in this library; `src/main.rs` is a one-line shim onto
//! [`cli::main`]. Having a single module tree (rather than the binary
//! re-declaring modules privately) is what lets other front ends — an
//! Android `cdylib`, a browser build — run the same [`app::TapviewApp`].

/// The Android front end: device picker, session assembly, eframe entry.
#[cfg(target_os = "android")]
pub mod android;
/// The Android USB transport: HID reports over the Kotlin `UsbBridge` via
/// JNI, because Android has no `/dev/hidraw*`.
#[cfg(target_os = "android")]
pub mod android_hid;
pub mod app;
#[cfg(not(target_os = "android"))]
pub mod cli;
pub mod config;
pub mod dimensions;
pub mod discovery;
pub mod heatmap;
pub mod hid;
pub mod input;
#[cfg(target_os = "linux")]
pub mod libinput_backend;
pub mod libinput_state;
pub mod multitouch;
pub mod ptp;
pub mod recording;
pub mod render;
pub mod session;
#[cfg(target_os = "windows")]
pub mod windows_input_backend;

// Re-export commonly used types
pub use discovery::{DeviceDiscovery, DeviceInfo, DiscoveryError};
pub use heatmap::HeatmapFrame;
pub use input::{InputBackend, InputError, TouchState};
pub use multitouch::{TouchData, MAX_TOUCH_POINTS};
