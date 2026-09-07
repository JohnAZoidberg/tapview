//! Linux: PTP configuration over the touchpad's sibling hidraw node, with the
//! descriptor read from sysfs. All the actual work is in
//! [`LayoutConfigBackend`]; this file only finds and opens the device.

use super::layout_backend::{
    touchpad_physical_size, LayoutConfigBackend, PtpFields, KEY_BUTTON_PRESS_THRESHOLD,
    KEY_HAPTIC_INTENSITY,
};
use super::{ConfigDescription, Discovered};
use crate::heatmap::discovery::find_sibling_hidraw;
use crate::heatmap::hidraw::HidrawDevice;
use crate::hid::ReportLayout;
use std::path::Path;

pub fn discover(evdev_path: &Path) -> Option<Discovered> {
    let hidraw_path = match find_sibling_hidraw(evdev_path) {
        Ok(p) => p,
        Err(e) => {
            log::error!("config: failed to find hidraw device: {}", e);
            return None;
        }
    };

    let desc = match crate::hid::linux::read_report_descriptor(&hidraw_path) {
        Ok(d) => d,
        Err(e) => {
            log::error!("config: failed to read report descriptor: {}", e);
            return None;
        }
    };

    let layout = ReportLayout::parse(&desc);
    let fields = PtpFields::from_layout(&layout);
    let physical_size = touchpad_physical_size(&layout);

    if fields.is_empty() {
        return None;
    }

    let features = fields.features();
    let button_press_threshold_range = fields.range(KEY_BUTTON_PRESS_THRESHOLD);
    let haptic_intensity_range = fields.range(KEY_HAPTIC_INTENSITY);

    let device = match HidrawDevice::open(&hidraw_path) {
        Ok(d) => d,
        Err(e) => {
            log::error!("config: failed to open hidraw device: {}", e);
            return None;
        }
    };

    log::info!("config: found PTP features on {}", hidraw_path.display());

    Some(Discovered {
        description: ConfigDescription {
            features,
            button_press_threshold_range,
            haptic_intensity_range,
            physical_size,
        },
        backend: LayoutConfigBackend::new(device, fields)?,
    })
}
