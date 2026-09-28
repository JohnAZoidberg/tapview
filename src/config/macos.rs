//! macOS: PTP configuration on the touchpad's own IOHIDDevice, with the
//! descriptor read from IOKit. All the actual work is in
//! [`LayoutConfigBackend`]; this file only opens the device.

use super::layout_backend::{
    touchpad_physical_size, LayoutConfigBackend, PtpFields, KEY_BUTTON_PRESS_THRESHOLD,
    KEY_HAPTIC_INTENSITY,
};
use super::{ConfigDescription, Discovered};
use crate::hid::macos::MacHidDevice;
use crate::hid::ReportLayout;
use std::path::Path;

pub fn discover(path: &Path) -> Option<Discovered> {
    // Apple pads have no PTP feature reports.
    if crate::input::macos_mt::is_mt_path(path) {
        return None;
    }
    let device = match MacHidDevice::open(path) {
        Ok(d) => d,
        Err(e) => {
            log::error!("config: {}", e);
            return None;
        }
    };
    let desc = match device.report_descriptor() {
        Ok(d) => d,
        Err(e) => {
            log::error!("config: failed to read report descriptor: {}", e);
            return None;
        }
    };

    let layout = ReportLayout::parse(&desc);
    let fields = PtpFields::from_layout(&layout);
    if fields.is_empty() {
        return None;
    }

    log::info!("config: found PTP features on {}", path.display());

    Some(Discovered {
        description: ConfigDescription {
            features: fields.features(),
            button_press_threshold_range: fields.range(KEY_BUTTON_PRESS_THRESHOLD),
            haptic_intensity_range: fields.range(KEY_HAPTIC_INTENSITY),
            physical_size: touchpad_physical_size(&layout),
        },
        backend: LayoutConfigBackend::new(device, fields)?,
    })
}
