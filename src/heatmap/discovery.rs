use std::io;
use std::path::{Path, PathBuf};

// ── Linux: find sibling hidraw via udev ───────────────────────────────────

#[cfg(target_os = "linux")]
use std::fs;

#[cfg(target_os = "linux")]
pub fn find_sibling_hidraw(evdev_path: &Path) -> io::Result<PathBuf> {
    let evdev_name = evdev_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad evdev path"))?;

    let mut enumerator = udev::Enumerator::new().map_err(io::Error::other)?;
    enumerator
        .match_subsystem("input")
        .map_err(io::Error::other)?;
    enumerator
        .match_sysname(evdev_name)
        .map_err(io::Error::other)?;

    let evdev_dev = enumerator
        .scan_devices()
        .map_err(io::Error::other)?
        .next()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("evdev device not found in udev: {}", evdev_name),
            )
        })?;

    let mut current_path = evdev_dev.syspath().to_path_buf();
    let hid_path = loop {
        current_path = current_path
            .parent()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "no HID parent found for evdev device",
                )
            })?;

        let subsystem_link = current_path.join("subsystem");
        if let Ok(target) = fs::read_link(&subsystem_link) {
            if let Some(name) = target.file_name().and_then(|n| n.to_str()) {
                if name == "hid" {
                    break current_path;
                }
            }
        }

        if current_path.as_os_str() == "/sys" || current_path.parent().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no HID parent found for evdev device",
            ));
        }
    };

    let mut hidraw_enum = udev::Enumerator::new().map_err(io::Error::other)?;
    hidraw_enum
        .match_subsystem("hidraw")
        .map_err(io::Error::other)?;
    hidraw_enum
        .match_parent(&udev::Device::from_syspath(&hid_path).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;

    for hidraw_dev in hidraw_enum.scan_devices().map_err(io::Error::other)? {
        if let Some(devnode) = hidraw_dev.devnode() {
            return Ok(devnode.to_path_buf());
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no sibling hidraw device found",
    ))
}

#[cfg(target_os = "linux")]
pub fn determine_burst_report_length(hidraw_path: &Path) -> io::Result<usize> {
    let hidraw_name = hidraw_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad hidraw path"))?;

    let desc_path = format!("/sys/class/hidraw/{}/device/report_descriptor", hidraw_name);
    let desc = fs::read(desc_path)?;

    parse_report_descriptor_for_burst_len(&desc).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "could not find Report ID 0x41 ReportCount in HID descriptor",
        )
    })
}

#[cfg(target_os = "linux")]
fn parse_report_descriptor_for_burst_len(desc: &[u8]) -> Option<usize> {
    let mut i = 0;
    let mut current_report_id: Option<u8> = None;
    let mut current_report_count: Option<usize> = None;

    while i < desc.len() {
        let prefix = desc[i];

        // Long item
        if prefix == 0xFE {
            if i + 2 >= desc.len() {
                break;
            }
            let data_size = desc[i + 1] as usize;
            i += 3 + data_size;
            continue;
        }

        // Short item
        let size = match prefix & 0x03 {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 4,
            _ => unreachable!(),
        };

        if i + 1 + size > desc.len() {
            break;
        }

        let tag = prefix & 0xFC;
        let data = &desc[i + 1..i + 1 + size];

        match tag {
            // Report ID (Global, tag = 0x84)
            0x84 => {
                if let Some(&id) = data.first() {
                    current_report_id = Some(id);
                }
            }
            // Report Count (Global, tag = 0x94)
            0x94 => {
                let count = match size {
                    1 => data[0] as usize,
                    2 => u16::from_le_bytes([data[0], data[1]]) as usize,
                    4 => u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize,
                    _ => 0,
                };
                current_report_count = Some(count);
            }
            // Feature (Main, tag = 0xB0)
            0xB0 => {
                if current_report_id == Some(0x41) {
                    if let Some(count) = current_report_count {
                        return Some(count);
                    }
                }
            }
            _ => {}
        }

        i += 1 + size;
    }

    None
}

// ── Windows: find HID device for heatmap via SetupAPI ─────────────────────

#[cfg(target_os = "windows")]
use windows::core::PCWSTR;
#[cfg(target_os = "windows")]
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
#[cfg(target_os = "windows")]
use windows::Win32::Devices::HumanInterfaceDevice::*;
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::*;
#[cfg(target_os = "windows")]
use windows::Win32::Storage::FileSystem::*;

/// Find a HID device suitable for heatmap feature reports and determine its
/// burst report length. Returns (device_path, burst_len).
///
/// Enumerates all HID devices, looking for one on the same physical hardware
/// as the touchpad that supports Report ID 0x41 feature reports.
#[cfg(target_os = "windows")]
pub fn find_hid_device_for_heatmap(touchpad_path: &Path) -> io::Result<(PathBuf, usize)> {
    let _ = touchpad_path; // We enumerate all HID devices instead of walking the device tree
    unsafe { find_hid_device_for_heatmap_inner() }
}

#[cfg(target_os = "windows")]
unsafe fn find_hid_device_for_heatmap_inner() -> io::Result<(PathBuf, usize)> {
    let hid_guid = HidD_GetHidGuid();

    let dev_info = SetupDiGetClassDevsW(
        Some(&hid_guid),
        PCWSTR::null(),
        None,
        DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
    )
    .map_err(|e| io::Error::other(format!("SetupDiGetClassDevsW: {}", e)))?;

    let mut index = 0u32;
    let mut best_result: Option<(PathBuf, usize)> = None;

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

        if let Some(result) = check_hid_device_for_heatmap(dev_info, &mut interface_data) {
            best_result = Some(result);
            break;
        }

        index += 1;
    }

    let _ = SetupDiDestroyDeviceInfoList(dev_info);

    best_result.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no HID device with Report ID 0x41 feature report found",
        )
    })
}

#[cfg(target_os = "windows")]
unsafe fn check_hid_device_for_heatmap(
    dev_info: HDEVINFO,
    interface_data: &mut SP_DEVICE_INTERFACE_DATA,
) -> Option<(PathBuf, usize)> {
    // Get device path
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

    let mut buf = vec![0u8; required_size as usize];
    let detail = buf.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
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

    let device_path_ptr = &(*detail).DevicePath as *const u16;
    let device_path = pcwstr_to_string(device_path_ptr);

    // Try to open with read/write access for feature reports
    let wide_path: Vec<u16> = device_path
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let handle = CreateFileW(
        PCWSTR(wide_path.as_ptr()),
        0x80000000 | 0x40000000, // GENERIC_READ | GENERIC_WRITE
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        None,
    )
    .ok()?;

    let mut preparsed_data = PHIDP_PREPARSED_DATA::default();
    let result = if HidD_GetPreparsedData(handle, &mut preparsed_data) {
        let mut caps = HIDP_CAPS::default();
        if HidP_GetCaps(preparsed_data, &mut caps) == HIDP_STATUS_SUCCESS {
            // Check for Report ID 0x41 feature report by looking at feature report byte length
            // If the device has feature reports, check for burst report
            if caps.NumberFeatureValueCaps > 0 {
                // Get feature value caps to find Report ID 0x41
                let mut num_caps = caps.NumberFeatureValueCaps;
                let mut value_caps = vec![HIDP_VALUE_CAPS::default(); num_caps as usize];
                if HidP_GetValueCaps(
                    HidP_Feature,
                    value_caps.as_mut_ptr(),
                    &mut num_caps,
                    preparsed_data,
                ) == HIDP_STATUS_SUCCESS
                {
                    // Look for a value cap with Report ID 0x41
                    let burst_cap = value_caps[..num_caps as usize]
                        .iter()
                        .find(|vc| vc.ReportID == 0x41);

                    if let Some(vc) = burst_cap {
                        // ReportCount tells us the burst payload length
                        let burst_len = vc.ReportCount as usize;
                        if burst_len > 0 {
                            Some((PathBuf::from(&device_path), burst_len))
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    if preparsed_data.0 != 0 {
        let _ = HidD_FreePreparsedData(preparsed_data);
    }
    let _ = CloseHandle(handle);

    result
}

#[cfg(target_os = "windows")]
unsafe fn pcwstr_to_string(ptr: *const u16) -> String {
    let mut len = 0;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    String::from_utf16_lossy(std::slice::from_raw_parts(ptr, len))
}
