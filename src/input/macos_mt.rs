//! macOS: Apple trackpads through the private MultitouchSupport framework.
//!
//! Apple's own pads (built-in, Magic Trackpad) are not Precision Touchpads.
//! Their HID descriptor has an empty Touch Pad collection and ships the
//! touches in a 1751-byte vendor report (0x44) in an undocumented format.
//! MultitouchSupport is the framework that decodes it for the system; it
//! hands out finished contacts per frame.
//!
//! The framework is loaded with `dlopen`: it is private, so the SDK carries
//! no stubs to link against. The `MTTouch` layout and the function
//! signatures are the ones every public user of the framework relies on
//! (OpenMultitouchSupport, Fingerpaint); they have been stable since 10.5.

use super::{host_now_us, InputBackend, InputError, TouchState};
use crate::discovery::{Bus, DeviceInfo, Integration};
use crate::multitouch::{TouchData, MAX_TOUCH_POINTS};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// `DeviceInfo::devnode` prefix of a MultitouchSupport device; the rest is
/// its MT device ID in hex.
const PATH_PREFIX: &str = "MTDevice:";

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct MTPoint {
    x: f32,
    y: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct MTVector {
    position: MTPoint,
    velocity: MTPoint,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct MTTouch {
    frame: c_int,
    timestamp: f64,
    path_index: c_int,
    state: c_int,
    finger_id: c_int,
    hand_id: c_int,
    normalized: MTVector,
    z_total: f32,
    field9: c_int,
    angle: f32,
    major_axis: f32,
    minor_axis: f32,
    absolute: MTVector,
    field14: c_int,
    field15: c_int,
    z_density: f32,
}

const _: () = assert!(std::mem::size_of::<MTTouch>() == 96);

// MTTouch::state
const MT_MAKE_TOUCH: c_int = 4;
const MT_TOUCHING: c_int = 5;

type MTDeviceRef = *mut c_void;
type ContactCallback = extern "C" fn(MTDeviceRef, *const MTTouch, c_int, f64, c_int) -> c_int;

struct Api {
    create_list: unsafe extern "C" fn() -> *const c_void,
    get_device_id: unsafe extern "C" fn(MTDeviceRef, *mut u64) -> c_int,
    is_built_in: unsafe extern "C" fn(MTDeviceRef) -> bool,
    surface_dimensions: unsafe extern "C" fn(MTDeviceRef, *mut c_int, *mut c_int) -> c_int,
    register: unsafe extern "C" fn(MTDeviceRef, ContactCallback),
    unregister: unsafe extern "C" fn(MTDeviceRef, ContactCallback),
    start: unsafe extern "C" fn(MTDeviceRef, c_int),
    stop: unsafe extern "C" fn(MTDeviceRef),
}

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
}

const RTLD_LAZY: c_int = 0x1;

// The transmute targets are the `Api` field types.
#[allow(clippy::missing_transmute_annotations)]
fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| unsafe {
        let lib = dlopen(
            c"/System/Library/PrivateFrameworks/MultitouchSupport.framework/MultitouchSupport"
                .as_ptr(),
            RTLD_LAZY,
        );
        if lib.is_null() {
            log::warn!("multitouch: MultitouchSupport.framework not available");
            return None;
        }
        let sym = |name: &CStr| {
            let p = dlsym(lib, name.as_ptr());
            if p.is_null() {
                log::warn!("multitouch: missing symbol {}", name.to_string_lossy());
            }
            p
        };
        macro_rules! load {
            ($name:literal) => {{
                let p = sym($name);
                if p.is_null() {
                    return None;
                }
                std::mem::transmute::<*mut c_void, _>(p)
            }};
        }
        Some(Api {
            create_list: load!(c"MTDeviceCreateList"),
            get_device_id: load!(c"MTDeviceGetDeviceID"),
            is_built_in: load!(c"MTDeviceIsBuiltIn"),
            surface_dimensions: load!(c"MTDeviceGetSensorSurfaceDimensions"),
            register: load!(c"MTRegisterContactFrameCallback"),
            unregister: load!(c"MTUnregisterContactFrameCallback"),
            start: load!(c"MTDeviceStart"),
            stop: load!(c"MTDeviceStop"),
        })
    })
    .as_ref()
}

/// Every MT device with its ID. The list and the devices in it are never
/// released: the framework keeps its devices alive for the process anyway,
/// and a started device must outlive its callback.
fn devices(api: &Api) -> Vec<(u64, MTDeviceRef)> {
    unsafe {
        let list = (api.create_list)();
        if list.is_null() {
            return Vec::new();
        }
        (0..CFArrayGetCount(list))
            .filter_map(|i| {
                let dev = CFArrayGetValueAtIndex(list, i) as MTDeviceRef;
                let mut id = 0u64;
                ((api.get_device_id)(dev, &mut id) == 0).then_some((id, dev))
            })
            .collect()
    }
}

/// Whether `path` names a MultitouchSupport device rather than a HID one.
pub fn is_mt_path(path: &Path) -> bool {
    path.to_string_lossy().starts_with(PATH_PREFIX)
}

/// The trackpads MultitouchSupport knows about, built-in first.
pub fn find_touchpads() -> Vec<DeviceInfo> {
    let Some(api) = api() else {
        return Vec::new();
    };
    let mut found: Vec<DeviceInfo> = devices(api)
        .into_iter()
        .map(|(id, dev)| {
            let built_in = unsafe { (api.is_built_in)(dev) };
            DeviceInfo {
                devnode: PathBuf::from(format!("{}{:x}", PATH_PREFIX, id)),
                name: Some(
                    if built_in {
                        "Apple Internal Trackpad"
                    } else {
                        "Apple Multitouch Trackpad"
                    }
                    .to_string(),
                ),
                bus: Bus::Unknown,
                integration: if built_in {
                    Integration::Internal
                } else {
                    Integration::External
                },
                vendor_id: None,
                product_id: None,
            }
        })
        .collect();
    found.sort_by_key(|d| d.integration != Integration::Internal);
    found
}

/// One contact as the frame callback delivers it, in the pad's surface
/// coordinates (hundredths of a millimetre, origin top left).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Contact {
    id: i32,
    down: bool,
    x: i32,
    y: i32,
    major: i32,
    minor: i32,
    pressure: i32,
}

/// The one started device's frame sink. The callback carries no context
/// pointer, so it finds its channel here.
struct Sink {
    tx: mpsc::Sender<Vec<Contact>>,
    surface: (i32, i32),
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

extern "C" fn on_frame(
    _dev: MTDeviceRef,
    touches: *const MTTouch,
    count: c_int,
    _timestamp: f64,
    _frame: c_int,
) -> c_int {
    let Ok(guard) = SINK.lock() else {
        return 0;
    };
    let Some(sink) = guard.as_ref() else {
        return 0;
    };
    let touches = if touches.is_null() || count <= 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(touches, count as usize) }
    };
    let (w, h) = sink.surface;
    let contacts = touches
        .iter()
        .map(|t| Contact {
            id: t.path_index,
            down: matches!(t.state, MT_MAKE_TOUCH | MT_TOUCHING),
            x: (t.normalized.position.x * w as f32) as i32,
            // MT's y grows upwards.
            y: ((1.0 - t.normalized.position.y) * h as f32) as i32,
            major: (t.major_axis * 100.0) as i32,
            minor: (t.minor_axis * 100.0) as i32,
            pressure: (t.z_total * 100.0) as i32,
        })
        .collect();
    let _ = sink.tx.send(contacts);
    0
}

/// Slot table between frames, as [`crate::ptp::PtpParser`] keeps it: a
/// contact holds its slot and tracking id until it lifts.
#[derive(Default)]
struct Slots {
    slots: [TouchData; MAX_TOUCH_POINTS],
    ids: [Option<i32>; MAX_TOUCH_POINTS],
    next_tracking_id: i32,
}

impl Slots {
    fn frame(&mut self, contacts: &[Contact]) -> TouchState {
        let mut seen = [false; MAX_TOUCH_POINTS];
        for c in contacts.iter().filter(|c| c.down) {
            let slot = (0..MAX_TOUCH_POINTS)
                .find(|&s| self.ids[s] == Some(c.id))
                .or_else(|| (0..MAX_TOUCH_POINTS).find(|&s| self.ids[s].is_none() && !seen[s]));
            let Some(s) = slot else {
                continue;
            };
            if self.ids[s].is_none() {
                self.slots[s] = TouchData {
                    used: true,
                    tracking_id: self.next_tracking_id,
                    ..TouchData::default()
                };
                self.next_tracking_id += 1;
                self.ids[s] = Some(c.id);
            }
            seen[s] = true;
            let t = &mut self.slots[s];
            t.position_x = c.x;
            t.position_y = c.y;
            t.pressure = c.pressure;
            t.touch_major = c.major;
            t.touch_minor = c.minor;
        }
        for ((t, id), _) in self
            .slots
            .iter_mut()
            .zip(self.ids.iter_mut())
            .zip(seen)
            .filter(|(_, seen)| !seen)
        {
            *t = TouchData::default();
            *id = None;
        }
        let active = seen.iter().filter(|&&s| s).count();
        let mut touches = self.slots;
        touches[0].pressed = active > 0;
        touches[0].pressed_double = active == 2;
        TouchState {
            touches,
            timestamp_us: Some(host_now_us()),
            ..TouchState::default()
        }
    }
}

pub struct MtBackend {
    dev: usize,
    rx: mpsc::Receiver<Vec<Contact>>,
    slots: Slots,
    extents: (i32, i32),
}

// The MTDeviceRef is only handed back to the framework, which is
// thread-safe for start/stop.
unsafe impl Send for MtBackend {}

impl MtBackend {
    /// Surface size in hundredths of a millimetre, the unit touches come in.
    pub fn extents(&self) -> (i32, i32) {
        self.extents
    }
}

impl InputBackend for MtBackend {
    fn open(device_path: &Path) -> Result<Self, InputError> {
        let api = api().ok_or_else(|| {
            InputError::OpenFailed("MultitouchSupport.framework not available".to_string())
        })?;
        let wanted = device_path
            .to_string_lossy()
            .strip_prefix(PATH_PREFIX)
            .and_then(|id| u64::from_str_radix(id, 16).ok())
            .ok_or_else(|| {
                InputError::OpenFailed(format!("{}: not an MT device", device_path.display()))
            })?;
        let dev = devices(api)
            .into_iter()
            .find(|&(id, _)| id == wanted)
            .map(|(_, dev)| dev)
            .ok_or_else(|| {
                InputError::OpenFailed(format!("{}: device is gone", device_path.display()))
            })?;

        let (mut w, mut h) = (0, 0);
        unsafe { (api.surface_dimensions)(dev, &mut w, &mut h) };
        if w <= 0 || h <= 0 {
            return Err(InputError::OpenFailed(format!(
                "{}: no surface dimensions",
                device_path.display()
            )));
        }

        let (tx, rx) = mpsc::channel();
        {
            let mut sink = SINK.lock().expect("sink poisoned");
            if sink.is_some() {
                return Err(InputError::OpenFailed(
                    "another MT device is already open".to_string(),
                ));
            }
            *sink = Some(Sink {
                tx,
                surface: (w, h),
            });
        }
        unsafe {
            (api.register)(dev, on_frame);
            (api.start)(dev, 0);
        }
        Ok(Self {
            dev: dev as usize,
            rx,
            slots: Slots::default(),
            extents: (w, h),
        })
    }

    fn grab(&mut self) -> Result<(), InputError> {
        Err(InputError::GrabFailed(
            "not possible through MultitouchSupport".to_string(),
        ))
    }

    fn ungrab(&mut self) -> Result<(), InputError> {
        Ok(())
    }

    fn poll_events(&mut self) -> Result<Option<TouchState>, InputError> {
        match self.rx.recv_timeout(Duration::from_millis(5)) {
            Ok(contacts) => Ok(Some(self.slots.frame(&contacts))),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(InputError::ReadError("MT device stopped".to_string()))
            }
        }
    }
}

impl Drop for MtBackend {
    fn drop(&mut self) {
        if let Some(api) = api() {
            let dev = self.dev as MTDeviceRef;
            unsafe {
                (api.stop)(dev);
                (api.unregister)(dev, on_frame);
            }
        }
        if let Ok(mut sink) = SINK.lock() {
            *sink = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: i32, down: bool, x: i32) -> Contact {
        Contact {
            id,
            down,
            x,
            y: 0,
            major: 0,
            minor: 0,
            pressure: 0,
        }
    }

    #[test]
    fn contacts_keep_their_slot_and_tracking_id() {
        let mut s = Slots::default();
        let f = s.frame(&[c(7, true, 10)]);
        assert!(f.touches[0].used && f.touches[0].pressed);
        assert_eq!(f.touches[0].tracking_id, 0);

        let f = s.frame(&[c(7, true, 11), c(9, true, 50)]);
        assert_eq!(f.touches[0].position_x, 11);
        assert_eq!(f.touches[1].tracking_id, 1);
        assert!(f.touches[0].pressed_double);

        // 7 lifts; 9 stays in slot 1.
        let f = s.frame(&[c(7, false, 11), c(9, true, 51)]);
        assert!(!f.touches[0].used);
        assert_eq!(f.touches[1].tracking_id, 1);
        assert!(f.touches[0].pressed && !f.touches[0].pressed_double);

        let f = s.frame(&[]);
        assert!(f.touches.iter().all(|t| !t.used));
    }
}
