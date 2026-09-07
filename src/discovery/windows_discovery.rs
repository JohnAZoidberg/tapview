use super::{Bus, DeviceDiscovery, DeviceInfo, DiscoveryError, Integration};
use std::path::PathBuf;
use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Devices::Properties::*;
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

    // The last parameter yields the device node backing this interface, which
    // is where the human-readable names live.
    let mut devinfo_data = SP_DEVINFO_DATA {
        cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
        ..Default::default()
    };

    if SetupDiGetDeviceInterfaceDetailW(
        dev_info,
        interface_data,
        Some(detail),
        required_size,
        None,
        Some(&mut devinfo_data),
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

    let (vendor_id, product_id, name, bus) = if is_touchpad {
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
        let product_string = if HidD_GetProductString(
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
        let chain = device_chain(devinfo_data.DevInst);
        let bus = match bus_from_device_path(&device_path) {
            Bus::Unknown => bus_from_chain(&chain),
            bus => bus,
        };
        (vid, pid, device_name(&chain, product_string), bus)
    } else {
        (None, None, None, Bus::Unknown)
    };

    let _ = CloseHandle(handle);

    if is_touchpad {
        Some(DeviceInfo {
            devnode: PathBuf::from(&device_path),
            name,
            bus,
            integration: Integration::Unknown,
            vendor_id,
            product_id,
        })
    } else {
        None
    }
}

/// A device node plus its first few ancestors, as `(devinst, instance id)`.
///
/// What we enumerate is a HID collection, which knows almost nothing about the
/// hardware; the name and the transport live further up the tree, e.g.
/// `HID\PIXA3854&Col02` -> `ACPI\PIXA3854` -> `ACPI\AMDI0010` (the I2C
/// controller). Four levels reach the physical device on every chain seen so
/// far without climbing all the way to the host controller.
unsafe fn device_chain(devinst: u32) -> Vec<(u32, String)> {
    let mut chain = Vec::new();
    let mut node = Some(devinst);
    for _ in 0..4 {
        let Some(n) = node else { break };
        let id = device_instance_id(n).unwrap_or_default();
        // A hub or host controller describes the port the device hangs off,
        // not the device: stop before names like "4-Port USB 2.0 Hub" can be
        // mistaken for ours.
        if is_hub_or_controller(n, &id) {
            break;
        }
        chain.push((n, id));
        node = parent_devinst(n);
    }
    chain
}

fn is_hub_or_controller(devinst: u32, id: &str) -> bool {
    let service = unsafe { devnode_string(devinst, &DEVPKEY_Device_Service) }
        .unwrap_or_default()
        .to_ascii_lowercase();
    service.starts_with("usbhub") || id.to_ascii_uppercase().starts_with("PCI\\")
}

/// Best-effort human-readable name for a device chain.
///
/// The HID product string is a poor label on Windows: I2C touchpads report the
/// driver ("HIDI2C Device") and Bluetooth ones usually report nothing at all.
/// The SetupAPI properties Device Manager shows are better: `FriendlyName`
/// holds the Bluetooth device name ("Touchpad KB") and
/// `BusReportedDeviceDesc` the name a device reports over its bus ("Synaptics
/// Touchpad"). Both sit on the physical device node rather than the HID
/// collection, so try each level before falling back to the driver-supplied
/// description ("HID-compliant touch pad") and finally the product string.
unsafe fn device_name(chain: &[(u32, String)], product_string: Option<String>) -> Option<String> {
    for (node, _) in chain {
        if let Some(name) = devnode_string(*node, &DEVPKEY_Device_FriendlyName) {
            return Some(name);
        }
        if let Some(name) = devnode_string(*node, &DEVPKEY_Device_BusReportedDeviceDesc)
            .filter(|n| !is_placeholder_name(n))
        {
            return Some(name);
        }
    }

    chain
        .first()
        .and_then(|(node, _)| devnode_string(*node, &DEVPKEY_Device_DeviceDesc))
        .or(product_string)
}

/// Whether a bus-reported description is a Windows placeholder rather than a
/// product name. Seen in the wild on real touchpads:
/// - the generic Bluetooth labels, "Bluetooth LE Service {00001812-...}" and
///   "Bluetooth LE Device e772e0ecf6ae"
/// - internal-looking tokens such as `BT_FUNCTION` or `Wireless_Device`
/// - per-interface labels on USB composite devices ("HID1"), where the
///   product name ("Touchpad KB") is on the parent node
fn is_placeholder_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("bluetooth le service") || lower.starts_with("bluetooth le device") {
        return true;
    }
    if name.contains('_') && !name.contains(' ') {
        return true;
    }
    let stem = lower.trim_end_matches(|c: char| c.is_ascii_digit()).trim();
    matches!(
        stem,
        "hid" | "if" | "interface" | "input" | "usb" | "usb input"
    )
}

/// Determine the transport from the drivers and enumerators along the chain.
///
/// Used when the HID interface path alone is inconclusive, which is the case
/// for every internal touchpad. The driver service name is exact and
/// language-independent (`hidi2c` on the `ACPI\PIXA3854` node above); the
/// enumerator prefix of the instance ID is the fallback.
fn bus_from_chain(chain: &[(u32, String)]) -> Bus {
    for (node, id) in chain {
        let service = unsafe { devnode_string(*node, &DEVPKEY_Device_Service) }
            .unwrap_or_default()
            .to_ascii_lowercase();
        match service.as_str() {
            "hidi2c" | "i2chid" => return Bus::I2c,
            "hidspi" | "spihid" => return Bus::Spi,
            "hidusb" | "usbhid" => return Bus::Usb,
            "i8042prt" | "hidi8042" => return Bus::Ps2,
            s if s.starts_with("bth") => return Bus::Bluetooth,
            _ => {}
        }

        let enumerator = id.split('\\').next().unwrap_or("").to_ascii_uppercase();
        match enumerator.as_str() {
            "USB" => return Bus::Usb,
            "BTH" | "BTHLE" | "BTHLEDEVICE" | "BTHENUM" => return Bus::Bluetooth,
            _ => {}
        }
    }
    Bus::Unknown
}

/// Device instance ID, e.g. `HID\PIXA3854&Col02\4&10d8260e&0&0001`.
unsafe fn device_instance_id(devinst: u32) -> Option<String> {
    // MAX_DEVICE_ID_LEN is 200 characters.
    let mut buf = [0u16; 256];
    if CM_Get_Device_IDW(devinst, &mut buf, 0) != CR_SUCCESS {
        return None;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// Read a string property off a device node, `None` if unset or not a string.
unsafe fn devnode_string(devinst: u32, key: &DEVPROPKEY) -> Option<String> {
    let mut prop_type = DEVPROPTYPE::default();
    let mut size = 0u32;
    // First call sizes the buffer and is expected to fail with CR_BUFFER_SMALL.
    let _ = CM_Get_DevNode_PropertyW(devinst, key, &mut prop_type, None, &mut size, 0);
    if size == 0 {
        return None;
    }

    // Buffer as u16 so it is aligned for the UTF-16 read below; the API size is
    // in bytes throughout.
    let mut buf: Vec<u16> = vec![0; (size as usize).div_ceil(2)];
    if CM_Get_DevNode_PropertyW(
        devinst,
        key,
        &mut prop_type,
        Some(buf.as_mut_ptr() as *mut u8),
        &mut size,
        0,
    ) != CR_SUCCESS
        || prop_type != DEVPROP_TYPE_STRING
    {
        return None;
    }

    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    let s = String::from_utf16_lossy(&buf[..len]).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

unsafe fn parent_devinst(devinst: u32) -> Option<u32> {
    let mut parent = 0u32;
    if CM_Get_Parent(&mut parent, devinst, 0) == CR_SUCCESS {
        Some(parent)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_rejected() {
        for name in [
            "Bluetooth LE Service {00001812-0000-1000-8000-00805f9b34fb}",
            "Bluetooth LE Device e772e0ecf6ae",
            "BT_FUNCTION",
            "Wireless_Device",
            "HID1",
            "Interface 3",
        ] {
            assert!(
                is_placeholder_name(name),
                "{} should be a placeholder",
                name
            );
        }
    }

    #[test]
    fn product_names_are_kept() {
        for name in [
            "Touchpad KB",
            "Wireless Touchpad Keyboard USB-A Dongle",
            "Synaptics Touchpad",
            "Magic Trackpad 2",
        ] {
            assert!(!is_placeholder_name(name), "{} should be kept", name);
        }
    }

    #[test]
    fn bus_from_path_detects_usb_and_bluetooth() {
        assert_eq!(
            bus_from_device_path(r"\\?\hid#vid_32ac&pid_0039&mi_06&col02#8&6f60102&0&0001"),
            Bus::Usb
        );
        assert_eq!(
            bus_from_device_path(
                r"\\?\hid#{00001812-0000-1000-8000-00805f9b34fb}_dev_vid&0232ac_pid&0034"
            ),
            Bus::Bluetooth
        );
        // I2C-HID has no VID in the path; the device chain decides.
        assert_eq!(
            bus_from_device_path(r"\\?\hid#pixa3854&col02#4&10d8260e&0&0001"),
            Bus::Unknown
        );
    }
}
