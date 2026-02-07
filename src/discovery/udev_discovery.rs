use super::{DeviceDiscovery, DeviceInfo, DiscoveryError};
use std::path::PathBuf;

pub struct UdevDiscovery;

impl DeviceDiscovery for UdevDiscovery {
    fn find_touchpads() -> Result<Vec<DeviceInfo>, DiscoveryError> {
        let mut enumerator =
            udev::Enumerator::new().map_err(|e| DiscoveryError::UdevError(e.to_string()))?;

        enumerator
            .match_subsystem("input")
            .map_err(|e| DiscoveryError::UdevError(e.to_string()))?;

        enumerator
            .match_property("ID_INPUT_TOUCHPAD", "1")
            .map_err(|e| DiscoveryError::UdevError(e.to_string()))?;

        let mut results = Vec::new();

        for device in enumerator
            .scan_devices()
            .map_err(|e| DiscoveryError::UdevError(e.to_string()))?
        {
            let syspath = device.syspath().to_string_lossy().to_string();
            if !syspath.contains("/event") {
                continue;
            }

            if let Some(devnode) = device.devnode() {
                results.push(DeviceInfo {
                    devnode: PathBuf::from(devnode),
                });
            }
        }

        if results.is_empty() {
            Err(DiscoveryError::NotFound)
        } else {
            Ok(results)
        }
    }
}
