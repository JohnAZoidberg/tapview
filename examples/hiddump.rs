//! Diagnostic: dump HID feature-report capabilities and exercise the PixArt
//! register protocol on Windows, printing real Win32 error codes.
//!
//! Usage:
//!   cargo run --example hiddump                 # list all HID collections
//!   cargo run --example hiddump -- <substr>     # only paths containing <substr>
//!   cargo run --example hiddump -- <substr> try # also try the register protocol

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("windows only");
}

#[cfg(target_os = "windows")]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let filter = args.first().map(|s| s.to_ascii_lowercase());
    let try_proto = args.iter().any(|a| a == "try");
    unsafe { run(filter.as_deref(), try_proto) }
}

#[cfg(target_os = "windows")]
unsafe fn run(filter: Option<&str>, try_proto: bool) {
    use windows::core::PCWSTR;
    use windows::Win32::Devices::DeviceAndDriverInstallation::*;
    use windows::Win32::Devices::HumanInterfaceDevice::*;

    let hid_guid = HidD_GetHidGuid();
    let dev_info = SetupDiGetClassDevsW(
        Some(&hid_guid),
        PCWSTR::null(),
        None,
        DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
    )
    .expect("SetupDiGetClassDevsW");

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
        index += 1;

        let Some(path) = interface_path(dev_info, &mut interface_data) else {
            continue;
        };
        if let Some(f) = filter {
            if !path.to_ascii_lowercase().contains(f) {
                continue;
            }
        }
        dump(&path, try_proto);
    }

    let _ = SetupDiDestroyDeviceInfoList(dev_info);
}

#[cfg(target_os = "windows")]
unsafe fn interface_path(
    dev_info: windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO,
    interface_data: &mut windows::Win32::Devices::DeviceAndDriverInstallation::SP_DEVICE_INTERFACE_DATA,
) -> Option<String> {
    use windows::Win32::Devices::DeviceAndDriverInstallation::*;

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
    SetupDiGetDeviceInterfaceDetailW(
        dev_info,
        interface_data,
        Some(detail),
        required_size,
        None,
        None,
    )
    .ok()?;

    let ptr = &(*detail).DevicePath as *const u16;
    let mut len = 0;
    while *ptr.add(len) != 0 {
        len += 1;
    }
    Some(String::from_utf16_lossy(std::slice::from_raw_parts(
        ptr, len,
    )))
}

#[cfg(target_os = "windows")]
unsafe fn dump(path: &str, try_proto: bool) {
    use windows::core::PCWSTR;
    use windows::Win32::Devices::HumanInterfaceDevice::*;
    use windows::Win32::Foundation::*;
    use windows::Win32::Storage::FileSystem::*;

    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    // Open read/write like the heatmap code does; fall back to query-only so we
    // can still report caps for exclusively-owned collections.
    let (handle, rw) = match CreateFileW(
        PCWSTR(wide.as_ptr()),
        (GENERIC_READ | GENERIC_WRITE).0,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        None,
        OPEN_EXISTING,
        FILE_FLAGS_AND_ATTRIBUTES(0),
        None,
    ) {
        Ok(h) => (h, true),
        Err(e) => {
            println!("{}\n  open RW failed: {}", path, e);
            match CreateFileW(
                PCWSTR(wide.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            ) {
                Ok(h) => (h, false),
                Err(e2) => {
                    println!("  open query-only failed too: {}", e2);
                    return;
                }
            }
        }
    };

    if rw {
        println!("{}", path);
    }

    let mut pp = PHIDP_PREPARSED_DATA::default();
    if !HidD_GetPreparsedData(handle, &mut pp) {
        println!("  HidD_GetPreparsedData failed: {:?}", GetLastError());
        let _ = CloseHandle(handle);
        return;
    }

    let mut caps = HIDP_CAPS::default();
    if HidP_GetCaps(pp, &mut caps) == HIDP_STATUS_SUCCESS {
        println!(
            "  usage {:#06x}/{:#04x}  in={} out={} feat={}  featValueCaps={} featButtonCaps={}  rw={}",
            caps.UsagePage,
            caps.Usage,
            caps.InputReportByteLength,
            caps.OutputReportByteLength,
            caps.FeatureReportByteLength,
            caps.NumberFeatureValueCaps,
            caps.NumberFeatureButtonCaps,
            rw,
        );

        if caps.NumberFeatureValueCaps > 0 {
            let mut n = caps.NumberFeatureValueCaps;
            let mut vc = vec![HIDP_VALUE_CAPS::default(); n as usize];
            if HidP_GetValueCaps(HidP_Feature, vc.as_mut_ptr(), &mut n, pp) == HIDP_STATUS_SUCCESS {
                for c in &vc[..n as usize] {
                    println!(
                        "    valueCap  reportId={:#04x} reportCount={} bitSize={} usagePage={:#06x} isRange={}",
                        c.ReportID, c.ReportCount, c.BitSize, c.UsagePage, c.IsRange,
                    );
                }
            } else {
                println!("    HidP_GetValueCaps(Feature) failed");
            }
        }
        if caps.NumberFeatureButtonCaps > 0 {
            let mut n = caps.NumberFeatureButtonCaps;
            let mut bc = vec![HIDP_BUTTON_CAPS::default(); n as usize];
            if HidP_GetButtonCaps(HidP_Feature, bc.as_mut_ptr(), &mut n, pp) == HIDP_STATUS_SUCCESS
            {
                for c in &bc[..n as usize] {
                    println!(
                        "    buttonCap reportId={:#04x} usagePage={:#06x} isRange={}",
                        c.ReportID, c.UsagePage, c.IsRange,
                    );
                }
            }
        }

        if try_proto && rw && caps.FeatureReportByteLength > 0 {
            try_protocol(handle, caps.FeatureReportByteLength as usize);
            try_burst(handle, caps.FeatureReportByteLength as usize);
        }
    } else {
        println!("  HidP_GetCaps failed");
    }

    let _ = HidD_FreePreparsedData(pp);
    let _ = CloseHandle(handle);
}

/// Read PixArt Part ID (bank 0, regs 0x78/0x79) via report 0x42, once with the
/// 4-byte buffer the app uses and once with a full-length buffer.
#[cfg(target_os = "windows")]
unsafe fn try_protocol(handle: windows::Win32::Foundation::HANDLE, feat_len: usize) {
    for &len in &[4usize, feat_len] {
        println!("  -- protocol attempt with {}-byte buffer --", len);
        let mut part = 0u16;
        let mut ok = true;
        for (i, &reg) in [0x78u8, 0x79u8].iter().enumerate() {
            let mut out = vec![0u8; len];
            out[0] = 0x42; // report id
            out[1] = reg; // addr
            out[2] = 0x10; // bank 0, with the read flag set
            out[3] = 0x00;
            if !set_feature(handle, &out) {
                println!("     SetFeature(0x42) failed: {:?}", last_err());
                ok = false;
                break;
            }
            let mut inb = vec![0u8; len];
            inb[0] = 0x42;
            if !get_feature(handle, &mut inb) {
                println!("     GetFeature(0x42) failed: {:?}", last_err());
                ok = false;
                break;
            }
            println!("     reg {:#04x} -> {:02x?}", reg, &inb[..len.min(8)]);
            part |= (inb[3] as u16) << (8 * i);
        }
        if ok {
            println!("     Part ID = {:#06x}", part);
        }
    }
}

/// Try GetFeature on the burst report 0x41 at a range of buffer sizes, to see
/// which one the transport is willing to accept.
#[cfg(target_os = "windows")]
unsafe fn try_burst(handle: windows::Win32::Foundation::HANDLE, feat_len: usize) {
    println!("  -- burst report 0x41 GetFeature by buffer size --");
    for &len in &[
        feat_len,
        feat_len + 1,
        feat_len + 4,
        feat_len * 2,
        512,
        513,
        1024,
        4096,
    ] {
        let mut buf = vec![0u8; len];
        buf[0] = 0x41;
        if get_feature(handle, &mut buf) {
            println!("     {:5} bytes -> ok, first 8: {:02x?}", len, &buf[..8]);
        } else {
            println!("     {:5} bytes -> {:?}", len, last_err());
        }
    }
}

#[cfg(target_os = "windows")]
unsafe fn set_feature(handle: windows::Win32::Foundation::HANDLE, buf: &[u8]) -> bool {
    windows::Win32::Devices::HumanInterfaceDevice::HidD_SetFeature(
        handle,
        buf.as_ptr() as *const std::ffi::c_void,
        buf.len() as u32,
    )
}

#[cfg(target_os = "windows")]
unsafe fn get_feature(handle: windows::Win32::Foundation::HANDLE, buf: &mut [u8]) -> bool {
    windows::Win32::Devices::HumanInterfaceDevice::HidD_GetFeature(
        handle,
        buf.as_mut_ptr() as *mut std::ffi::c_void,
        buf.len() as u32,
    )
}

#[cfg(target_os = "windows")]
unsafe fn last_err() -> windows::Win32::Foundation::WIN32_ERROR {
    windows::Win32::Foundation::GetLastError()
}
