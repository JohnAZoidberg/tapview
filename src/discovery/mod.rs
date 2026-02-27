#[cfg(target_os = "linux")]
pub mod udev_discovery;
#[cfg(target_os = "windows")]
pub mod windows_discovery;

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub devnode: PathBuf,
    /// Whether this is an internal (built-in) touchpad, external, or unknown.
    pub integration: Integration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Internal/External used on Linux only
pub enum Integration {
    Internal,
    External,
    Unknown,
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

impl std::fmt::Display for DeviceInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self.integration {
            Integration::Internal => " (internal)",
            Integration::External => " (external)",
            Integration::Unknown => "",
        };
        write!(f, "{}{}", self.devnode.display(), label)
    }
}

pub trait DeviceDiscovery {
    fn find_touchpads() -> Result<Vec<DeviceInfo>, DiscoveryError>;
}
