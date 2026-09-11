//! WebHID transport: the browser's route to a touchpad's HID device.
//!
//! Plays the role of hidraw (Linux) and the Kotlin USB bridge (Android):
//! [`open_session`] turns a granted `HIDDevice` into a [`Session`] — touch
//! reports through the PTP parser, heatmap loop, PTP config worker — and
//! [`WebHidDevice`] is the [`HidDevice`] the heatmap and config code drive.
//!
//! Three things differ from every native transport:
//!
//! - **Devices are granted, not enumerated.** A page only sees a device after
//!   the user picks it in the browser's prompt (`requestDevice`, which itself
//!   needs a click), and re-grants it silently on later visits
//!   (`getDevices`). [`connect`] runs that flow.
//! - **There is no raw report descriptor.** WebHID exposes the parsed
//!   `collections` tree instead; [`layout_from_collections`] rebuilds the
//!   [`ReportLayout`] the shared code expects from it. Chromium lists every
//!   report item on its top-level collection in descriptor order, which
//!   loses the per-finger Logical collections a PTP descriptor nests — so
//!   [`synthesize_finger_collections`] puts them back.
//! - **Nothing may block.** Every operation is a promise settled by the event
//!   loop of the one thread the UI shares, which is why [`HidDevice`] and
//!   the config worker are async; the touch path needs no loop at all, since
//!   input reports arrive as events.
//!
//! The bindings are hand-written `wasm_bindgen` externs: web-sys still gates
//! WebHID behind an unstable cfg and the slice of the API used here is small.

use std::cell::RefCell;
use std::io;
use std::rc::Rc;
use std::sync::mpsc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::config::layout_backend::{
    touchpad_physical_size, LayoutConfigBackend, PtpFields, KEY_BUTTON_PRESS_THRESHOLD,
    KEY_HAPTIC_INTENSITY,
};
use crate::config::{self, ConfigDescription, ConfigHandle};
use crate::heatmap::{self, HidDevice};
use crate::hid::{Collection, ReportField, ReportKind, ReportLayout};
use crate::input::TouchState;
use crate::ptp::{synthesize_finger_collections, PtpLayout, PtpParser};
use crate::session::Session;

const DIGITIZER: u16 = 0x0D;
const USAGE_TOUCH_PAD: u16 = 0x05;
/// The PTP device's Configuration collection. A touchpad is recognized by
/// this where the Touch Pad collection is not on offer: Chromium on Windows
/// withholds the latter (the Precision Touchpad driver holds it) but passes
/// this one and the vendor collection through, which is enough for the
/// heatmap and the config panel.
const USAGE_CONFIGURATION: u16 = 0x0E;

#[wasm_bindgen]
extern "C" {
    /// `navigator.hid`.
    type Hid;

    #[wasm_bindgen(method, js_name = requestDevice)]
    fn request_device(this: &Hid, options: &JsValue) -> js_sys::Promise;

    #[wasm_bindgen(method, js_name = getDevices)]
    fn get_devices(this: &Hid) -> js_sys::Promise;

    #[wasm_bindgen(method, js_name = addEventListener)]
    fn add_event_listener(this: &Hid, kind: &str, callback: &js_sys::Function);

    #[wasm_bindgen(method, js_name = removeEventListener)]
    fn remove_event_listener(this: &Hid, kind: &str, callback: &js_sys::Function);

    /// One granted `HIDDevice`: on every platform Chromium merges the
    /// collections of one physical device into one object.
    #[derive(Clone)]
    pub type JsHidDevice;

    #[wasm_bindgen(method, getter, js_name = vendorId)]
    fn vendor_id(this: &JsHidDevice) -> u16;

    #[wasm_bindgen(method, getter, js_name = productId)]
    fn product_id(this: &JsHidDevice) -> u16;

    #[wasm_bindgen(method, getter, js_name = productName)]
    fn product_name(this: &JsHidDevice) -> String;

    #[wasm_bindgen(method, getter)]
    fn opened(this: &JsHidDevice) -> bool;

    #[wasm_bindgen(method, getter)]
    fn collections(this: &JsHidDevice) -> js_sys::Array;

    #[wasm_bindgen(method)]
    fn open(this: &JsHidDevice) -> js_sys::Promise;

    #[wasm_bindgen(method)]
    fn close(this: &JsHidDevice) -> js_sys::Promise;

    #[wasm_bindgen(method, js_name = sendFeatureReport)]
    fn send_feature_report(this: &JsHidDevice, report_id: u8, data: &[u8]) -> js_sys::Promise;

    #[wasm_bindgen(method, js_name = receiveFeatureReport)]
    fn receive_feature_report(this: &JsHidDevice, report_id: u8) -> js_sys::Promise;

    #[wasm_bindgen(method, setter, js_name = oninputreport)]
    fn set_oninputreport(this: &JsHidDevice, callback: Option<&js_sys::Function>);
}

// ── JS plumbing ─────────────────────────────────────────────────────────────

/// A rejected promise as an error message naming the operation. Exceptions
/// rarely stringify via `as_string()`; `Debug` renders them all.
fn js_error(what: &str, e: &JsValue) -> String {
    let detail = e
        .as_string()
        .unwrap_or_else(|| format!("{:?}", e).chars().take(200).collect());
    format!("{what}: {detail}")
}

fn js_io_error(what: &str, e: &JsValue) -> io::Error {
    io::Error::other(js_error(what, e))
}

fn get(obj: &JsValue, key: &str) -> JsValue {
    js_sys::Reflect::get(obj, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

fn num(obj: &JsValue, key: &str) -> f64 {
    get(obj, key).as_f64().unwrap_or(0.0)
}

fn boolean(obj: &JsValue, key: &str) -> bool {
    get(obj, key).as_bool().unwrap_or(false)
}

fn array(obj: &JsValue, key: &str) -> js_sys::Array {
    let v = get(obj, key);
    if v.is_array() {
        v.unchecked_into()
    } else {
        js_sys::Array::new()
    }
}

fn dataview_bytes(v: &JsValue) -> Option<Vec<u8>> {
    let view = v.dyn_ref::<js_sys::DataView>()?;
    Some(
        js_sys::Uint8Array::new_with_byte_offset_and_length(
            &view.buffer(),
            view.byte_offset() as u32,
            view.byte_length() as u32,
        )
        .to_vec(),
    )
}

/// `navigator.hid`, or a clear answer for browsers that have no WebHID.
fn hid() -> Result<Hid, String> {
    let navigator = web_sys::window()
        .ok_or_else(|| "no window object (not running in a browser?)".to_string())?
        .navigator();
    let hid = get(navigator.as_ref(), "hid");
    if hid.is_undefined() || hid.is_null() {
        return Err(
            "this browser has no WebHID; use a Chromium-based browser on a desktop \
             (Chrome, Edge, Opera)"
                .to_string(),
        );
    }
    Ok(hid.unchecked_into())
}

/// Whether this browser has WebHID at all.
pub fn supported() -> bool {
    hid().is_ok()
}

// ── Descriptor model from HIDDevice.collections ─────────────────────────────

/// Re-encode a `HIDReportItem`'s unit fields as the raw HID Unit item, so
/// [`crate::hid::physical_range_mm`] can decode it like a native descriptor.
fn unit_from_item(item: &JsValue) -> u32 {
    let system = match get(item, "unitSystem").as_string().as_deref() {
        Some("si-linear") => 1,
        Some("si-rotation") => 2,
        Some("english-linear") => 3,
        Some("english-rotation") => 4,
        Some("vendor-defined") => 0x0F,
        _ => 0,
    };
    let nibble = |key: &str, shift: u32| (((num(item, key) as i32) & 0x0F) as u32) << shift;
    system
        | nibble("unitFactorLengthExponent", 4)
        | nibble("unitFactorMassExponent", 8)
        | nibble("unitFactorTimeExponent", 12)
        | nibble("unitFactorTemperatureExponent", 16)
        | nibble("unitFactorCurrentExponent", 20)
        | nibble("unitFactorLuminousIntensityExponent", 24)
}

/// The usages of one item, expanded the way the descriptor walker does:
/// explicit list, or Usage Minimum..Maximum for a range item.
fn item_usages(item: &JsValue) -> Vec<(u16, u16)> {
    let split = |v: f64| {
        let v = v as u32;
        ((v >> 16) as u16, v as u16)
    };
    if boolean(item, "isRange") {
        let (page, min) = split(num(item, "usageMinimum"));
        let (_, max) = split(num(item, "usageMaximum"));
        (min..=max).map(|u| (page, u)).collect()
    } else {
        array(item, "usages")
            .iter()
            .filter_map(|u| u.as_f64())
            .map(split)
            .collect()
    }
}

struct LayoutBuilder {
    fields: Vec<ReportField>,
    collections: Vec<Collection>,
}

impl LayoutBuilder {
    fn walk(&mut self, collection: &JsValue, parent: Option<usize>) {
        let idx = self.collections.len();
        self.collections.push(Collection {
            parent,
            kind: num(collection, "type") as u8,
            usage_page: num(collection, "usagePage") as u16,
            usage: num(collection, "usage") as u16,
        });
        for (kind, key) in [
            (ReportKind::Input, "inputReports"),
            (ReportKind::Output, "outputReports"),
            (ReportKind::Feature, "featureReports"),
        ] {
            for report in array(collection, key).iter() {
                let report_id = num(&report, "reportId") as u8;
                self.add_report(idx, kind, report_id, &array(&report, "items"));
            }
        }
        for child in array(collection, "children").iter() {
            self.walk(&child, Some(idx));
        }
    }

    /// Lay out one report's items, expanding `reportCount` into one field each.
    fn add_report(&mut self, owner: usize, kind: ReportKind, report_id: u8, items: &js_sys::Array) {
        let mut offset = 0usize;
        for item in items.iter() {
            let constant = boolean(&item, "isConstant");
            let variable = !boolean(&item, "isArray");
            let bit_size = num(&item, "reportSize") as usize;
            let count = num(&item, "reportCount") as usize;
            let usages = item_usages(&item);
            let logical_min = num(&item, "logicalMinimum") as i32;
            let logical_max = num(&item, "logicalMaximum") as i32;
            let physical_min = num(&item, "physicalMinimum") as i32;
            let physical_max = num(&item, "physicalMaximum") as i32;
            let unit = unit_from_item(&item);
            // Chromium hands the exponent decoded (-8..7); re-encode it as the
            // raw nibble the shared decoder expects (a raw value survives too).
            let unit_exponent = (num(&item, "unitExponent") as i32) & 0x0F;

            for i in 0..count {
                let (usage_page, usage) = if constant && usages.is_empty() {
                    (0, 0)
                } else if variable {
                    usages.get(i).or(usages.last()).copied().unwrap_or((0, 0))
                } else {
                    usages.first().copied().unwrap_or((0, 0))
                };

                self.fields.push(ReportField {
                    kind,
                    report_id,
                    usage_page,
                    usage,
                    collection: Some(owner),
                    bit_offset: offset + i * bit_size,
                    bit_size,
                    constant,
                    variable,
                    logical_min,
                    logical_max,
                    physical_min,
                    physical_max,
                    unit,
                    unit_exponent,
                });
            }
            offset += count * bit_size;
        }
    }
}

/// Rebuild the descriptor model from a `HIDDevice`'s parsed `collections`.
pub fn layout_from_collections(collections: &js_sys::Array) -> ReportLayout {
    let mut b = LayoutBuilder {
        fields: Vec::new(),
        collections: Vec::new(),
    };
    for c in collections.iter() {
        b.walk(&c, None);
    }
    let mut layout = ReportLayout::from_parts(b.fields, b.collections);
    synthesize_finger_collections(&mut layout);
    layout
}

// ── Granting and listing ────────────────────────────────────────────────────

/// A touchpad this page has been granted.
#[derive(Clone)]
pub struct Granted {
    device: JsHidDevice,
    pub layout: ReportLayout,
    pub name: String,
    pub vendor_id: u16,
    pub product_id: u16,
}

impl Granted {
    pub fn label(&self) -> String {
        if self.name.is_empty() {
            format!("{:04x}:{:04x}", self.vendor_id, self.product_id)
        } else {
            format!(
                "{} ({:04x}:{:04x})",
                self.name, self.vendor_id, self.product_id
            )
        }
    }
}

/// Ask the user for a touchpad (`request_new`) and/or list every touchpad
/// this page was already granted.
///
/// `requestDevice` needs transient user activation, so `request_new` must
/// come from a click; the `getDevices` pass does not, and runs at startup so
/// a pad granted on an earlier visit comes back on its own. Cancelling the
/// prompt is not an error: the list is simply whatever was granted before.
pub async fn connect(request_new: bool) -> Result<Vec<Granted>, String> {
    let hid = hid()?;

    if request_new {
        // Only touchpads appear in the prompt: a Touch Pad collection, or —
        // where the browser withholds it — the PTP Configuration collection.
        let options = js_sys::JSON::parse(&format!(
            r#"{{"filters":[{{"usagePage":{DIGITIZER},"usage":{USAGE_TOUCH_PAD}}},{{"usagePage":{DIGITIZER},"usage":{USAGE_CONFIGURATION}}}]}}"#
        ))
        .expect("static filter JSON parses");
        JsFuture::from(hid.request_device(&options))
            .await
            .map_err(|e| js_error("requesting a HID device", &e))?;
    }

    let devices = JsFuture::from(hid.get_devices())
        .await
        .map_err(|e| js_error("listing granted HID devices", &e))?;
    let devices: js_sys::Array = devices.unchecked_into();

    log::info!("webhid: {} granted device(s)", devices.length());

    let mut granted = Vec::new();
    for device in devices.iter() {
        let device: JsHidDevice = device.unchecked_into();
        let layout = layout_from_collections(&device.collections());
        let accepted = layout.has_application_collection(DIGITIZER, USAGE_TOUCH_PAD)
            || layout.has_application_collection(DIGITIZER, USAGE_CONFIGURATION);
        // What the browser handed over and what was made of it. Granted
        // devices are never enumerated, so without this there is no way to
        // see why a pad was passed over — the top-level collections are
        // exactly what that decision is made on.
        log::info!(
            "webhid: {} {:04x}:{:04x} [{}] feature reports {:02x?} -> {}",
            device.product_name(),
            device.vendor_id(),
            device.product_id(),
            layout
                .collections
                .iter()
                .filter(|c| c.parent.is_none())
                .map(|c| format!("{:04x}:{:04x} type {}", c.usage_page, c.usage, c.kind))
                .collect::<Vec<_>>()
                .join(", "),
            layout.report_ids(ReportKind::Feature),
            if accepted { "touchpad" } else { "skipped" }
        );
        if !accepted {
            continue;
        }
        granted.push(Granted {
            name: device.product_name(),
            vendor_id: device.vendor_id(),
            product_id: device.product_id(),
            layout,
            device,
        });
    }
    Ok(granted)
}

thread_local! {
    /// Bumped by the page-wide `connect`/`disconnect` listeners so the
    /// picker knows to re-list; `None` until [`watch_hotplug`] installed them.
    static HOTPLUG: RefCell<Option<Hotplug>> = const { RefCell::new(None) };
}

struct Hotplug {
    generation: u32,
    _closures: Vec<Closure<dyn FnMut(JsValue)>>,
}

/// Install page-wide HID hot-plug listeners (once). Returns whether WebHID
/// exists at all.
pub fn watch_hotplug() -> bool {
    let Ok(hid) = hid() else {
        return false;
    };
    HOTPLUG.with_borrow_mut(|slot| {
        if slot.is_some() {
            return;
        }
        let mut closures = Vec::new();
        for kind in ["connect", "disconnect"] {
            let closure = Closure::<dyn FnMut(JsValue)>::new(move |_event: JsValue| {
                HOTPLUG.with_borrow_mut(|slot| {
                    if let Some(h) = slot.as_mut() {
                        h.generation += 1;
                    }
                });
            });
            hid.add_event_listener(kind, closure.as_ref().unchecked_ref());
            closures.push(closure);
        }
        *slot = Some(Hotplug {
            generation: 0,
            _closures: closures,
        });
    });
    true
}

/// How many hot-plug events the page has seen; changes mean "re-list".
pub fn hotplug_generation() -> u32 {
    HOTPLUG.with_borrow(|slot| slot.as_ref().map_or(0, |h| h.generation))
}

// ── Feature reports ─────────────────────────────────────────────────────────

/// The feature-report transport for one opened device: what the heatmap
/// protocol and the PTP config backend drive.
#[derive(Clone)]
pub struct WebHidDevice {
    device: JsHidDevice,
}

impl HidDevice for WebHidDevice {
    /// `buf` is in hidraw framing — report ID first — which WebHID wants split.
    async fn set_feature(&self, buf: &[u8]) -> io::Result<()> {
        let (id, data) = buf
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty report"))?;
        JsFuture::from(self.device.send_feature_report(*id, data))
            .await
            .map_err(|e| js_io_error("sending a feature report", &e))?;
        Ok(())
    }

    /// Chromium returns the data with the report ID prepended, matching
    /// hidraw, so it is copied into `buf` from the front.
    async fn get_feature(&self, buf: &mut [u8]) -> io::Result<usize> {
        let id = *buf
            .first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty buffer"))?;
        let data = JsFuture::from(self.device.receive_feature_report(id))
            .await
            .map_err(|e| js_io_error("receiving a feature report", &e))?;
        let bytes = dataview_bytes(&data)
            .ok_or_else(|| io::Error::other("receiveFeatureReport returned no DataView"))?;
        let n = bytes.len().min(buf.len());
        buf[..n].copy_from_slice(&bytes[..n]);
        Ok(n)
    }
}

// ── Sessions ────────────────────────────────────────────────────────────────

/// Lives in [`Session::guard`]: the open device and the event handlers that
/// feed it. Dropping it unhooks the handlers and closes the device, so a
/// later connect can open it again.
struct SessionGuard {
    device: JsHidDevice,
    hid: Hid,
    /// `None` on a heatmap-only session, which hooks no input reports.
    _input: Option<Closure<dyn FnMut(JsValue)>>,
    disconnect: Closure<dyn FnMut(JsValue)>,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.device.set_oninputreport(None);
        self.hid
            .remove_event_listener("disconnect", self.disconnect.as_ref().unchecked_ref());
        // Fire and forget; a rejection (already gone) is of no interest.
        let _ = self.device.close();
    }
}

/// Open a granted touchpad and start everything that reads from it.
pub async fn open_session(granted: &Granted) -> Result<Session, String> {
    let hid = hid()?;
    let device = granted.device.clone();
    let layout = &granted.layout;
    // No Touch Pad collection means no touches. That is not fatal: Chromium on
    // Windows withholds it while passing the vendor and configuration
    // collections through, which still makes a heatmap-only session. Whether
    // anything at all is on offer is decided once those are built, below.
    let ptp = PtpLayout::from_layout(layout);

    // `opened` is per page, so `true` here means an earlier session of ours
    // whose fire-and-forget close() has not settled yet: finish that first,
    // else open() rejects with InvalidStateError.
    if device.opened() {
        let _ = JsFuture::from(device.close()).await;
    }
    JsFuture::from(device.open()).await.map_err(|e| {
        format!(
            "{} (in use by another tab, or — on Linux — missing hidraw permission)",
            js_error("opening the device", &e)
        )
    })?;
    let extents = ptp.as_ref().map(|ptp| (ptp.x_max, ptp.y_max));

    // Touches: every input report of the touch report ID through the parser.
    // The parser expects hidraw framing (report ID first for numbered reports);
    // WebHID hands the ID separately.
    let (touch_rx, input) = match ptp {
        Some(ptp) => {
            log::info!(
                "webhid: opened {} (touch report {}, {} finger slots, up to {} contacts)",
                granted.label(),
                ptp.report_id,
                ptp.fingers.len(),
                ptp.contact_count_max
            );
            let touch_report_id = ptp.report_id;
            let (touch_tx, touch_rx) = mpsc::channel::<TouchState>();
            let parser = Rc::new(RefCell::new(PtpParser::new(ptp)));
            let input = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
                let report_id = num(&event, "reportId") as u8;
                if report_id != touch_report_id {
                    return;
                }
                let Some(data) = dataview_bytes(&get(&event, "data")) else {
                    return;
                };
                let mut report = Vec::with_capacity(data.len() + 1);
                if report_id != 0 {
                    report.push(report_id);
                }
                report.extend_from_slice(&data);
                if let Some(state) = parser.borrow_mut().feed(&report) {
                    // A closed channel means the session was dropped; the
                    // guard's Drop unhooks this handler right after.
                    let _ = touch_tx.send(state);
                }
            });
            device.set_oninputreport(Some(input.as_ref().unchecked_ref()));
            (Some(touch_rx), Some(input))
        }
        None => {
            log::info!(
                "webhid: opened {} with no Touch Pad collection: heatmap-only",
                granted.label()
            );
            (None, None)
        }
    };

    // Unplug: `navigator.hid` fires `disconnect` with the device.
    let (lost_tx, lost_rx) = mpsc::channel::<String>();
    let disconnect = {
        let device = device.clone();
        Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
            if js_sys::Object::is(&get(&event, "device"), device.as_ref()) {
                let _ = lost_tx.send("device disconnected".to_string());
            }
        })
    };
    hid.add_event_listener("disconnect", disconnect.as_ref().unchecked_ref());

    let hid_dev = WebHidDevice {
        device: device.clone(),
    };

    // Heatmap: only PixArt pads answer the chip identification; on anything
    // else the task logs and ends and the panel simply never appears.
    let heatmap_stream = match heatmap::discovery::burst_report_length(layout) {
        Ok(burst_len) => {
            log::info!("heatmap: burst report length = {}", burst_len);
            let (tx, rx) = mpsc::channel();
            let control = heatmap::backend::HeatmapControl::default();
            let dev = hid_dev.clone();
            let loop_control = control.clone();
            wasm_bindgen_futures::spawn_local(async move {
                heatmap::backend::run_heatmap_loop(
                    &dev,
                    burst_len,
                    None,
                    &tx,
                    loop_control,
                    || gloo_timers::future::TimeoutFuture::new(50),
                )
                .await;
            });
            Some(heatmap::backend::HeatmapStream::new(rx, control))
        }
        Err(e) => {
            log::info!("heatmap: not available: {}", e);
            None
        }
    };

    // PTP config, when the descriptor declares any of its fields.
    let fields = PtpFields::from_layout(layout);
    let config = match LayoutConfigBackend::new(hid_dev.clone(), fields.clone()) {
        Some(mut backend) => {
            let mut description = ConfigDescription {
                features: fields.features(),
                button_press_threshold_range: fields.range(KEY_BUTTON_PRESS_THRESHOLD),
                haptic_intensity_range: fields.range(KEY_HAPTIC_INTENSITY),
                physical_size: touchpad_physical_size(layout),
            };
            let state = config::initialize(&mut backend, &mut description).await;
            log::info!("config: PTP configuration available");
            let (cmd_tx, cmd_rx) = mpsc::channel();
            let (evt_tx, evt_rx) = mpsc::channel();
            wasm_bindgen_futures::spawn_local(config::run_config_worker_polling(
                backend,
                cmd_rx,
                evt_tx,
                || gloo_timers::future::TimeoutFuture::new(30),
            ));
            Some(ConfigHandle::new(description, state, cmd_tx, evt_rx))
        }
        None => None,
    };

    // Nothing on offer at all. Building the guard and dropping it right away
    // is the teardown: it unhooks the disconnect listener and closes the
    // device, so a later attempt can open it again.
    if touch_rx.is_none() && heatmap_stream.is_none() && config.is_none() {
        drop(SessionGuard {
            device,
            hid,
            _input: input,
            disconnect,
        });
        return Err(format!(
            "{} exposes no touches, heatmap or configuration to the browser",
            granted.label()
        ));
    }

    Ok(Session {
        name: granted.label(),
        touch_rx,
        grab_tx: None,
        heatmap: heatmap_stream,
        config,
        extents,
        recorder: None,
        lost: Some(lost_rx),
        guard: Some(Box::new(SessionGuard {
            device,
            hid,
            _input: input,
            disconnect,
        })),
    })
}
