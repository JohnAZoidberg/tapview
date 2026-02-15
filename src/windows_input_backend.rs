//! Windows backend for mouse/scroll events, used for the libinput side panel.
//!
//! Uses a low-level mouse hook (WH_MOUSE_LL) to capture pointer movement,
//! button clicks, and scroll events. This is the standard mechanism used by
//! games and input utilities on Windows.
//!
//! Pinch-to-zoom is detected as Ctrl+scroll (the standard Windows convention).
//! Swipe and hold gestures are not available as the OS shell consumes them.

use crate::libinput_state::LibinputEvent;
use std::sync::mpsc;
use windows::Win32::Foundation::*;
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
use windows::Win32::UI::WindowsAndMessaging::*;

/// Spawn a thread that captures mouse input via a low-level hook and sends
/// structured events over the returned channel.
pub fn spawn_windows_input_thread() -> mpsc::Receiver<LibinputEvent> {
    let (tx, rx) = mpsc::channel();

    std::thread::spawn(move || {
        if let Err(e) = run_mouse_hook_loop(tx) {
            eprintln!("Windows input backend error: {}", e);
        }
    });

    rx
}

thread_local! {
    static MOUSE_TX: std::cell::Cell<Option<mpsc::Sender<LibinputEvent>>> = const { std::cell::Cell::new(None) };
    static LAST_PT: std::cell::Cell<Option<POINT>> = const { std::cell::Cell::new(None) };
    /// Tracks cumulative pinch scale during a Ctrl+scroll (pinch-to-zoom) gesture.
    /// None = no pinch active, Some(scale) = pinch in progress.
    static PINCH_SCALE: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

/// Virtual key code for Ctrl
const VK_CONTROL: i32 = 0x11;

fn run_mouse_hook_loop(tx: mpsc::Sender<LibinputEvent>) -> Result<(), Box<dyn std::error::Error>> {
    unsafe {
        MOUSE_TX.set(Some(tx));

        let hook = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_ll_proc), None, 0)
            .map_err(|e| format!("SetWindowsHookExW: {}", e))?;

        eprintln!("Windows mouse input backend started (low-level hook)");

        // A message pump is required for WH_MOUSE_LL to work.
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        let _ = UnhookWindowsHookEx(hook);
    }

    Ok(())
}

fn end_pinch_if_active(sender: &mpsc::Sender<LibinputEvent>) {
    PINCH_SCALE.with(|cell| {
        if cell.get().is_some() {
            cell.set(None);
            let _ = sender.send(LibinputEvent::GesturePinchEnd);
        }
    });
}

unsafe extern "system" fn mouse_ll_proc(ncode: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if ncode >= 0 {
        let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        let msg = wparam.0 as u32;

        MOUSE_TX.with(|cell| {
            let tx = cell.take();
            if let Some(ref sender) = tx {
                match msg {
                    WM_MOUSEMOVE => {
                        // End pinch if Ctrl was released
                        if PINCH_SCALE.with(|c| c.get().is_some()) && GetKeyState(VK_CONTROL) >= 0 {
                            end_pinch_if_active(sender);
                        }
                        // Compute delta from last known position
                        LAST_PT.with(|last| {
                            let prev = last.get();
                            last.set(Some(info.pt));
                            if let Some(prev) = prev {
                                let dx = (info.pt.x - prev.x) as f64;
                                let dy = (info.pt.y - prev.y) as f64;
                                if dx != 0.0 || dy != 0.0 {
                                    let _ = sender.send(LibinputEvent::PointerMotion {
                                        dx,
                                        dy,
                                        dx_unaccel: dx,
                                        dy_unaccel: dy,
                                    });
                                }
                            }
                        });
                    }
                    WM_LBUTTONDOWN => {
                        let _ = sender.send(LibinputEvent::PointerButton {
                            button: 0x110,
                            pressed: true,
                        });
                    }
                    WM_LBUTTONUP => {
                        let _ = sender.send(LibinputEvent::PointerButton {
                            button: 0x110,
                            pressed: false,
                        });
                    }
                    WM_RBUTTONDOWN => {
                        let _ = sender.send(LibinputEvent::PointerButton {
                            button: 0x111,
                            pressed: true,
                        });
                    }
                    WM_RBUTTONUP => {
                        let _ = sender.send(LibinputEvent::PointerButton {
                            button: 0x111,
                            pressed: false,
                        });
                    }
                    WM_MBUTTONDOWN => {
                        let _ = sender.send(LibinputEvent::PointerButton {
                            button: 0x112,
                            pressed: true,
                        });
                    }
                    WM_MBUTTONUP => {
                        let _ = sender.send(LibinputEvent::PointerButton {
                            button: 0x112,
                            pressed: false,
                        });
                    }
                    WM_MOUSEWHEEL => {
                        let delta = (info.mouseData >> 16) as i16;
                        let ctrl_down = GetKeyState(VK_CONTROL) < 0;

                        if ctrl_down {
                            // Ctrl+Scroll = pinch-to-zoom gesture
                            let scale_delta = delta as f64 / 120.0 * 0.1;
                            PINCH_SCALE.with(|cell| {
                                let prev = cell.get();
                                if prev.is_none() {
                                    let _ = sender
                                        .send(LibinputEvent::GesturePinchBegin { fingers: 2 });
                                }
                                let new_scale = prev.unwrap_or(1.0) + scale_delta;
                                cell.set(Some(new_scale));
                                let _ = sender.send(LibinputEvent::GesturePinchUpdate {
                                    fingers: 2,
                                    dx: 0.0,
                                    dy: 0.0,
                                    dx_unaccel: 0.0,
                                    dy_unaccel: 0.0,
                                    scale: new_scale,
                                    angle: 0.0,
                                });
                            });
                        } else {
                            end_pinch_if_active(sender);
                            let _ = sender.send(LibinputEvent::Scroll {
                                source: crate::libinput_state::ScrollSource::Wheel,
                                vert: -(delta as f64) / 120.0 * 15.0,
                                horiz: 0.0,
                            });
                        }
                    }
                    WM_MOUSEHWHEEL => {
                        let delta = (info.mouseData >> 16) as i16;
                        end_pinch_if_active(sender);
                        let _ = sender.send(LibinputEvent::Scroll {
                            source: crate::libinput_state::ScrollSource::Wheel,
                            vert: 0.0,
                            horiz: (delta as f64) / 120.0 * 15.0,
                        });
                    }
                    _ => {}
                }
            }
            cell.set(tx);
        });
    }

    CallNextHookEx(None, ncode, wparam, lparam)
}
