//! The window's touchpad picker, for a launch without `--device`: the
//! detected pads on the no-session screen, and a **Switch touchpad** bar
//! over a live session while there is more than one pad to switch to. The
//! browser's picker (`web.rs`) is the model; here the list comes from
//! discovery and every pad can be opened without asking.

use super::{open_for_window, window_size, OpenOptions};
use crate::app::{NoSessionUi, TapviewApp};
use crate::discovery::DeviceInfo;
use std::cell::RefCell;
use std::rc::Rc;

pub(super) struct Picker {
    devices: Vec<DeviceInfo>,
    opts: OpenOptions,
    /// The pad clicked on. Opening it blocks the UI thread on the device, so
    /// it waits for a frame that has drawn the spinner (`pending_drawn`).
    pending: Option<DeviceInfo>,
    pending_drawn: bool,
    error: Option<String>,
}

impl Picker {
    pub(super) fn new(devices: Vec<DeviceInfo>, opts: OpenOptions) -> Self {
        Self {
            devices,
            opts,
            pending: None,
            pending_drawn: false,
            error: None,
        }
    }

    /// Put the picker on the app's no-session screen and the switch bar
    /// over its sessions.
    pub(super) fn install(self, app: TapviewApp) -> TapviewApp {
        let picker = Rc::new(RefCell::new(self));
        let controls = picker.clone();
        app.with_no_session_ui(NoSessionUi {
            message: String::new(),
            controls: Some(Box::new(move |ui| {
                controls.borrow_mut().draw(ui);
                None
            })),
        })
        .with_frame_hook(Box::new(move |ctx, app| {
            picker.borrow_mut().frame(ctx, app)
        }))
    }

    fn refresh(&mut self) {
        match super::find_touchpads() {
            Ok(devices) => {
                self.devices = devices;
                self.error = None;
            }
            Err(e) => {
                self.devices.clear();
                self.error = Some(e.to_string());
            }
        }
    }

    /// Open the pad picked last frame, and draw the switch bar. The frame
    /// hook rather than the controls does this: it has the app, which a
    /// Linux pick also hands a libinput context of its own.
    fn frame(&mut self, ctx: &egui::Context, app: &mut TapviewApp) {
        if self.pending_drawn {
            if let Some(device) = self.pending.take() {
                self.pending_drawn = false;
                match open_for_window(&device, &mut self.opts) {
                    Ok(session) => {
                        #[cfg(target_os = "linux")]
                        app.set_libinput(
                            self.opts.libinput.then(|| super::spawn_libinput(&device)),
                        );
                        ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(window_size(
                            Some(&session),
                            self.opts.libinput,
                        )));
                        app.attach_session(session);
                    }
                    Err(e) => {
                        log::error!("{}", e);
                        self.error = Some(e);
                    }
                }
            }
        }

        if !app.has_session() {
            app.set_no_session_message(if self.devices.is_empty() {
                "No touchpad found"
            } else {
                "Choose a touchpad"
            });
            return;
        }
        if self.devices.len() < 2 {
            return;
        }
        egui::TopBottomPanel::top("native-session-bar").show(ctx, |ui| {
            if ui.button("Switch touchpad").clicked() {
                app.detach_session();
                #[cfg(target_os = "linux")]
                app.set_libinput(None);
                self.refresh();
            }
        });
    }

    fn draw(&mut self, ui: &mut egui::Ui) {
        if self.pending.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Opening the touchpad…");
            });
            self.pending_drawn = true;
            return;
        }

        let mut picked = None;
        for device in &self.devices {
            ui.horizontal(|ui| {
                ui.label(device.label());
                if ui.button("Open").clicked() {
                    picked = Some(device.clone());
                }
            });
        }
        if picked.is_some() {
            self.pending = picked;
            self.error = None;
        }

        if let Some(err) = &self.error {
            ui.add_space(8.0);
            ui.label(egui::RichText::new(err).color(egui::Color32::RED));
        }

        ui.add_space(16.0);
        if ui.button("Refresh").clicked() {
            self.refresh();
        }
    }
}
