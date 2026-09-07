//! The Android front end: the same [`TapviewApp`] as the desktop, started
//! without a device, with a USB device picker on the no-session screen.
//!
//! Called from `android_main` in `android/rust`. Everything USB goes through
//! [`crate::android_hid`]; this module assembles a [`Session`] from an opened
//! touchpad (touch reader, heatmap loop, config worker) and drives the picker
//! state machine: re-list on every USB attach/detach/permission change,
//! auto-open when exactly one touchpad is present or when the app was
//! launched for a device, and explain failures with a retry.

use crate::android_hid::{self, AndroidHidDevice, UsbDeviceInfo};
use crate::app::{NoSessionUi, SessionRequest, TapviewApp};
use crate::config::layout_backend::{
    touchpad_physical_size, LayoutConfigBackend, PtpFields, KEY_BUTTON_PRESS_THRESHOLD,
    KEY_HAPTIC_INTENSITY,
};
use crate::config::{self, ConfigDescription, ConfigHandle};
use crate::heatmap;
use crate::hid::{ReportKind, ReportLayout};
use crate::ptp::{PtpLayout, PtpParser};
use crate::recording::Recording;
use crate::session::Session;
use std::sync::mpsc;
use std::thread;

/// The bundled demo recording (a Framework 13 pad), for a phone without a
/// touchpad attached.
const DEMO_RECORDING: &[u8] = include_bytes!("../testdata/sample.tapv");

/// Run the app with caller-built `NativeOptions` (`android_app` filled in).
pub fn run_with(options: eframe::NativeOptions) -> eframe::Result {
    eframe::run_native(
        "Tapview",
        options,
        Box::new(|_cc| {
            let mut picker = UsbPicker::default();
            Ok(Box::new(
                TapviewApp::new(20)
                    .with_no_session_ui(NoSessionUi {
                        message: "Plug a touchpad into the USB-C port".to_string(),
                        controls: Some(Box::new(move |ui| picker.draw(ui))),
                    })
                    .with_frame_hook(Box::new(reserve_insets)),
            ))
        }),
    )
}

/// Keep the UI out from under the status bar and the gesture pill: winit
/// exposes no safe-area insets, so reserve exactly the overlap the window
/// reports with empty panels. First-added top/bottom panels sit outermost.
fn reserve_insets(ctx: &egui::Context, _app: &mut TapviewApp) {
    let ppp = ctx.pixels_per_point();
    let (top, bottom) = android_hid::insets_px();
    if top > 0.0 {
        egui::TopBottomPanel::top("android-status-inset")
            .exact_height(top / ppp)
            .show_separator_line(false)
            .show(ctx, |_| {});
    }
    if bottom > 0.0 {
        egui::TopBottomPanel::bottom("android-gesture-inset")
            .exact_height(bottom / ppp)
            .show_separator_line(false)
            .show(ctx, |_| {});
    }
}

/// Open a touchpad and start everything that reads from it. Blocking (USB
/// transfers, the config probe): run on a worker thread.
pub fn open_session(dev: &UsbDeviceInfo) -> Result<Session, String> {
    let (iface, desc) = dev
        .touchpad
        .clone()
        .ok_or_else(|| "not a touchpad".to_string())?;
    let layout = ReportLayout::parse(&desc);
    let ptp = PtpLayout::from_layout(&layout)
        .ok_or_else(|| "no Touch Pad collection in report descriptor".to_string())?;
    let hid = AndroidHidDevice::open(&dev.path, iface).map_err(|e| e.to_string())?;
    log::info!(
        "usb: opened {} interface {} (touch report {}, {} finger slots, up to {} contacts)",
        dev.label(),
        iface,
        ptp.report_id,
        ptp.fingers.len(),
        ptp.contact_count_max
    );

    let extents = Some((ptp.x_max, ptp.y_max));
    let max_report_len = layout
        .report_ids(ReportKind::Input)
        .iter()
        .map(|&id| layout.report_bytes(ReportKind::Input, id))
        .max()
        .unwrap_or(0)
        + 1;

    let (touch_tx, touch_rx) = mpsc::channel();
    let (lost_tx, lost_rx) = mpsc::channel();
    android_hid::spawn_reader(
        hid.clone(),
        PtpParser::new(ptp),
        max_report_len,
        touch_tx,
        lost_tx,
    );

    // Heatmap: only PixArt pads answer the chip identification; on anything
    // else the thread logs and exits and the panel simply never appears.
    let heatmap_rx = match heatmap::discovery::burst_report_length(&layout) {
        Ok(burst_len) => {
            log::info!("heatmap: burst report length = {}", burst_len);
            Some(heatmap::backend::spawn_heatmap_thread_with(
                hid.clone(),
                burst_len,
                None,
            ))
        }
        Err(e) => {
            log::info!("heatmap: not available: {}", e);
            None
        }
    };

    // PTP config, when the descriptor declares any of its fields.
    let fields = PtpFields::from_layout(&layout);
    let config = match LayoutConfigBackend::new(hid.clone(), fields.clone()) {
        Some(mut backend) => {
            let mut description = ConfigDescription {
                features: fields.features(),
                button_press_threshold_range: fields.range(KEY_BUTTON_PRESS_THRESHOLD),
                haptic_intensity_range: fields.range(KEY_HAPTIC_INTENSITY),
                physical_size: touchpad_physical_size(&layout),
            };
            let state = pollster::block_on(config::initialize(&mut backend, &mut description));
            log::info!("config: PTP configuration available");
            Some(ConfigHandle::spawn_thread(description, state, backend))
        }
        None => None,
    };

    Ok(Session {
        touch_rx,
        grab_tx: None,
        heatmap_rx,
        config,
        extents,
        recorder: None,
        lost: Some(lost_rx),
    })
}

/// The no-session screen's controls: the USB device list and its actions.
#[derive(Default)]
struct UsbPicker {
    devices: Vec<UsbDeviceInfo>,
    /// A list refresh in progress.
    listing: Option<mpsc::Receiver<Result<Vec<UsbDeviceInfo>, String>>>,
    /// A device being opened (label, result channel).
    opening: Option<(String, mpsc::Receiver<Result<Session, String>>)>,
    /// Bridge generation the current list corresponds to; `None` = never listed.
    listed_generation: Option<i32>,
    /// Device the app was launched for, to open as soon as it is listed.
    auto_open: Option<String>,
    /// Last failure, shown with a retry button.
    error: Option<String>,
    /// Set once a failed open should not be retried without a tap.
    auto_open_done: bool,
}

impl UsbPicker {
    fn refresh(&mut self) {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(android_hid::list_devices().map_err(|e| e.to_string()));
        });
        self.listing = Some(rx);
    }

    fn open(&mut self, dev: &UsbDeviceInfo) {
        let (tx, rx) = mpsc::channel();
        let label = dev.label();
        let dev = dev.clone();
        thread::spawn(move || {
            let _ = tx.send(open_session(&dev));
        });
        self.opening = Some((label, rx));
        self.error = None;
    }

    fn poll(&mut self) -> Option<SessionRequest> {
        // Re-list whenever the bridge saw a USB or permission change.
        let generation = android_hid::generation();
        if self.listing.is_none() && self.listed_generation != Some(generation) {
            self.listed_generation = Some(generation);
            self.refresh();
        }
        if let Some(path) = android_hid::take_attached_path() {
            self.auto_open = Some(path);
            self.auto_open_done = false;
        }

        if let Some(rx) = &self.listing {
            match rx.try_recv() {
                Ok(Ok(devices)) => {
                    self.devices = devices;
                    self.listing = None;
                }
                Ok(Err(e)) => {
                    self.error = Some(e);
                    self.listing = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.listing = None,
            }
        }

        if let Some((_, rx)) = &self.opening {
            match rx.try_recv() {
                Ok(Ok(session)) => {
                    self.opening = None;
                    return Some(SessionRequest::Attach(Box::new(session)));
                }
                Ok(Err(e)) => {
                    self.error = Some(e);
                    self.opening = None;
                    self.auto_open_done = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.opening = None,
            }
        }

        // Auto-open: the device we were launched for, else the only touchpad.
        if self.opening.is_none() && self.listing.is_none() && !self.auto_open_done {
            let touchpads: Vec<&UsbDeviceInfo> = self
                .devices
                .iter()
                .filter(|d| d.touchpad.is_some())
                .collect();
            let target = match &self.auto_open {
                Some(path) => touchpads.iter().find(|d| d.path == *path).copied(),
                None if touchpads.len() == 1 => Some(touchpads[0]),
                None => None,
            };
            if let Some(dev) = target.cloned() {
                self.auto_open = None;
                self.open(&dev);
            }
        }
        None
    }

    /// Draw the controls; returns a request when a session is ready.
    fn draw(&mut self, ui: &mut egui::Ui) -> Option<SessionRequest> {
        let ready = self.poll();

        if let Some((label, _)) = &self.opening {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(format!("Opening {}…", label));
            });
        }

        if self.devices.is_empty() && self.listing.is_none() {
            ui.label(egui::RichText::new("No USB devices found").color(egui::Color32::DARK_GRAY));
        }

        let mut to_open: Option<UsbDeviceInfo> = None;
        let mut to_request: Option<String> = None;
        for dev in &self.devices {
            ui.horizontal(|ui| {
                ui.label(dev.label());
                if let Some((iface, _)) = &dev.touchpad {
                    ui.label(
                        egui::RichText::new(format!("touchpad, interface {}", iface))
                            .color(egui::Color32::DARK_GRAY),
                    );
                    if self.opening.is_none() && ui.button("Open").clicked() {
                        to_open = Some(dev.clone());
                    }
                } else if !dev.granted && dev.has_hid {
                    if ui.button("Request access").clicked() {
                        to_request = Some(dev.path.clone());
                    }
                } else if dev.has_hid {
                    ui.label(
                        egui::RichText::new("no Touch Pad collection")
                            .color(egui::Color32::DARK_GRAY),
                    );
                } else {
                    ui.label(
                        egui::RichText::new("not a HID device").color(egui::Color32::DARK_GRAY),
                    );
                }
            });
        }
        if let Some(dev) = to_open {
            self.auto_open_done = true;
            self.open(&dev);
        }
        if let Some(path) = to_request {
            if let Err(e) = android_hid::request_permission(&path) {
                self.error = Some(e.to_string());
            }
        }

        if let Some(err) = self.error.clone() {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(err).color(egui::Color32::RED));
            if ui.button("Retry").clicked() {
                self.error = None;
                self.auto_open_done = false;
                self.listed_generation = None;
            }
        }

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if ui.button("Rescan").clicked() {
                self.listed_generation = None;
            }
            if ui.button("Play demo recording").clicked() {
                match Recording::from_bytes(DEMO_RECORDING) {
                    Ok(rec) => return Some(SessionRequest::Playback(Box::new(rec))),
                    Err(e) => self.error = Some(format!("demo recording: {}", e)),
                }
            }
            None
        })
        .inner
        .or(ready)
    }
}
