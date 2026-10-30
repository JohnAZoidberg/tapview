use super::{DeviceDiscovery, DeviceInfo, DiscoveryError};

/// Apple trackpads through MultitouchSupport first (the built-in pad is the
/// one most runs want), then PTP pads on the HID stack (USB, Bluetooth).
pub struct MacDiscovery;

impl DeviceDiscovery for MacDiscovery {
    fn find_touchpads() -> Result<Vec<DeviceInfo>, DiscoveryError> {
        let mut found = crate::input::macos_mt::find_touchpads();
        found.extend(crate::hid::macos::find_ptp_touchpads()?);
        if found.is_empty() {
            return Err(DiscoveryError::NotFound);
        }
        Ok(found)
    }
}
