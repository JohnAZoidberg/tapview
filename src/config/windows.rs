use super::{ConfigBackend, ConfigValues, PtpConfig, PtpFeatures};
use crate::heatmap::discovery::{extract_parent_device_id, pcwstr_to_string};
use crate::heatmap::windows_hid::WinHidDevice;
use crate::heatmap::HidDevice;
use std::io;
use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::*;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::Storage::FileSystem::*;

const DIGITIZER_PAGE: u16 = 0x000D;

const USAGE_INPUT_MODE: u16 = 0x0052;
const USAGE_CONTACT_COUNT_MAX: u16 = 0x0055;
const USAGE_SURFACE_SWITCH: u16 = 0x0057;
const USAGE_BUTTON_SWITCH: u16 = 0x0058;
const USAGE_PAD_TYPE: u16 = 0x0059;
const USAGE_LATENCY_MODE: u16 = 0x0060;
const USAGE_BUTTON_PRESS_THRESHOLD: u16 = 0x00B0;

struct PtpUsageInfo {
    report_id: u8,
}

struct WindowsConfigBackend {
    device: WinHidDevice,
    preparsed: isize, // PHIDP_PREPARSED_DATA stored as isize for Send
    feature_report_len: usize,
    input_mode: Option<PtpUsageInfo>,
    surface_switch: Option<PtpUsageInfo>,
    button_switch: Option<PtpUsageInfo>,
    contact_count_max: Option<PtpUsageInfo>,
    pad_type: Option<PtpUsageInfo>,
    latency_mode: Option<PtpUsageInfo>,
    button_press_threshold: Option<PtpUsageInfo>,
}

// PHIDP_PREPARSED_DATA is an opaque pointer (isize), safe to send across threads
unsafe impl Send for WindowsConfigBackend {}

impl Drop for WindowsConfigBackend {
    fn drop(&mut self) {
        if self.preparsed != 0 {
            unsafe {
                let _ = HidD_FreePreparsedData(PHIDP_PREPARSED_DATA(self.preparsed));
            }
        }
    }
}

impl WindowsConfigBackend {
    fn read_usage_value(&self, usage: u16, info: &PtpUsageInfo) -> Option<u32> {
        let mut buf = vec![0u8; self.feature_report_len];
        buf[0] = info.report_id;
        self.device.get_feature(&mut buf).ok()?;

        let preparsed = PHIDP_PREPARSED_DATA(self.preparsed);
        let mut value: u32 = 0;
        let status = unsafe {
            HidP_GetUsageValue(
                HidP_Feature,
                DIGITIZER_PAGE,
                Some(0),
                usage,
                &mut value,
                preparsed,
                &buf,
            )
        };
        if status == HIDP_STATUS_SUCCESS {
            Some(value)
        } else {
            None
        }
    }

    fn write_usage_value(&self, usage: u16, info: &PtpUsageInfo, value: u32) -> io::Result<()> {
        let mut buf = vec![0u8; self.feature_report_len];
        buf[0] = info.report_id;
        // Read-modify-write
        self.device.get_feature(&mut buf)?;

        let preparsed = PHIDP_PREPARSED_DATA(self.preparsed);
        let status = unsafe {
            HidP_SetUsageValue(
                HidP_Feature,
                DIGITIZER_PAGE,
                Some(0),
                usage,
                value,
                preparsed,
                &mut buf,
            )
        };
        if status != HIDP_STATUS_SUCCESS {
            return Err(io::Error::other("HidP_SetUsageValue failed"));
        }

        self.device.set_feature(&buf)
    }
}

impl ConfigBackend for WindowsConfigBackend {
    fn read_all(&mut self) -> ConfigValues {
        ConfigValues {
            input_mode: self
                .input_mode
                .as_ref()
                .and_then(|i| self.read_usage_value(USAGE_INPUT_MODE, i))
                .map(|v| v as u8),
            surface_switch: self
                .surface_switch
                .as_ref()
                .and_then(|i| self.read_usage_value(USAGE_SURFACE_SWITCH, i))
                .map(|v| v != 0),
            button_switch: self
                .button_switch
                .as_ref()
                .and_then(|i| self.read_usage_value(USAGE_BUTTON_SWITCH, i))
                .map(|v| v != 0),
            contact_count_max: self
                .contact_count_max
                .as_ref()
                .and_then(|i| self.read_usage_value(USAGE_CONTACT_COUNT_MAX, i))
                .map(|v| v as u8),
            pad_type: self
                .pad_type
                .as_ref()
                .and_then(|i| self.read_usage_value(USAGE_PAD_TYPE, i))
                .map(|v| v as u8),
            latency_mode: self
                .latency_mode
                .as_ref()
                .and_then(|i| self.read_usage_value(USAGE_LATENCY_MODE, i))
                .map(|v| v != 0),
            button_press_threshold: self
                .button_press_threshold
                .as_ref()
                .and_then(|i| self.read_usage_value(USAGE_BUTTON_PRESS_THRESHOLD, i))
                .map(|v| v as u8),
        }
    }

    fn write_input_mode(&mut self, value: u8) -> io::Result<()> {
        let info = self
            .input_mode
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "input mode not supported"))?;
        self.write_usage_value(USAGE_INPUT_MODE, info, value as u32)
    }

    fn write_selective_reporting(&mut self, surface: bool, button: bool) -> io::Result<()> {
        if let Some(info) = &self.surface_switch {
            self.write_usage_value(USAGE_SURFACE_SWITCH, info, surface as u32)?;
        }
        if let Some(info) = &self.button_switch {
            self.write_usage_value(USAGE_BUTTON_SWITCH, info, button as u32)?;
        }
        Ok(())
    }

    fn write_latency_mode(&mut self, high: bool) -> io::Result<()> {
        let info = self
            .latency_mode
            .as_ref()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "latency mode not supported"))?;
        self.write_usage_value(USAGE_LATENCY_MODE, info, high as u32)
    }

    fn write_button_press_threshold(&mut self, value: u8) -> io::Result<()> {
        let info = self.button_press_threshold.as_ref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "button press threshold not supported",
            )
        })?;
        self.write_usage_value(USAGE_BUTTON_PRESS_THRESHOLD, info, value as u32)
    }
}

// ── Discovery ─────────────────────────────────────────────────────────────────

pub fn discover(touchpad_path: &Path) -> Option<PtpConfig> {
    let parent_id = extract_parent_device_id(touchpad_path);
    unsafe { discover_inner(parent_id.as_deref()) }
}

unsafe fn discover_inner(parent_id: Option<&str>) -> Option<PtpConfig> {
    let hid_guid = HidD_GetHidGuid();

    let dev_info = SetupDiGetClassDevsW(
        Some(&hid_guid),
        PCWSTR::null(),
        None,
        DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
    )
    .ok()?;

    let mut index = 0u32;
    let mut result: Option<PtpConfig> = None;

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

        if let Some(config) = check_hid_device_for_config(dev_info, &mut interface_data, parent_id)
        {
            result = Some(config);
            break;
        }

        index += 1;
    }

    let _ = SetupDiDestroyDeviceInfoList(dev_info);
    result
}

unsafe fn check_hid_device_for_config(
    dev_info: HDEVINFO,
    interface_data: &mut SP_DEVICE_INTERFACE_DATA,
    parent_id: Option<&str>,
) -> Option<PtpConfig> {
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

    // Filter by parent device ID
    if let Some(parent) = parent_id {
        let candidate_parent = extract_parent_device_id(std::path::Path::new(&device_path));
        if candidate_parent.as_deref() != Some(parent) {
            return None;
        }
    }

    // Open device
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
    if !HidD_GetPreparsedData(handle, &mut preparsed_data) {
        let _ = CloseHandle(handle);
        return None;
    }

    let mut caps = HIDP_CAPS::default();
    if HidP_GetCaps(preparsed_data, &mut caps) != HIDP_STATUS_SUCCESS {
        let _ = HidD_FreePreparsedData(preparsed_data);
        let _ = CloseHandle(handle);
        return None;
    }

    if caps.NumberFeatureValueCaps == 0 {
        let _ = HidD_FreePreparsedData(preparsed_data);
        let _ = CloseHandle(handle);
        return None;
    }

    // Get feature value caps
    let mut num_caps = caps.NumberFeatureValueCaps;
    let mut value_caps = vec![HIDP_VALUE_CAPS::default(); num_caps as usize];
    if HidP_GetValueCaps(
        HidP_Feature,
        value_caps.as_mut_ptr(),
        &mut num_caps,
        preparsed_data,
    ) != HIDP_STATUS_SUCCESS
    {
        let _ = HidD_FreePreparsedData(preparsed_data);
        let _ = CloseHandle(handle);
        return None;
    }

    // Search for PTP usages on Digitizer page
    let mut input_mode: Option<PtpUsageInfo> = None;
    let mut surface_switch: Option<PtpUsageInfo> = None;
    let mut button_switch: Option<PtpUsageInfo> = None;
    let mut contact_count_max: Option<PtpUsageInfo> = None;
    let mut pad_type: Option<PtpUsageInfo> = None;
    let mut latency_mode: Option<PtpUsageInfo> = None;
    let mut button_press_threshold: Option<PtpUsageInfo> = None;

    for vc in &value_caps[..num_caps as usize] {
        if vc.UsagePage != DIGITIZER_PAGE {
            continue;
        }

        // Skip range caps — PTP usages are individual
        if vc.Anonymous.NotRange.Usage == 0 {
            continue;
        }

        let usage = vc.Anonymous.NotRange.Usage;
        let info = PtpUsageInfo {
            report_id: vc.ReportID,
        };

        match usage {
            USAGE_INPUT_MODE => input_mode = Some(info),
            USAGE_CONTACT_COUNT_MAX => contact_count_max = Some(info),
            USAGE_SURFACE_SWITCH => surface_switch = Some(info),
            USAGE_BUTTON_SWITCH => button_switch = Some(info),
            USAGE_PAD_TYPE => pad_type = Some(info),
            USAGE_LATENCY_MODE => latency_mode = Some(info),
            USAGE_BUTTON_PRESS_THRESHOLD => button_press_threshold = Some(info),
            _ => {}
        }
    }

    // Need at least one PTP feature
    let has_any = input_mode.is_some()
        || surface_switch.is_some()
        || button_switch.is_some()
        || contact_count_max.is_some()
        || pad_type.is_some()
        || latency_mode.is_some()
        || button_press_threshold.is_some();

    if !has_any {
        let _ = HidD_FreePreparsedData(preparsed_data);
        let _ = CloseHandle(handle);
        return None;
    }

    let features = PtpFeatures {
        has_input_mode: input_mode.is_some(),
        has_surface_switch: surface_switch.is_some(),
        has_button_switch: button_switch.is_some(),
        has_contact_count_max: contact_count_max.is_some(),
        has_pad_type: pad_type.is_some(),
        has_latency_mode: latency_mode.is_some(),
        has_button_press_threshold: button_press_threshold.is_some(),
        // Windows HidP API doesn't expose the Constant flag from the descriptor;
        // assume writable if the usage exists — write errors are handled gracefully.
        input_mode_writable: input_mode.is_some(),
        surface_switch_writable: surface_switch.is_some(),
        button_switch_writable: button_switch.is_some(),
        latency_mode_writable: latency_mode.is_some(),
        button_press_threshold_writable: button_press_threshold.is_some(),
    };

    // Transfer handle ownership to WinHidDevice by opening a new one,
    // since WinHidDevice::open expects a path
    let _ = CloseHandle(handle);
    let device = match WinHidDevice::open(std::path::Path::new(&device_path)) {
        Ok(d) => d,
        Err(_) => {
            let _ = HidD_FreePreparsedData(preparsed_data);
            return None;
        }
    };

    eprintln!("config: found PTP features on {}", device_path);

    let mut backend = WindowsConfigBackend {
        device,
        preparsed: preparsed_data.0,
        feature_report_len: caps.FeatureReportByteLength as usize,
        input_mode,
        surface_switch,
        button_switch,
        contact_count_max,
        pad_type,
        latency_mode,
        button_press_threshold,
    };

    let values = backend.read_all();

    let mut config = PtpConfig {
        features,
        input_mode: values.input_mode,
        surface_switch: values.surface_switch,
        button_switch: values.button_switch,
        contact_count_max: values.contact_count_max,
        pad_type: values.pad_type,
        latency_mode: values.latency_mode,
        button_press_threshold: values.button_press_threshold,
        backend: Box::new(backend),
    };
    config.probe_writable();
    Some(config)
}
