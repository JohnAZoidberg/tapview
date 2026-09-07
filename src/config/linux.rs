//! Linux: PTP configuration over the touchpad's sibling hidraw node, with the
//! descriptor read from sysfs. All the actual work is in
//! [`LayoutConfigBackend`]; this file only finds and opens the device.

use super::layout_backend::{
    touchpad_physical_size, LayoutConfigBackend, PtpFields, KEY_BUTTON_PRESS_THRESHOLD,
    KEY_HAPTIC_INTENSITY,
};
use super::{ConfigBackend, PtpConfig};
use crate::heatmap::discovery::find_sibling_hidraw;
use crate::heatmap::hidraw::HidrawDevice;
use crate::hid::ReportLayout;
use std::path::Path;

pub fn discover(evdev_path: &Path) -> Option<PtpConfig> {
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

    let mut backend = LayoutConfigBackend::new(device, fields)?;
    let values = backend.read_all();

    // Click force / haptic intensity are write-only on this firmware; seed startup defaults.
    let button_press_threshold = features.has_button_press_threshold.then_some(2);
    let haptic_intensity = features.has_haptic_intensity.then_some(50);

    let mut config = PtpConfig {
        features,
        input_mode: values.input_mode,
        surface_switch: values.surface_switch,
        button_switch: values.button_switch,
        contact_count_max: values.contact_count_max,
        pad_type: values.pad_type,
        latency_mode: values.latency_mode,
        button_press_threshold,
        button_press_threshold_range,
        haptic_intensity,
        haptic_intensity_range,
        physical_size,
        backend: Box::new(backend),
    };
    config.probe_writable();
    Some(config)
}
