use super::{DeviceDiscovery, DeviceInfo, DiscoveryError, Integration};
use std::path::PathBuf;

fn read_input_ids(device: &udev::Device) -> (Option<u16>, Option<u16>) {
    // Try udev properties first (set for USB devices by usb_id builtin).
    let from_props = || -> Option<(u16, u16)> {
        let vid = u16::from_str_radix(device.property_value("ID_VENDOR_ID")?.to_str()?, 16).ok()?;
        let pid = u16::from_str_radix(device.property_value("ID_MODEL_ID")?.to_str()?, 16).ok()?;
        Some((vid, pid))
    };
    if let Some((v, p)) = from_props() {
        return (Some(v), Some(p));
    }

    // Fall back to sysfs id/vendor + id/product on the parent inputX device.
    // The eventX device's syspath is e.g. `.../input/input7/event6`; its parent
    // is `input7` which carries the id/ directory.
    let parent = match device.parent() {
        Some(p) => p,
        None => return (None, None),
    };
    let vid = parent
        .attribute_value("id/vendor")
        .and_then(|v| u16::from_str_radix(v.to_str()?, 16).ok());
    let pid = parent
        .attribute_value("id/product")
        .and_then(|v| u16::from_str_radix(v.to_str()?, 16).ok());
    (vid, pid)
}

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
                let integration = match device.property_value("ID_INPUT_TOUCHPAD_INTEGRATION") {
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

                // USB devices expose ID_VENDOR_ID/ID_MODEL_ID as udev properties.
                // I2C-HID devices don't, but the parent inputX device has the IDs
                // in its sysfs id/vendor and id/product attributes.
                let (vendor_id, product_id) = read_input_ids(&device);

                results.push(DeviceInfo {
                    devnode: PathBuf::from(devnode),
                    integration,
                    vendor_id,
                    product_id,
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
