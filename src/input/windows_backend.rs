use super::{InputBackend, InputError, TouchState};
use crate::multitouch::{ButtonState, TouchData, MAX_TOUCH_POINTS};
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc;
use windows::core::PCWSTR;
use windows::Win32::Devices::HumanInterfaceDevice::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const HID_USAGE_PAGE_DIGITIZER: u16 = 0x0D;
const HID_USAGE_DIGITIZER_TOUCHPAD: u16 = 0x05;
use crate::ptp::MT_TOOL_PALM;

/// Windows RawInput-based touch backend.
///
/// Unlike the Linux evdev backend which processes events one at a time,
/// Windows delivers complete HID reports via WM_INPUT messages. Each report
/// contains all active contacts atomically.
pub struct WindowsBackend {
    touch_rx: mpsc::Receiver<TouchState>,
    _thread: Option<std::thread::JoinHandle<()>>,
}

impl InputBackend for WindowsBackend {
    fn open(device_path: &Path) -> Result<Self, InputError> {
        let (tx, rx) = mpsc::channel();
        let wanted = normalize_device_path(&device_path.to_string_lossy());

        let thread = std::thread::spawn(move || {
            if let Err(e) = run_rawinput_loop(tx, wanted) {
                log::error!("RawInput thread error: {}", e);
            }
        });

        Ok(Self {
            touch_rx: rx,
            _thread: Some(thread),
        })
    }

    fn grab(&mut self) -> Result<(), InputError> {
        // Not implemented on Windows - would need RIDEV_NOLEGACY or similar
        Ok(())
    }

    fn ungrab(&mut self) -> Result<(), InputError> {
        Ok(())
    }

    fn poll_events(&mut self) -> Result<Option<TouchState>, InputError> {
        match self.touch_rx.try_recv() {
            Ok(state) => Ok(Some(state)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => {
                Err(InputError::ReadError("RawInput thread died".to_string()))
            }
        }
    }
}

fn run_rawinput_loop(
    tx: mpsc::Sender<TouchState>,
    wanted_device: String,
) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        let hinstance = GetModuleHandleW(PCWSTR::null())?;

        // Register window class
        let class_name: Vec<u16> = "TapviewRawInput\0".encode_utf16().collect();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(raw_input_wnd_proc),
            hInstance: hinstance.into(),
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        RegisterClassExW(&wc);

        // Create a message-only window
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class_name.as_ptr()),
            PCWSTR::null(),
            WS_OVERLAPPEDWINDOW,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance.into()),
            None,
        )?;

        // RawInput registration is by HID usage, not by device, so every
        // attached touchpad's reports arrive at this window. Remember which one
        // the user picked so the rest get dropped.
        WANTED_DEVICE.with(|w| *w.borrow_mut() = Some(wanted_device));

        // Register for raw touchpad input
        let rid = RAWINPUTDEVICE {
            usUsagePage: HID_USAGE_PAGE_DIGITIZER,
            usUsage: HID_USAGE_DIGITIZER_TOUCHPAD,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        };

        RegisterRawInputDevices(&[rid], std::mem::size_of::<RAWINPUTDEVICE>() as u32)
            .map_err(|e| format!("RegisterRawInputDevices: {}", e))?;

        // Store sender in thread-local for the wndproc
        TX.set(Some(tx));

        // Message loop
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

thread_local! {
    static TX: std::cell::Cell<Option<mpsc::Sender<TouchState>>> = const { std::cell::Cell::new(None) };
    /// Normalized interface path of the touchpad the user selected.
    static WANTED_DEVICE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
    /// Per-device-handle verdict from `WANTED_DEVICE`, to keep the string
    /// comparison off the per-report path.
    static DEVICE_MATCH: std::cell::RefCell<HashMap<usize, bool>> = std::cell::RefCell::new(HashMap::new());
    /// HID descriptors are per device: parsing one touchpad's reports with
    /// another's caps yields garbage coordinates.
    static PREPARSED_CACHE: std::cell::RefCell<HashMap<usize, PreparsedCache>> = std::cell::RefCell::new(HashMap::new());
}

struct PreparsedCache {
    data: Vec<u8>,
    #[allow(dead_code)]
    caps: HIDP_CAPS,
    #[allow(dead_code)]
    value_caps: Vec<HIDP_VALUE_CAPS>,
    button_caps: Vec<HIDP_BUTTON_CAPS>,
    max_contacts: u32,
}

unsafe extern "system" fn raw_input_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_INPUT {
        let hrawinput = HRAWINPUT(lparam.0 as *mut std::ffi::c_void);
        handle_raw_input(hrawinput);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

unsafe fn handle_raw_input(hrawinput: HRAWINPUT) {
    // Get required buffer size
    let mut size = 0u32;
    let header_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    if GetRawInputData(hrawinput, RID_INPUT, None, &mut size, header_size) != 0 {
        return;
    }

    let mut buffer = vec![0u8; size as usize];
    let read = GetRawInputData(
        hrawinput,
        RID_INPUT,
        Some(buffer.as_mut_ptr() as *mut std::ffi::c_void),
        &mut size,
        header_size,
    );
    if read == u32::MAX {
        return;
    }

    let raw = &*(buffer.as_ptr() as *const RAWINPUT);
    if raw.header.dwType != RIM_TYPEHID.0 {
        return;
    }

    let hid = &raw.data.hid;
    let report_size = hid.dwSizeHid as usize;
    let report_count = hid.dwCount as usize;

    if report_size == 0 || report_count == 0 {
        return;
    }

    // Reports from the other attached touchpads are not ours to show.
    let device_handle = raw.header.hDevice;
    if !is_selected_device(device_handle) {
        return;
    }

    // Get or create preparsed data cache for this device
    ensure_preparsed_cache(device_handle);

    PREPARSED_CACHE.with(|cache| {
        let cache = cache.borrow();
        let cache = match cache.get(&(device_handle.0 as usize)) {
            Some(c) => c,
            None => return,
        };

        // The raw report data starts at bRawData[0]
        let raw_data_ptr = &hid.bRawData as *const u8;

        for report_idx in 0..report_count {
            let report_offset = report_idx * report_size;
            let report = std::slice::from_raw_parts(raw_data_ptr.add(report_offset), report_size);

            if let Some(state) = parse_touchpad_report(cache, report) {
                TX.with(|cell| {
                    let tx = cell.take();
                    if let Some(ref sender) = tx {
                        let _ = sender.send(state);
                    }
                    cell.set(tx);
                });
            }
        }
    });
}

/// Whether raw input from this device handle came from the touchpad the user
/// selected. Resolved once per handle from the interface path, which is the
/// only thing tying a RawInput device back to what `--list` showed.
unsafe fn is_selected_device(device_handle: HANDLE) -> bool {
    let key = device_handle.0 as usize;
    if let Some(known) = DEVICE_MATCH.with(|m| m.borrow().get(&key).copied()) {
        return known;
    }

    let wanted = WANTED_DEVICE.with(|w| w.borrow().clone());
    let name = raw_input_device_name(device_handle);
    let matches = match (&wanted, &name) {
        (Some(wanted), Some(name)) => normalize_device_path(name) == *wanted,
        // Nothing to compare against: behave as before and accept the reports
        // rather than showing nothing at all.
        (None, _) => true,
        (Some(_), None) => true,
    };

    if !matches {
        log::info!(
            "input: ignoring reports from another touchpad ({})",
            name.as_deref().unwrap_or("unknown device")
        );
    }

    DEVICE_MATCH.with(|m| m.borrow_mut().insert(key, matches));
    matches
}

/// The `\\?\HID#...` interface path RawInput knows a device handle by.
unsafe fn raw_input_device_name(device_handle: HANDLE) -> Option<String> {
    // Size first; for RIDI_DEVICENAME the size is in characters.
    let mut len = 0u32;
    if GetRawInputDeviceInfoW(Some(device_handle), RIDI_DEVICENAME, None, &mut len) != 0 {
        return None;
    }

    let mut buf = vec![0u16; len as usize];
    let read = GetRawInputDeviceInfoW(
        Some(device_handle),
        RIDI_DEVICENAME,
        Some(buf.as_mut_ptr() as *mut std::ffi::c_void),
        &mut len,
    );
    if read == u32::MAX {
        return None;
    }

    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// Normalize a HID interface path for comparison: SetupAPI and RawInput report
/// the same device with different casing (`\\?\hid#...` vs `\\?\HID#...`).
fn normalize_device_path(path: &str) -> String {
    path.strip_prefix(r"\\?\")
        .unwrap_or(path)
        .to_ascii_lowercase()
}

unsafe fn ensure_preparsed_cache(device_handle: HANDLE) {
    PREPARSED_CACHE.with(|cache| {
        if cache.borrow().contains_key(&(device_handle.0 as usize)) {
            return;
        }

        // Get preparsed data size
        let mut data_size = 0u32;
        if GetRawInputDeviceInfoW(
            Some(device_handle),
            RIDI_PREPARSEDDATA,
            None,
            &mut data_size,
        ) != 0
        {
            return;
        }

        let mut preparsed_buf = vec![0u8; data_size as usize];
        let read = GetRawInputDeviceInfoW(
            Some(device_handle),
            RIDI_PREPARSEDDATA,
            Some(preparsed_buf.as_mut_ptr() as *mut std::ffi::c_void),
            &mut data_size,
        );
        if read == u32::MAX {
            return;
        }

        let preparsed = PHIDP_PREPARSED_DATA(preparsed_buf.as_ptr() as isize);

        let mut caps = HIDP_CAPS::default();
        if HidP_GetCaps(preparsed, &mut caps) != HIDP_STATUS_SUCCESS {
            return;
        }

        // Get value caps
        let mut num_value_caps = caps.NumberInputValueCaps;
        let mut value_caps = vec![HIDP_VALUE_CAPS::default(); num_value_caps as usize];
        if num_value_caps > 0 {
            let _ = HidP_GetValueCaps(
                HidP_Input,
                value_caps.as_mut_ptr(),
                &mut num_value_caps,
                preparsed,
            );
            value_caps.truncate(num_value_caps as usize);
        }

        // Get button caps
        let mut num_button_caps = caps.NumberInputButtonCaps;
        let mut button_caps = vec![HIDP_BUTTON_CAPS::default(); num_button_caps as usize];
        if num_button_caps > 0 {
            let _ = HidP_GetButtonCaps(
                HidP_Input,
                button_caps.as_mut_ptr(),
                &mut num_button_caps,
                preparsed,
            );
            button_caps.truncate(num_button_caps as usize);
        }

        // Determine max contacts from Contact Count Maximum
        let max_contacts = value_caps
            .iter()
            .find(|vc| vc.UsagePage == 0x0D && vc.Anonymous.NotRange.Usage == 0x55)
            .map(|vc| vc.LogicalMax as u32)
            .unwrap_or(5);

        cache.borrow_mut().insert(
            device_handle.0 as usize,
            PreparsedCache {
                data: preparsed_buf,
                caps,
                value_caps,
                button_caps,
                max_contacts,
            },
        );
    });
}

unsafe fn parse_touchpad_report(cache: &PreparsedCache, report: &[u8]) -> Option<TouchState> {
    let preparsed = PHIDP_PREPARSED_DATA(cache.data.as_ptr() as isize);

    // Get Contact Count from this report
    let contact_count = get_usage_value(
        preparsed, 0x0D, // Digitizer
        0,    // Link collection 0 (top-level)
        0x54, // Contact Count
        report,
    )
    .unwrap_or(0);

    let mut touches = [TouchData::default(); MAX_TOUCH_POINTS];
    let mut buttons = ButtonState::default();

    // Check button state (touchpad click)
    check_buttons(cache, preparsed, report, &mut buttons);

    // Extract per-contact data
    // Each contact is in a separate link collection (typically 1, 2, 3, ...)
    let mut slot = 0usize;
    for link_collection in 1..=cache.max_contacts {
        if slot >= MAX_TOUCH_POINTS || slot >= contact_count as usize {
            break;
        }

        // Check if this contact has Tip Switch set (finger is touching)
        let tip_switch = get_button_state(
            cache,
            preparsed,
            0x0D,
            link_collection as u16,
            0x42, // Tip Switch
            report,
        );

        if !tip_switch && slot >= contact_count as usize {
            continue;
        }

        let touch = &mut touches[slot];

        // Position
        if let Some(x) = get_usage_value(preparsed, 0x01, link_collection as u16, 0x30, report) {
            touch.position_x = x as i32;
        }
        if let Some(y) = get_usage_value(preparsed, 0x01, link_collection as u16, 0x31, report) {
            touch.position_y = y as i32;
        }

        // Contact ID → tracking_id
        if let Some(id) = get_usage_value(preparsed, 0x0D, link_collection as u16, 0x51, report) {
            touch.tracking_id = id as i32;
        }

        // Pressure
        if let Some(p) = get_usage_value(preparsed, 0x0D, link_collection as u16, 0x30, report) {
            touch.pressure = p as i32;
        }

        // Width / Height → touch_major / touch_minor
        if let Some(w) = get_usage_value(preparsed, 0x0D, link_collection as u16, 0x48, report) {
            touch.touch_major = w as i32;
        }
        if let Some(h) = get_usage_value(preparsed, 0x0D, link_collection as u16, 0x49, report) {
            touch.touch_minor = h as i32;
        }

        // Confidence: if false, it's a palm
        let confidence = get_button_state(
            cache,
            preparsed,
            0x0D,
            link_collection as u16,
            0x47, // Confidence
            report,
        );
        if !confidence {
            touch.tool_type = MT_TOOL_PALM;
        }

        touch.used = tip_switch;
        touch.pressed = tip_switch;

        slot += 1;
    }

    Some(TouchState {
        touches,
        buttons,
        timestamp_us: Some(crate::input::host_now_us()),
        // Scan Time is not read here yet, so only the host rate is known.
        scan_time_us: None,
    })
}

unsafe fn get_usage_value(
    preparsed: PHIDP_PREPARSED_DATA,
    usage_page: u16,
    link_collection: u16,
    usage: u16,
    report: &[u8],
) -> Option<u32> {
    let mut value = 0u32;
    let status = HidP_GetUsageValue(
        HidP_Input,
        usage_page,
        Some(link_collection),
        usage,
        &mut value,
        preparsed,
        report,
    );
    if status == HIDP_STATUS_SUCCESS {
        Some(value)
    } else {
        None
    }
}

unsafe fn get_button_state(
    cache: &PreparsedCache,
    preparsed: PHIDP_PREPARSED_DATA,
    usage_page: u16,
    link_collection: u16,
    usage: u16,
    report: &[u8],
) -> bool {
    // Check if this button exists in button caps for this link collection
    let relevant = cache
        .button_caps
        .iter()
        .any(|bc| bc.UsagePage == usage_page && bc.LinkCollection == link_collection);

    if !relevant {
        // If no button caps for this collection, try with usage value
        return get_usage_value(preparsed, usage_page, link_collection, usage, report)
            .map(|v| v != 0)
            .unwrap_or(false);
    }

    let mut usage_list = [USAGE_AND_PAGE::default(); 64];
    let mut usage_count = usage_list.len() as u32;

    let status = HidP_GetUsagesEx(
        HidP_Input,
        Some(link_collection),
        usage_list.as_mut_ptr(),
        &mut usage_count,
        preparsed,
        report,
    );

    if status != HIDP_STATUS_SUCCESS {
        return false;
    }

    usage_list[..usage_count as usize]
        .iter()
        .any(|u| u.UsagePage == usage_page && u.Usage == usage)
}

unsafe fn check_buttons(
    cache: &PreparsedCache,
    preparsed: PHIDP_PREPARSED_DATA,
    report: &[u8],
    buttons: &mut ButtonState,
) {
    // Collect all link collections that have button caps (Usage Page 0x09)
    let mut collections_to_check: Vec<u16> = cache
        .button_caps
        .iter()
        .filter(|bc| bc.UsagePage == 0x09)
        .map(|bc| bc.LinkCollection)
        .collect();
    collections_to_check.dedup();

    // Always try the top-level collection (0) even if no explicit button caps
    if !collections_to_check.contains(&0) {
        collections_to_check.insert(0, 0);
    }

    for &link_collection in &collections_to_check {
        let mut usage_list = [USAGE_AND_PAGE::default(); 16];
        let mut usage_count = usage_list.len() as u32;

        let status = HidP_GetUsagesEx(
            HidP_Input,
            Some(link_collection),
            usage_list.as_mut_ptr(),
            &mut usage_count,
            preparsed,
            report,
        );

        if status != HIDP_STATUS_SUCCESS {
            continue;
        }

        for u in &usage_list[..usage_count as usize] {
            if u.UsagePage == 0x09 {
                // Button page
                match u.Usage {
                    1 => buttons.left = true,
                    2 => buttons.right = true,
                    3 => buttons.middle = true,
                    _ => {}
                }
            }
        }
    }

    // Also check via usage value as a fallback — some devices report
    // the button as a value rather than a button usage
    if !buttons.left {
        if let Some(v) = get_usage_value(preparsed, 0x09, 0, 1, report) {
            if v != 0 {
                buttons.left = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{windows_discovery::WindowsDiscovery, DeviceDiscovery};

    #[test]
    fn normalize_device_path_ignores_prefix_and_case() {
        assert_eq!(
            normalize_device_path(r"\\?\HID#PIXA3854&Col02#4&10d8260e&0&0001"),
            normalize_device_path(r"hid#pixa3854&col02#4&10d8260e&0&0001")
        );
    }

    /// The devnode `--list` shows and hands to `open` must be the same string
    /// RawInput reports for that device, or `is_selected_device` would drop
    /// every report and no touches would show up at all.
    ///
    /// Needs real hardware; skipped when no touchpad is attached (CI).
    #[test]
    fn discovered_touchpads_are_known_to_rawinput() {
        let Ok(devices) = WindowsDiscovery::find_touchpads() else {
            eprintln!("no touchpad attached, skipping");
            return;
        };

        let known: Vec<String> = unsafe { rawinput_device_names() }
            .iter()
            .map(|n| normalize_device_path(n))
            .collect();

        for d in &devices {
            let wanted = normalize_device_path(&d.devnode.to_string_lossy());
            assert!(
                known.contains(&wanted),
                "{} is not in the RawInput device list:\n{}",
                wanted,
                known.join("\n")
            );
        }
    }

    unsafe fn rawinput_device_names() -> Vec<String> {
        let entry_size = std::mem::size_of::<RAWINPUTDEVICELIST>() as u32;
        let mut count = 0u32;
        if GetRawInputDeviceList(None, &mut count, entry_size) == u32::MAX || count == 0 {
            return Vec::new();
        }

        let mut list = vec![RAWINPUTDEVICELIST::default(); count as usize];
        let written = GetRawInputDeviceList(Some(list.as_mut_ptr()), &mut count, entry_size);
        if written == u32::MAX {
            return Vec::new();
        }
        list.truncate(written as usize);

        list.iter()
            .filter(|e| e.dwType == RIM_TYPEHID)
            .filter_map(|e| raw_input_device_name(e.hDevice))
            .collect()
    }
}
