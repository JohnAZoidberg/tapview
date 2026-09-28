//! macOS: HID devices through IOHIDManager, by way of hidapi.
//!
//! macOS has no device nodes for HID. A device is named by its IORegistry
//! entry ID, which hidapi spells as a path (`DevSrvsID:4294970123`); that
//! string is what [`crate::DeviceInfo::devnode`] holds here. Every consumer
//! (touch reader, heatmap, config) opens its own handle on it: hidapi opens
//! devices shared (the `macos-shared-device` feature), so the handles and
//! the system's own driver coexist.

use crate::discovery::{Bus, DeviceInfo, DiscoveryError, Integration};
use crate::heatmap::HidDevice;
use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

const DIGITIZER_PAGE: u16 = 0x000D;
const USAGE_TOUCH_PAD: u16 = 0x0005;

pub struct MacHidDevice {
    dev: hidapi::HidDevice,
}

fn hid_err(e: hidapi::HidError) -> io::Error {
    io::Error::other(e.to_string())
}

impl MacHidDevice {
    pub fn open(path: &Path) -> io::Result<Self> {
        let api = hidapi::HidApi::new().map_err(hid_err)?;
        let cpath = CString::new(path.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let dev = api.open_path(&cpath).map_err(|e| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{}: {} (grant Input Monitoring to the terminal in System Settings > Privacy & Security)",
                    path.display(),
                    e
                ),
            )
        })?;
        Ok(Self { dev })
    }

    /// The raw report descriptor, as IOKit reconstructs it from the device.
    pub fn report_descriptor(&self) -> io::Result<Vec<u8>> {
        let mut buf = vec![0u8; hidapi::MAX_REPORT_DESCRIPTOR_SIZE];
        let n = self.dev.get_report_descriptor(&mut buf).map_err(hid_err)?;
        buf.truncate(n);
        Ok(buf)
    }

    /// One input report into `buf`, report-ID byte first for numbered
    /// reports. `Ok(None)` when nothing arrived within `timeout_ms`.
    pub fn read_report(&self, buf: &mut [u8], timeout_ms: i32) -> io::Result<Option<usize>> {
        match self.dev.read_timeout(buf, timeout_ms).map_err(hid_err)? {
            0 => Ok(None),
            n => Ok(Some(n)),
        }
    }
}

impl HidDevice for MacHidDevice {
    async fn set_feature(&self, buf: &[u8]) -> io::Result<()> {
        self.dev.send_feature_report(buf).map_err(hid_err)
    }

    async fn get_feature(&self, buf: &mut [u8]) -> io::Result<usize> {
        self.dev.get_feature_report(buf).map_err(hid_err)
    }
}

/// Every HID device with a Digitizer / Touch Pad top-level collection.
///
/// hidapi lists a device once per top-level collection, all under one path,
/// so a pad that also declares a Mouse collection would otherwise show up
/// twice.
pub fn find_touchpads() -> Result<Vec<DeviceInfo>, DiscoveryError> {
    let api = hidapi::HidApi::new().map_err(|e| DiscoveryError::Platform(e.to_string()))?;
    let mut found: Vec<DeviceInfo> = Vec::new();
    for d in api.device_list() {
        if d.usage_page() != DIGITIZER_PAGE || d.usage() != USAGE_TOUCH_PAD {
            continue;
        }
        let devnode = PathBuf::from(std::ffi::OsStr::from_bytes(d.path().to_bytes()));
        if found.iter().any(|f| f.devnode == devnode) {
            continue;
        }
        found.push(DeviceInfo {
            devnode,
            name: d
                .product_string()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            bus: match d.bus_type() {
                hidapi::BusType::Usb => Bus::Usb,
                hidapi::BusType::Bluetooth => Bus::Bluetooth,
                hidapi::BusType::I2c => Bus::I2c,
                hidapi::BusType::Spi => Bus::Spi,
                hidapi::BusType::Unknown => Bus::Unknown,
            },
            integration: Integration::Unknown,
            vendor_id: Some(d.vendor_id()),
            product_id: Some(d.product_id()),
        });
    }
    if found.is_empty() {
        return Err(DiscoveryError::NotFound);
    }
    Ok(found)
}
