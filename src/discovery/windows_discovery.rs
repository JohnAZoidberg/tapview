use super::{Bus, DeviceDiscovery, DeviceInfo, DiscoveryError, Integration};
use std::path::PathBuf;
use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;

pub struct WindowsDiscovery;

impl DeviceDiscovery for WindowsDiscovery {
    fn find_touchpads() -> Result<Vec<DeviceInfo>, DiscoveryError> {
        unsafe { find_touchpads_inner() }
    }
}

unsafe fn find_touchpads_inner() -> Result<Vec<DeviceInfo>, DiscoveryError> {
    let hid_guid = HidD_GetHidGuid();

    let dev_info = SetupDiGetClassDevsW(
        Some(&hid_guid),
        PCWSTR::null(),
        None,
        DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
    )
    .map_err(|e| DiscoveryError::UdevError(format!("SetupDiGetClassDevsW: {}", e)))?;

    let mut results = Vec::new();
    let mut index = 0u32;

    loop {
        let mut interface_data = SP_DEVICE_INTERFACE_DATA {
            cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };

        if SetupDiEnumDeviceInterfaces(dev_info, None, &hid_guid, index, &mut interface_data)
            .is_err()
        {
            break;
        }

        if let Some(info) = get_touchpad_info(dev_info, &mut interface_data) {
            results.push(info);
        }

        index += 1;
    }

    let _ = SetupDiDestroyDeviceInfoList(dev_info);

    if results.is_empty() {
        Err(DiscoveryError::NotFound)
    } else {
        Ok(results)
    }
}

unsafe fn get_touchpad_info(
    dev_info: HDEVINFO,
    interface_data: &mut SP_DEVICE_INTERFACE_DATA,
) -> Option<DeviceInfo> {
    // First call: get required size
    let mut required_size = 0u32;
    let _ = SetupDiGetDeviceInterfaceDetailW(
        dev_info,
        interface_data,
        None,
        0,
        Some(&mut required_size),
        None,
    );

    if required_size == 0 {
        return None;
    }

    // Allocate buffer for the detail data
    let mut buf = vec![0u8; required_size as usize];
    let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
    // cbSize must be set to the size of the fixed part of the struct (not the buffer size)
    (*detail).cbSize = std::mem::size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;

    if SetupDiGetDeviceInterfaceDetailW(
        dev_info,
        interface_data,
        Some(detail),
        required_size,
        None,
        None,
    )
    .is_err()
    {
        return None;
    }

    // Extract the device path string
    let device_path_ptr = &(*detail).DevicePath as *const u16;
    let device_path = pcwstr_to_string(device_path_ptr);

    // Try to open the device to check if it's a touchpad
    let wide_path: Vec<u16> = device_path
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let handle = CreateFileW(
        PCWSTR(wide_path.as_ptr()),
        0, // No access needed, just checking attributes
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        None,
    )
    .ok()?;

    let mut preparsed_data = PHIDP_PREPARSED_DATA::default();
    let is_touchpad = if HidD_GetPreparsedData(handle, &mut preparsed_data) {
        let mut caps = HIDP_CAPS::default();
        if HidP_GetCaps(preparsed_data, &mut caps) == HIDP_STATUS_SUCCESS {
            // Usage Page 0x0D = Digitizer, Usage 0x05 = Touchpad
            caps.UsagePage == 0x0D && caps.Usage == 0x05
        } else {
            false
        }
    } else {
        false
    };

    if preparsed_data.0 != 0 {
        let _ = HidD_FreePreparsedData(preparsed_data);
    }

    let (vendor_id, product_id, name) = if is_touchpad {
        let mut attrs = HIDD_ATTRIBUTES {
            Size: std::mem::size_of::<HIDD_ATTRIBUTES>() as u32,
            ..Default::default()
        };
        let (vid, pid) = if HidD_GetAttributes(handle, &mut attrs) {
            (Some(attrs.VendorID), Some(attrs.ProductID))
        } else {
            (None, None)
        };

        // HidD_GetProductString buffer must not exceed 4093 bytes.
        let mut buf = [0u16; 128];
        let name = if HidD_GetProductString(
            handle,
            buf.as_mut_ptr() as *mut _,
            (buf.len() * std::mem::size_of::<u16>()) as u32,
        ) {
            let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
            let s = String::from_utf16_lossy(&buf[..len]).trim().to_string();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        } else {
            None
        };
        (vid, pid, name)
    } else {
        (None, None, None)
    };

    let _ = CloseHandle(handle);

    if is_touchpad {
        Some(DeviceInfo {
            devnode: PathBuf::from(&device_path),
            name,
            bus: bus_from_device_path(&device_path),
            integration: Integration::Unknown,
            vendor_id,
            product_id,
        })
    } else {
        None
    }
}

/// Guess the transport from the HID interface path.
///
/// Bluetooth HID devices carry the HID (0x1124) or HID-over-GATT (0x1812)
/// service UUID in their hardware ID, e.g.
/// `\\?\hid#{00001812-0000-1000-8000-00805f9b34fb}_dev_vid&...`.
/// USB HID devices use `hid#vid_xxxx&pid_xxxx`. I2C-HID devices use an
/// ACPI-derived ID without a VID (e.g. `hid#syna7813&col01`).
fn bus_from_device_path(path: &str) -> Bus {
    let lower = path.to_ascii_lowercase();
    if lower.contains("00001124-0000-1000-8000-00805f9b34fb")
        || lower.contains("00001812-0000-1000-8000-00805f9b34fb")
        || lower.contains("bthenum")
        || lower.contains("bthledevice")
    {
        Bus::Bluetooth
    } else if lower.contains("hid#vid_") {
        Bus::Usb
    } else {
        Bus::Unknown
    }
}

unsafe fn pcwstr_to_string(ptr: *const u16) -> String {
    let mut len = 0;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}
