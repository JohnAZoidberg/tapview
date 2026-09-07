//! The browser front end: the same [`TapviewApp`] as the desktop, started
//! without a device, with a WebHID connect flow on the no-session screen.
//!
//! Entered from `main` on wasm32 (trunk builds that bin and `index.html`
//! loads it). Device I/O is [`crate::web_hid`]; this module drives the
//! picker: a silent re-grant pass at startup, **Connect touchpad** for the
//! browser prompt, auto-open when exactly one granted pad is present, a
//! re-list on every hot-plug event, a **Disconnect** bar over a live session
//! (to switch pads), and a demo recording for browsers that have no WebHID
//! at all. The right-hand panel — libinput's slot natively —
//! shows what the browser makes of the pad: pointer motion, scrolling and
//! ctrl+wheel pinch, taken from egui's own input events.

use crate::app::{NoSessionUi, SessionRequest, TapviewApp};
use crate::libinput_state::{LibinputEvent, ScrollSource};
use crate::recording::Recording;
use crate::session::Session;
use crate::web_hid::{self, Granted};
use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc;
use wasm_bindgen::JsCast as _;

/// The bundled demo recording (a Framework 13 pad), for a browser without
/// WebHID or a machine without a touchpad.
const DEMO_RECORDING: &[u8] = include_bytes!("../testdata/sample.tapv");

/// Start the app in the page's canvas. Returns immediately: eframe's web
/// runner is parked on the event loop.
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);

    wasm_bindgen_futures::spawn_local(async {
        let canvas = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.get_element_by_id("tapview_canvas"))
            .and_then(|element| element.dyn_into::<web_sys::HtmlCanvasElement>().ok())
            .expect("index.html carries <canvas id=\"tapview_canvas\">");
        eframe::WebRunner::new()
            .start(
                canvas,
                eframe::WebOptions::default(),
                Box::new(|_cc| Ok(Box::new(build_app()))),
            )
            .await
            .expect("eframe failed to start");
    });
}

const NO_SESSION_MESSAGE: &str = "No touchpad connected";

/// State the session bar and the picker share: whether the user just closed
/// the pad on purpose (so the picker must not open the only granted pad
/// straight back).
#[derive(Default)]
struct Shared {
    manual_disconnect: Cell<bool>,
}

fn build_app() -> TapviewApp {
    let supported = web_hid::watch_hotplug();
    let shared = Rc::new(Shared::default());
    let mut picker = WebPicker::new(supported, shared.clone());
    let (mut pointer, pointer_rx) = BrowserPointer::new();
    let message = if supported {
        NO_SESSION_MESSAGE.to_string()
    } else {
        "This browser has no WebHID".to_string()
    };
    TapviewApp::new(20)
        .with_no_session_ui(NoSessionUi {
            message,
            controls: Some(Box::new(move |ui| picker.draw(ui))),
        })
        .with_libinput(Some(pointer_rx))
        .with_frame_hook(Box::new(move |ctx, app| {
            pointer.pump(ctx);
            session_bar(ctx, app, &shared);
        }))
}

/// A slim bar above a live session with the way out: no unplugging a
/// built-in touchpad to switch to another one.
fn session_bar(ctx: &egui::Context, app: &mut TapviewApp, shared: &Shared) {
    if !app.has_session() {
        return;
    }
    egui::TopBottomPanel::top("web-session-bar").show(ctx, |ui| {
        if ui.button("Disconnect").clicked() {
            app.detach_session();
            app.set_no_session_message(NO_SESSION_MESSAGE);
            shared.manual_disconnect.set(true);
        }
    });
}

// ── Device picker ───────────────────────────────────────────────────────────

enum Reply {
    Listed(Result<Vec<Granted>, String>),
    Opened(Result<Box<Session>, String>),
}

/// The no-session screen's controls: granted touchpads and their actions.
struct WebPicker {
    supported: bool,
    granted: Vec<Granted>,
    /// What an in-flight operation is doing, for the spinner.
    busy: Option<&'static str>,
    reply_tx: mpsc::Sender<Reply>,
    reply_rx: mpsc::Receiver<Reply>,
    /// Hot-plug generation the current list corresponds to; `None` = never listed.
    listed_generation: Option<u32>,
    error: Option<String>,
    /// Auto-open only happens once per list, so a failure does not loop.
    auto_open_done: bool,
    shared: Rc<Shared>,
}

impl WebPicker {
    fn new(supported: bool, shared: Rc<Shared>) -> Self {
        let (reply_tx, reply_rx) = mpsc::channel();
        Self {
            supported,
            granted: Vec::new(),
            busy: None,
            reply_tx,
            reply_rx,
            listed_generation: None,
            error: None,
            auto_open_done: false,
            shared,
        }
    }

    fn list(&mut self, request_new: bool) {
        let tx = self.reply_tx.clone();
        self.busy = Some(if request_new {
            "Waiting for the browser prompt…"
        } else {
            "Looking for granted touchpads…"
        });
        self.error = None;
        self.auto_open_done = false;
        wasm_bindgen_futures::spawn_local(async move {
            let _ = tx.send(Reply::Listed(web_hid::connect(request_new).await));
        });
    }

    fn open(&mut self, granted: &Granted) {
        let tx = self.reply_tx.clone();
        let granted = granted.clone();
        self.busy = Some("Opening the touchpad…");
        self.error = None;
        wasm_bindgen_futures::spawn_local(async move {
            let _ = tx.send(Reply::Opened(
                web_hid::open_session(&granted).await.map(Box::new),
            ));
        });
    }

    fn poll(&mut self) -> Option<SessionRequest> {
        // Re-list at startup and after every plug/unplug.
        let generation = web_hid::hotplug_generation();
        if self.supported && self.busy.is_none() && self.listed_generation != Some(generation) {
            self.listed_generation = Some(generation);
            self.list(false);
        }

        while let Ok(reply) = self.reply_rx.try_recv() {
            self.busy = None;
            match reply {
                Reply::Listed(Ok(granted)) => self.granted = granted,
                Reply::Listed(Err(e)) => self.error = Some(e),
                Reply::Opened(Ok(session)) => return Some(SessionRequest::Attach(session)),
                Reply::Opened(Err(e)) => {
                    self.error = Some(e);
                    self.auto_open_done = true;
                }
            }
        }

        // A pad the user just disconnected stays disconnected until a click.
        if self.shared.manual_disconnect.replace(false) {
            self.auto_open_done = true;
        }

        // Exactly one granted pad: open it without a click.
        if self.busy.is_none() && !self.auto_open_done && self.granted.len() == 1 {
            self.auto_open_done = true;
            let only = self.granted[0].clone();
            self.open(&only);
        }
        None
    }

    /// Draw the controls; returns a request when a session is ready.
    fn draw(&mut self, ui: &mut egui::Ui) -> Option<SessionRequest> {
        let ready = self.poll();

        if !self.supported {
            ui.label(
                egui::RichText::new(
                    "WebHID is what reaches the touchpad from a page, and only Chromium-based \
                     browsers on the desktop have it (Chrome, Edge, Opera).",
                )
                .color(egui::Color32::DARK_GRAY),
            );
        }

        if let Some(what) = self.busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(what);
            });
        }

        let mut to_open: Option<Granted> = None;
        for g in &self.granted {
            ui.horizontal(|ui| {
                ui.label(g.label());
                if self.busy.is_none() && ui.button("Open").clicked() {
                    to_open = Some(g.clone());
                }
            });
        }
        if let Some(g) = to_open {
            self.auto_open_done = true;
            self.open(&g);
        }

        if let Some(err) = self.error.clone() {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(err).color(egui::Color32::RED));
        }

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            if self.supported && self.busy.is_none() && ui.button("Connect touchpad").clicked() {
                self.list(true);
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

// ── Pointer panel ───────────────────────────────────────────────────────────

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// Turns egui's raw pointer events into the [`LibinputEvent`]s the right
/// panel visualizes. The browser is the only interpretation of the pad a
/// page can see: pointer motion, smooth (finger) or line (wheel) scrolling,
/// and pinch, which browsers deliver as ctrl+wheel. Events arrive only while
/// the pointer is over the canvas.
struct BrowserPointer {
    tx: mpsc::Sender<LibinputEvent>,
    last_pos: Option<egui::Pos2>,
    /// Cumulative pinch scale since the gesture began, `None` when idle.
    pinch: Option<f64>,
    /// Frames since the last pinch update, to end the gesture.
    pinch_idle: u32,
}

impl BrowserPointer {
    fn new() -> (Self, mpsc::Receiver<LibinputEvent>) {
        let (tx, rx) = mpsc::channel();
        (
            Self {
                tx,
                last_pos: None,
                pinch: None,
                pinch_idle: 0,
            },
            rx,
        )
    }

    fn pump(&mut self, ctx: &egui::Context) {
        let events = ctx.input(|i| i.raw.events.clone());
        let mut pinched = false;
        for event in events {
            match event {
                egui::Event::PointerMoved(pos) => {
                    if let Some(last) = self.last_pos {
                        let d = pos - last;
                        if d != egui::Vec2::ZERO {
                            let _ = self.tx.send(LibinputEvent::PointerMotion {
                                dx: d.x as f64,
                                dy: d.y as f64,
                                dx_unaccel: d.x as f64,
                                dy_unaccel: d.y as f64,
                            });
                        }
                    }
                    self.last_pos = Some(pos);
                }
                egui::Event::PointerGone => self.last_pos = None,
                egui::Event::PointerButton {
                    button, pressed, ..
                } => {
                    let button = match button {
                        egui::PointerButton::Primary => BTN_LEFT,
                        egui::PointerButton::Secondary => BTN_RIGHT,
                        egui::PointerButton::Middle => BTN_MIDDLE,
                        _ => continue,
                    };
                    let _ = self
                        .tx
                        .send(LibinputEvent::PointerButton { button, pressed });
                }
                egui::Event::MouseWheel {
                    unit,
                    delta,
                    modifiers,
                } if modifiers.ctrl => {
                    // Browsers report trackpad pinch as ctrl+wheel; the
                    // factor mirrors egui's own zoom_delta mapping.
                    let step = match unit {
                        egui::MouseWheelUnit::Point => delta.y as f64 / 200.0,
                        egui::MouseWheelUnit::Line => delta.y as f64 / 10.0,
                        egui::MouseWheelUnit::Page => delta.y as f64,
                    };
                    let scale = self.pinch.unwrap_or(1.0) * step.exp();
                    if self.pinch.is_none() {
                        let _ = self
                            .tx
                            .send(LibinputEvent::GesturePinchBegin { fingers: 2 });
                    }
                    self.pinch = Some(scale);
                    pinched = true;
                    let _ = self.tx.send(LibinputEvent::GesturePinchUpdate {
                        fingers: 2,
                        dx: 0.0,
                        dy: 0.0,
                        dx_unaccel: 0.0,
                        dy_unaccel: 0.0,
                        scale,
                        angle: 0.0,
                    });
                }
                egui::Event::MouseWheel { unit, delta, .. } => {
                    // libinput's sign convention: positive = fingers move
                    // down/right; egui's delta is the content's movement.
                    let source = match unit {
                        egui::MouseWheelUnit::Point => ScrollSource::Finger,
                        egui::MouseWheelUnit::Line | egui::MouseWheelUnit::Page => {
                            ScrollSource::Wheel
                        }
                    };
                    let _ = self.tx.send(LibinputEvent::Scroll {
                        source,
                        vert: -delta.y as f64,
                        horiz: -delta.x as f64,
                    });
                }
                _ => {}
            }
        }

        if self.pinch.is_some() {
            if pinched {
                self.pinch_idle = 0;
            } else {
                self.pinch_idle += 1;
                if self.pinch_idle > 12 {
                    self.pinch = None;
                    let _ = self.tx.send(LibinputEvent::GesturePinchEnd);
                }
            }
        }
    }
}
