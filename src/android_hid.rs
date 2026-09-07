//! Android USB transport: JNI calls into the Kotlin `UsbBridge`.
//!
//! Android has no `/dev/hidraw*`: USB access goes through
//! `android.hardware.usb.UsbManager`, which hands an app a permission-gated
//! connection object, not a device node it could open itself. So on Android
//! the actual transfers live in Kotlin
//! (`android/app/src/main/kotlin/me/danielschaefer/tapview/UsbBridge.kt`) and
//! this module is the Rust half of that seam: [`list_devices`] enumerates,
//! [`AndroidHidDevice`] carries feature reports (heatmap, PTP config) and
//! interrupt-IN input reports (touches, via [`spawn_reader`]).
//!
//! Claiming the pad's HID interface detaches Android's own `hid-multitouch`,
//! so while a device is open the app receives every report and the pad stops
//! moving the system pointer — the Linux grab, unconditionally.
//!
//! The bridge object is created once at startup ([`init`], called from
//! `android_main` in `android/rust`) from the `JavaVM` and activity pointers
//! `android-activity` exposes. Every Rust-side caller runs on a worker thread
//! (reader, heatmap, config, the picker's list/open threads) or the egui
//! thread, so each call attaches its thread to the VM permanently — the `jni`
//! crate detaches on thread exit — rather than juggling attach guards.

use std::io;
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use jni::objects::{GlobalRef, JByteArray, JClass, JObject, JString, JValue};
use jni::{JNIEnv, JavaVM};

use crate::heatmap::HidDevice;
use crate::hid::ReportLayout;
use crate::input::TouchState;
use crate::ptp::PtpParser;

/// The Kotlin class that owns everything USB, loaded through the activity's
/// class loader (a plain `FindClass` on a native thread only sees system
/// classes).
const BRIDGE_CLASS: &str = "me.danielschaefer.tapview.UsbBridge";

/// Timeout for the EP0 control transfers behind feature reports.
const FEATURE_TIMEOUT_MS: i32 = 1000;

/// `UsbBridge.open` status codes below zero (a non-negative value is a live
/// handle). Mirrors the constants at the top of `UsbBridge.kt`.
const OPEN_GONE: i32 = -1;
const OPEN_PERMISSION_PENDING: i32 = -2;
const OPEN_NO_INTERFACE: i32 = -3;
const OPEN_CLAIM_FAILED: i32 = -4;

/// Digitizer usage page / Touch Pad application usage: what makes a HID
/// interface a touchpad for us.
const DIGITIZER_PAGE: u16 = 0x0D;
const USAGE_TOUCH_PAD: u16 = 0x05;

struct Bridge {
    vm: JavaVM,
    bridge: GlobalRef,
}

static BRIDGE: OnceLock<Bridge> = OnceLock::new();

/// Wire this module to the running app: `vm` and `activity` are
/// `AndroidApp::vm_as_ptr()` / `activity_as_ptr()`, passed as raw pointers so
/// tapview itself needs no `android-activity` dependency (see `android/rust`,
/// which owns that and calls this before entering the GUI).
///
/// # Safety
///
/// `vm` must be the process's `JavaVM` and `activity` a valid reference to
/// the app's activity, both live for the rest of the process — which is what
/// `android-activity` guarantees for the pointers it exposes.
pub unsafe fn init(vm: *mut std::ffi::c_void, activity: *mut std::ffi::c_void) -> io::Result<()> {
    if BRIDGE.get().is_some() {
        return Ok(());
    }
    let vm = JavaVM::from_raw(vm.cast()).map_err(|e| jni_error("JavaVM", e))?;
    let mut env = vm
        .attach_current_thread_permanently()
        .map_err(|e| jni_error("attach", e))?;
    let activity = JObject::from_raw(activity as jni::sys::jobject);

    let loader = env
        .call_method(
            &activity,
            "getClassLoader",
            "()Ljava/lang/ClassLoader;",
            &[],
        )
        .and_then(|v| v.l())
        .map_err(|e| exception_error(&mut env, "getClassLoader", e))?;
    let name = env
        .new_string(BRIDGE_CLASS)
        .map_err(|e| jni_error("new_string", e))?;
    let class = env
        .call_method(
            &loader,
            "loadClass",
            "(Ljava/lang/String;)Ljava/lang/Class;",
            &[JValue::Object(&name)],
        )
        .and_then(|v| v.l())
        .map_err(|e| exception_error(&mut env, "loadClass(UsbBridge)", e))?;
    let bridge = env
        .new_object(
            JClass::from(class),
            "(Landroid/app/Activity;)V",
            &[JValue::Object(&activity)],
        )
        .map_err(|e| exception_error(&mut env, "new UsbBridge", e))?;
    let bridge = env
        .new_global_ref(bridge)
        .map_err(|e| jni_error("new_global_ref", e))?;

    let _ = BRIDGE.set(Bridge { vm, bridge });
    Ok(())
}

/// Run `f` with an attached `JNIEnv` and the bridge object.
fn with_env<R>(f: impl FnOnce(&mut JNIEnv, &JObject) -> io::Result<R>) -> io::Result<R> {
    let bridge = BRIDGE.get().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotConnected,
            "the Android USB bridge is not initialised",
        )
    })?;
    let mut env = bridge
        .vm
        .attach_current_thread_permanently()
        .map_err(|e| jni_error("attach", e))?;
    f(&mut env, bridge.bridge.as_obj())
}

/// A plain JNI-infrastructure failure (no Java exception involved).
fn jni_error(what: &str, e: jni::errors::Error) -> io::Error {
    io::Error::other(format!("USB bridge: {}: {}", what, e))
}

/// A failed Java call: clear any pending exception (leaving it pending would
/// poison every later JNI call on this thread) and fold its description into
/// the error.
fn exception_error(env: &mut JNIEnv, what: &str, e: jni::errors::Error) -> io::Error {
    let mut detail = String::new();
    if env.exception_check().unwrap_or(false) {
        if let Ok(throwable) = env.exception_occurred() {
            let _ = env.exception_clear();
            if let Ok(text) = env
                .call_method(&throwable, "toString", "()Ljava/lang/String;", &[])
                .and_then(|v| v.l())
            {
                if let Ok(text) = env.get_string(&JString::from(text)) {
                    detail = format!(": {}", text.to_string_lossy());
                }
            }
        }
    }
    io::Error::other(format!("USB bridge: {}: {}{}", what, e, detail))
}

/// Call a bridge method returning `String?`, as `Option<String>`.
fn call_string(
    env: &mut JNIEnv,
    bridge: &JObject,
    name: &'static str,
    sig: &str,
    args: &[JValue],
) -> io::Result<Option<String>> {
    let obj = env
        .call_method(bridge, name, sig, args)
        .and_then(|v| v.l())
        .map_err(|e| exception_error(env, name, e))?;
    if obj.is_null() {
        return Ok(None);
    }
    let jstr = JString::from(obj);
    let s = env.get_string(&jstr).map_err(|e| jni_error(name, e))?;
    Ok(Some(s.to_string_lossy().into_owned()))
}

/// Call a bridge method returning `ByteArray?`, as `Option<Vec<u8>>`.
fn call_bytes(
    env: &mut JNIEnv,
    bridge: &JObject,
    name: &'static str,
    sig: &str,
    args: &[JValue],
) -> io::Result<Option<Vec<u8>>> {
    let obj = env
        .call_method(bridge, name, sig, args)
        .and_then(|v| v.l())
        .map_err(|e| exception_error(env, name, e))?;
    if obj.is_null() {
        return Ok(None);
    }
    let bytes = env
        .convert_byte_array(JByteArray::from(obj))
        .map_err(|e| exception_error(env, name, e))?;
    Ok(Some(bytes))
}

/// One USB device as the bridge enumerates it (`UsbBridge.list`).
#[derive(Debug, Clone)]
pub struct UsbDeviceInfo {
    /// Android's device name, e.g. `/dev/bus/usb/001/002`.
    pub path: String,
    pub vendor_id: u16,
    pub product_id: u16,
    /// USB product string, when Android could read it.
    pub product: Option<String>,
    /// Whether the user has granted this app access. Descriptors can only be
    /// read once granted, so `touchpad` is `None` until then.
    pub granted: bool,
    /// Whether the device has any HID-class interface at all.
    pub has_hid: bool,
    /// The HID interface with a Touch Pad application collection, and its raw
    /// report descriptor. `None` if not granted, not probed, or not a touchpad.
    pub touchpad: Option<(i32, Vec<u8>)>,
}

impl UsbDeviceInfo {
    pub fn label(&self) -> String {
        match &self.product {
            Some(p) if !p.is_empty() => {
                format!("{} ({:04x}:{:04x})", p, self.vendor_id, self.product_id)
            }
            _ => format!("{:04x}:{:04x}", self.vendor_id, self.product_id),
        }
    }
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// Every attached USB device, with granted HID devices probed for a Touch
/// Pad interface. Does control transfers (and briefly claims interfaces) for
/// the probe, so call it from a worker thread, never per frame.
pub fn list_devices() -> io::Result<Vec<UsbDeviceInfo>> {
    // One device per line: path \t vid \t pid \t granted \t hid \t product
    let listing =
        with_env(|env, bridge| call_string(env, bridge, "list", "()Ljava/lang/String;", &[]))?
            .unwrap_or_default();
    let mut devices = Vec::new();
    for line in listing.lines().filter(|l| !l.is_empty()) {
        let mut cols = line.splitn(6, '\t');
        let (Some(path), Some(vid), Some(pid), Some(granted), Some(hid)) = (
            cols.next(),
            cols.next(),
            cols.next(),
            cols.next(),
            cols.next(),
        ) else {
            log::warn!("USB bridge: unparseable list line: {:?}", line);
            continue;
        };
        let product = cols.next().filter(|p| !p.is_empty()).map(str::to_string);
        let mut dev = UsbDeviceInfo {
            path: path.to_string(),
            vendor_id: vid.parse().unwrap_or(0),
            product_id: pid.parse().unwrap_or(0),
            product,
            granted: granted == "1",
            has_hid: hid == "1",
            touchpad: None,
        };
        if dev.granted && dev.has_hid {
            dev.touchpad = find_touchpad_interface(&dev.path)?;
        }
        devices.push(dev);
    }
    Ok(devices)
}

/// Read every HID interface's report descriptor and pick the one with a
/// Touch Pad application collection.
fn find_touchpad_interface(path: &str) -> io::Result<Option<(i32, Vec<u8>)>> {
    // One interface per line: iface \t hex-descriptor; null if gone/ungranted
    let listing = with_env(|env, bridge| {
        let jpath = env
            .new_string(path)
            .map_err(|e| jni_error("new_string", e))?;
        call_string(
            env,
            bridge,
            "descriptors",
            "(Ljava/lang/String;)Ljava/lang/String;",
            &[JValue::Object(&jpath)],
        )
    })?;
    let Some(listing) = listing else {
        return Ok(None);
    };
    for line in listing.lines().filter(|l| !l.is_empty()) {
        let Some((iface, hex)) = line.split_once('\t') else {
            continue;
        };
        if hex.starts_with('!') {
            // The bridge could claim this interface but not read its descriptor.
            log::warn!(
                "usb: {} interface {}: report descriptor unreadable",
                path,
                iface
            );
            continue;
        }
        let (Ok(iface), Some(desc)) = (iface.parse::<i32>(), decode_hex(hex)) else {
            continue;
        };
        let layout = ReportLayout::parse(&desc);
        let apps: Vec<String> = layout
            .collections
            .iter()
            .filter(|c| c.kind == crate::hid::Collection::APPLICATION)
            .map(|c| format!("{:04x}:{:04x}", c.usage_page, c.usage))
            .collect();
        log::info!(
            "usb: {} interface {}: {} byte descriptor, application collections [{}]",
            path,
            iface,
            desc.len(),
            apps.join(" ")
        );
        if layout.has_application_collection(DIGITIZER_PAGE, USAGE_TOUCH_PAD) {
            return Ok(Some((iface, desc)));
        }
    }
    Ok(None)
}

/// Ask Android to show the USB permission dialog for a device. The answer
/// arrives asynchronously: [`generation`] changes when it does.
pub fn request_permission(path: &str) -> io::Result<bool> {
    with_env(|env, bridge| {
        let jpath = env
            .new_string(path)
            .map_err(|e| jni_error("new_string", e))?;
        env.call_method(
            bridge,
            "requestPermission",
            "(Ljava/lang/String;)Z",
            &[JValue::Object(&jpath)],
        )
        .and_then(|v| v.z())
        .map_err(|e| exception_error(env, "requestPermission", e))
    })
}

/// A counter the bridge bumps on every USB attach/detach and permission
/// answer. Cheap; poll it per frame and re-list when it changes.
pub fn generation() -> i32 {
    with_env(|env, bridge| {
        env.call_method(bridge, "generation", "()I", &[])
            .and_then(|v| v.i())
            .map_err(|e| exception_error(env, "generation", e))
    })
    .unwrap_or(0)
}

/// The device the app was launched (or brought forward) for by a
/// `USB_DEVICE_ATTACHED` intent, once.
pub fn take_attached_path() -> Option<String> {
    with_env(|env, bridge| {
        call_string(env, bridge, "takeAttachedPath", "()Ljava/lang/String;", &[])
    })
    .ok()
    .flatten()
}

/// System-bar overlap in pixels at the top and bottom of the window, so the
/// UI can stay out from under the status bar and the gesture pill (winit
/// 0.30 exposes no safe-area insets). Zero when unknown.
pub fn insets_px() -> (f32, f32) {
    with_env(|env, bridge| call_string(env, bridge, "insets", "()Ljava/lang/String;", &[]))
        .ok()
        .flatten()
        .and_then(|s| {
            // "left top right bottom"
            let fields: Vec<f32> = s
                .split_whitespace()
                .map_while(|f| f.parse::<f32>().ok())
                .collect();
            match fields[..] {
                [_left, top, _right, bottom] => Some((top, bottom)),
                _ => None,
            }
        })
        .unwrap_or((0.0, 0.0))
}

/// Map a `UsbBridge.open` status to an error the picker can explain.
fn open_error(path: &str, status: i32) -> io::Error {
    match status {
        OPEN_GONE => io::Error::new(
            io::ErrorKind::NotFound,
            format!("USB device {} is gone (unplugged?)", path),
        ),
        OPEN_PERMISSION_PENDING => io::Error::new(
            io::ErrorKind::PermissionDenied,
            "waiting for USB permission: accept the dialog Android just showed, then retry",
        ),
        OPEN_NO_INTERFACE => io::Error::new(
            io::ErrorKind::Unsupported,
            format!("{} has no usable HID interface", path),
        ),
        OPEN_CLAIM_FAILED => io::Error::other(format!(
            "could not detach the system driver from {} (an OEM kernel may refuse)",
            path
        )),
        other => io::Error::other(format!(
            "opening {} failed (UsbBridge status {})",
            path, other
        )),
    }
}

struct Inner {
    handle: i32,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Releasing the interface lets the kernel rebind hid-multitouch: the
        // pad becomes a pointer again.
        let _ = with_env(|env, bridge| {
            env.call_method(bridge, "close", "(I)V", &[JValue::Int(self.handle)])
                .map_err(|e| exception_error(env, "close", e))?;
            Ok(())
        });
    }
}

/// One claimed HID interface of one USB device. Cloneable so the reader,
/// heatmap and config threads can share it; the interface is released when
/// the last clone goes away.
#[derive(Clone)]
pub struct AndroidHidDevice(Arc<Inner>);

impl AndroidHidDevice {
    /// Claim interface `iface` of the device at `path` (from
    /// [`UsbDeviceInfo::touchpad`]).
    pub fn open(path: &str, iface: i32) -> io::Result<Self> {
        let status = with_env(|env, bridge| {
            let jpath = env
                .new_string(path)
                .map_err(|e| jni_error("new_string", e))?;
            env.call_method(
                bridge,
                "open",
                "(Ljava/lang/String;I)I",
                &[JValue::Object(&jpath), JValue::Int(iface)],
            )
            .and_then(|v| v.i())
            .map_err(|e| exception_error(env, "open", e))
        })?;
        if status < 0 {
            return Err(open_error(path, status));
        }
        Ok(Self(Arc::new(Inner { handle: status })))
    }

    /// One interrupt-IN report into `buf` (report-ID byte first for numbered
    /// reports). `Ok(None)` on timeout; `NotFound` once the device is closed
    /// or detached.
    pub fn read_report(&self, buf: &mut [u8], timeout: Duration) -> io::Result<Option<usize>> {
        let data = with_env(|env, bridge| {
            call_bytes(
                env,
                bridge,
                "read",
                "(III)[B",
                &[
                    JValue::Int(self.0.handle),
                    JValue::Int(buf.len() as i32),
                    JValue::Int(timeout.as_millis().min(i32::MAX as u128) as i32),
                ],
            )
        })?;
        match data {
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "USB device closed or detached",
            )),
            Some(d) if d.is_empty() => Ok(None),
            Some(d) => {
                let n = d.len().min(buf.len());
                buf[..n].copy_from_slice(&d[..n]);
                Ok(Some(n))
            }
        }
    }
}

impl HidDevice for AndroidHidDevice {
    /// `buf[0]` is the report ID, hidapi/hidraw style; the wire carries it
    /// too for numbered reports.
    async fn set_feature(&self, buf: &[u8]) -> io::Result<()> {
        let ok = with_env(|env, bridge| {
            let data = env
                .byte_array_from_slice(buf)
                .map_err(|e| jni_error("byte_array", e))?;
            env.call_method(
                bridge,
                "setFeature",
                "(I[BI)Z",
                &[
                    JValue::Int(self.0.handle),
                    JValue::Object(data.as_ref()),
                    JValue::Int(FEATURE_TIMEOUT_MS),
                ],
            )
            .and_then(|v| v.z())
            .map_err(|e| exception_error(env, "setFeature", e))
        })?;
        if ok {
            Ok(())
        } else {
            Err(io::Error::other(
                "SET feature report failed (device unplugged?)",
            ))
        }
    }

    async fn get_feature(&self, buf: &mut [u8]) -> io::Result<usize> {
        let report_id = i32::from(buf.first().copied().unwrap_or(0));
        let data = with_env(|env, bridge| {
            call_bytes(
                env,
                bridge,
                "getFeature",
                "(IIII)[B",
                &[
                    JValue::Int(self.0.handle),
                    JValue::Int(report_id),
                    JValue::Int(buf.len() as i32),
                    JValue::Int(FEATURE_TIMEOUT_MS),
                ],
            )
        })?
        .ok_or_else(|| io::Error::other("GET feature report failed (device unplugged?)"))?;
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }
}

/// The touch reader thread: interrupt-IN reports through the PTP parser into
/// `tx`. Ends when the UI drops the session (`tx` closed) or the device goes
/// away (reported on `lost`).
pub fn spawn_reader(
    dev: AndroidHidDevice,
    mut parser: PtpParser,
    max_report_len: usize,
    tx: mpsc::Sender<TouchState>,
    lost: mpsc::Sender<String>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buf = vec![0u8; max_report_len.max(64)];
        loop {
            match dev.read_report(&mut buf, Duration::from_millis(250)) {
                Ok(Some(n)) => {
                    if let Some(state) = parser.feed(&buf[..n]) {
                        if tx.send(state).is_err() {
                            break; // session dropped
                        }
                    }
                }
                Ok(None) => {
                    // Timeout: also the moment to notice a dropped session.
                    if tx.send(TouchStateProbe::probe()).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    let _ = lost.send(e.to_string());
                    break;
                }
            }
        }
    })
}

/// A frame to send on idle so the reader notices a closed channel promptly.
/// Idle pads send nothing, and `Sender::send` is the only way to learn the
/// receiver is gone; an empty frame is what the parser would produce anyway.
struct TouchStateProbe;

impl TouchStateProbe {
    fn probe() -> TouchState {
        TouchState::default()
    }
}
