#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;

use std::io;
use std::path::Path;

/// Physical dimensions of the touchpad surface, extracted from the HID descriptor.
pub struct TouchpadPhysicalSize {
    pub width_mm: f64,
    pub height_mm: f64,
}

/// Which PTP configuration features the device supports.
pub struct PtpFeatures {
    pub has_input_mode: bool,
    pub has_surface_switch: bool,
    pub has_button_switch: bool,
    pub has_contact_count_max: bool,
    pub has_pad_type: bool,
    pub has_latency_mode: bool,
    pub has_button_press_threshold: bool,
    // Writable flags — false when the descriptor marks the field as Constant,
    // or when a probe write at startup was rejected by the kernel driver.
    pub input_mode_writable: bool,
    pub surface_switch_writable: bool,
    pub button_switch_writable: bool,
    pub latency_mode_writable: bool,
    pub button_press_threshold_writable: bool,
}

/// Snapshot of current PTP configuration values.
pub struct ConfigValues {
    pub input_mode: Option<u8>,
    pub surface_switch: Option<bool>,
    pub button_switch: Option<bool>,
    pub contact_count_max: Option<u8>,
    pub pad_type: Option<u8>,
    pub latency_mode: Option<bool>,
    pub button_press_threshold: Option<u8>,
}

/// Platform-specific backend for reading/writing PTP feature reports.
pub(crate) trait ConfigBackend: Send {
    fn read_all(&mut self) -> ConfigValues;
    fn write_input_mode(&mut self, value: u8) -> io::Result<()>;
    fn write_selective_reporting(&mut self, surface: bool, button: bool) -> io::Result<()>;
    fn write_latency_mode(&mut self, high: bool) -> io::Result<()>;
    fn write_button_press_threshold(&mut self, value: u8) -> io::Result<()>;
}

/// PTP device configuration state and controls.
pub struct PtpConfig {
    pub features: PtpFeatures,
    pub input_mode: Option<u8>,
    pub surface_switch: Option<bool>,
    pub button_switch: Option<bool>,
    pub contact_count_max: Option<u8>,
    pub pad_type: Option<u8>,
    pub latency_mode: Option<bool>,
    pub button_press_threshold: Option<u8>,
    pub physical_size: Option<TouchpadPhysicalSize>,
    backend: Box<dyn ConfigBackend>,
}

impl PtpConfig {
    pub fn refresh(&mut self) {
        let v = self.backend.read_all();
        self.input_mode = v.input_mode;
        self.surface_switch = v.surface_switch;
        self.button_switch = v.button_switch;
        self.contact_count_max = v.contact_count_max;
        self.pad_type = v.pad_type;
        self.latency_mode = v.latency_mode;
        self.button_press_threshold = v.button_press_threshold;
    }

    /// Probe which fields are actually writable by attempting no-op writes.
    /// Disables writable flags for fields the kernel rejects.
    ///
    /// On Linux, writes can fail (EINVAL) when the heatmap module also has
    /// the same hidraw device open.  The kernel's hid-multitouch driver
    /// manages latency mode automatically (low on open, high on close),
    /// so losing write access here is harmless.
    pub fn probe_writable(&mut self) {
        if self.features.input_mode_writable {
            if let Some(v) = self.input_mode {
                if self.backend.write_input_mode(v).is_err() {
                    self.features.input_mode_writable = false;
                }
            }
        }
        if self.features.surface_switch_writable || self.features.button_switch_writable {
            let s = self.surface_switch.unwrap_or(true);
            let b = self.button_switch.unwrap_or(true);
            if self.backend.write_selective_reporting(s, b).is_err() {
                self.features.surface_switch_writable = false;
                self.features.button_switch_writable = false;
            }
        }
        if self.features.latency_mode_writable {
            if let Some(v) = self.latency_mode {
                if self.backend.write_latency_mode(v).is_err() {
                    self.features.latency_mode_writable = false;
                }
            }
        }
        if self.features.button_press_threshold_writable {
            if let Some(v) = self.button_press_threshold {
                if self.backend.write_button_press_threshold(v).is_err() {
                    self.features.button_press_threshold_writable = false;
                }
            }
        }
    }

    pub fn set_input_mode(&mut self, value: u8) -> io::Result<()> {
        self.backend.write_input_mode(value)?;
        self.input_mode = Some(value);
        Ok(())
    }

    pub fn set_selective_reporting(&mut self, surface: bool, button: bool) -> io::Result<()> {
        self.backend.write_selective_reporting(surface, button)?;
        self.surface_switch = Some(surface);
        self.button_switch = Some(button);
        Ok(())
    }

    pub fn set_latency_mode(&mut self, high: bool) -> io::Result<()> {
        self.backend.write_latency_mode(high)?;
        self.latency_mode = Some(high);
        Ok(())
    }

    pub fn set_button_press_threshold(&mut self, value: u8) -> io::Result<()> {
        self.backend.write_button_press_threshold(value)?;
        self.button_press_threshold = Some(value);
        Ok(())
    }
}

pub fn discover(device_path: &Path) -> Option<PtpConfig> {
    #[cfg(target_os = "linux")]
    {
        linux::discover(device_path)
    }
    #[cfg(target_os = "windows")]
    {
        windows::discover(device_path)
    }
}
