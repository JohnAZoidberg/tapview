// Public modules for library usage
pub mod discovery;
pub mod heatmap;
pub mod input;
pub mod multitouch;
pub mod recording;

// Re-export commonly used types
pub use discovery::{DeviceDiscovery, DeviceInfo, DiscoveryError};
pub use heatmap::HeatmapFrame;
pub use input::{InputBackend, InputError, TouchState};
pub use multitouch::{TouchData, MAX_TOUCH_POINTS};
