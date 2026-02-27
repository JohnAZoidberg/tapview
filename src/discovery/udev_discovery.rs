use super::{DeviceDiscovery, DeviceInfo, DiscoveryError, Integration};
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
                let integration =
                    match device.property_value("ID_INPUT_TOUCHPAD_INTEGRATION") {
                        Some(v) if v == "internal" => Integration::Internal,
                        Some(v) if v == "external" => Integration::External,
                        _ => {
                            // systemd's 70-touchpad.rules skips devices without ID_BUS
                            // (e.g. I2C touchpads), so fall back to the bus type in the
                            // sysfs path. I2C and SMBus touchpads are always built-in.
                            if syspath.contains("/i2c-") || syspath.contains("/rmi4-") {
                                Integration::Internal
                            } else {
                                Integration::Unknown
                            }
                        }
                    };

                results.push(DeviceInfo {
                    devnode: PathBuf::from(devnode),
                    integration,
                });
            }
        }

        if results.is_empty() {
            return Err(DiscoveryError::NotFound);
        }

        // Sort so internal touchpads come first, then unknown, then external.
        results.sort_by_key(|d| match d.integration {
            Integration::Internal => 0,
            Integration::Unknown => 1,
            Integration::External => 2,
        });

        Ok(results)
    }
}
