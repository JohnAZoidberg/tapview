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

// IOKit/hid/IOHIDLib.h, macOS 10.15+. IOKit is linked by hidapi.
const IOHID_REQUEST_TYPE_LISTEN_EVENT: u32 = 1;
const IOHID_ACCESS_TYPE_GRANTED: u32 = 0;
const IOHID_ACCESS_TYPE_UNKNOWN: u32 = 2;

extern "C" {
    fn IOHIDCheckAccess(request_type: u32) -> u32;
    fn IOHIDRequestAccess(request_type: u32) -> bool;
}

/// Input Monitoring, which macOS requires before a pointing device opens.
///
/// Without this call a denied open fails with `not permitted` and nothing
/// shows up in System Settings to approve: only `IOHIDRequestAccess` puts
/// the responsible app (the terminal, for a CLI) in the list and prompts.
fn ensure_input_monitoring() -> io::Result<()> {
    let access = unsafe { IOHIDCheckAccess(IOHID_REQUEST_TYPE_LISTEN_EVENT) };
    if access == IOHID_ACCESS_TYPE_GRANTED {
        return Ok(());
    }
    if access == IOHID_ACCESS_TYPE_UNKNOWN
        && unsafe { IOHIDRequestAccess(IOHID_REQUEST_TYPE_LISTEN_EVENT) }
    {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Input Monitoring is not granted: enable the terminal that runs tapview in \
         System Settings > Privacy & Security > Input Monitoring, then restart that terminal",
    ))
}

impl MacHidDevice {
    pub fn open(path: &Path) -> io::Result<Self> {
        ensure_input_monitoring()?;
        let api = hidapi::HidApi::new().map_err(hid_err)?;
        let cpath = CString::new(path.as_os_str().as_bytes())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let dev = api
            .open_path(&cpath)
            .map_err(|e| io::Error::other(format!("{}: {}", path.display(), e)))?;
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

/// Whether the device's descriptor has PTP finger fields. `None` when the
/// device cannot be opened, so the caller can keep it and let the open
/// report why.
fn has_ptp_layout(path: &Path) -> Option<bool> {
    let desc = MacHidDevice::open(path).ok()?.report_descriptor().ok()?;
    Some(crate::ptp::PtpLayout::from_layout(&crate::hid::ReportLayout::parse(&desc)).is_some())
}

/// Every HID device with a Digitizer / Touch Pad top-level collection that
/// carries PTP finger fields.
///
/// hidapi lists a device once per top-level collection, all under one path,
/// so a pad that also declares a Mouse collection would otherwise show up
/// twice. Apple's own pads declare a Touch Pad collection with nothing PTP
/// in it; they are left to MultitouchSupport.
pub fn find_ptp_touchpads() -> Result<Vec<DeviceInfo>, DiscoveryError> {
    let api = hidapi::HidApi::new().map_err(|e| DiscoveryError::Platform(e.to_string()))?;
    let mut found: Vec<DeviceInfo> = Vec::new();
    for d in api.device_list() {
        if d.usage_page() != DIGITIZER_PAGE || d.usage() != USAGE_TOUCH_PAD {
            continue;
        }
        let devnode = PathBuf::from(std::ffi::OsStr::from_bytes(d.path().to_bytes()));
        if found.iter().any(|f| f.devnode == devnode) || has_ptp_layout(&devnode) == Some(false) {
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
    Ok(found)
}
