#[cfg(target_os = "linux")]
pub mod udev_discovery;
#[cfg(target_os = "windows")]
pub mod windows_discovery;

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub devnode: PathBuf,
}

#[derive(Debug)]
pub enum DiscoveryError {
    UdevError(String),
    NotFound,
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiscoveryError::UdevError(msg) => write!(f, "udev error: {}", msg),
            DiscoveryError::NotFound => write!(f, "no touchpad found"),
        }
    }
}

impl std::error::Error for DiscoveryError {}

pub trait DeviceDiscovery {
    fn find_touchpads() -> Result<Vec<DeviceInfo>, DiscoveryError>;
}
