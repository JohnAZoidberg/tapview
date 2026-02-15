//! Libinput library backend for reading pointer, scroll, and gesture events.

use crate::libinput_state::{LibinputEvent, ScrollSource};
use std::os::unix::io::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use input::event::gesture::{GestureEvent, GestureEventCoordinates, GesturePinchEventTrait};
use input::event::pointer::{Axis, ButtonState, PointerEvent, PointerScrollEvent};
use input::{Event, Libinput, LibinputInterface};

struct Interface;

impl LibinputInterface for Interface {
    fn open_restricted(&mut self, path: &Path, flags: i32) -> Result<OwnedFd, i32> {
        let path_cstr = match std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) {
            Ok(c) => c,
            Err(_) => return Err(libc::EINVAL),
        };
        let fd = unsafe { libc::open(path_cstr.as_ptr(), flags) };
        if fd < 0 {
            Err(unsafe { *libc::__errno_location() })
        } else {
            Ok(unsafe { OwnedFd::from_raw_fd(fd) })
        }
    }

    fn close_restricted(&mut self, fd: OwnedFd) {
        drop(fd);
    }
}

/// Spawn a thread that reads libinput events from the given device and sends
/// structured events over the returned channel.
pub fn spawn_libinput_thread(device_path: &Path) -> mpsc::Receiver<LibinputEvent> {
    let (tx, rx) = mpsc::channel();
    let path = device_path.to_path_buf();

    thread::spawn(move || {
        if let Err(e) = run_libinput_loop(&path, &tx) {
            eprintln!("libinput backend error: {}", e);
        }
    });

    rx
}

fn run_libinput_loop(
    device_path: &Path,
    tx: &mpsc::Sender<LibinputEvent>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut ctx = Libinput::new_from_path(Interface);
    let path_str = device_path
        .to_str()
        .ok_or("Device path is not valid UTF-8")?;
    let mut device = ctx
        .path_add_device(path_str)
        .ok_or("Failed to add device to libinput context")?;

    // Enable tap-to-click (disabled by default in new_from_path contexts)
    if device.config_tap_finger_count() > 0 {
        let _ = device.config_tap_set_enabled(true);
    }

    let poll_fd = ctx.as_raw_fd();
    let mut pollfd = libc::pollfd {
        fd: poll_fd,
        events: libc::POLLIN,
        revents: 0,
    };

    loop {
        let ret = unsafe { libc::poll(&mut pollfd, 1, 100) };
        if ret < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err.into());
        }

        ctx.dispatch()?;

        for event in &mut ctx {
            let li_event = match event {
                Event::Pointer(PointerEvent::Motion(m)) => {
                    use input::event::pointer::PointerMotionEvent;
                    Some(LibinputEvent::PointerMotion {
                        dx: PointerMotionEvent::dx(&m),
                        dy: PointerMotionEvent::dy(&m),
                        dx_unaccel: m.dx_unaccelerated(),
                        dy_unaccel: m.dy_unaccelerated(),
                    })
                }
                Event::Pointer(PointerEvent::Button(b)) => Some(LibinputEvent::PointerButton {
                    button: b.button(),
                    pressed: b.button_state() == ButtonState::Pressed,
                }),
                Event::Pointer(PointerEvent::ScrollWheel(s)) => Some(LibinputEvent::Scroll {
                    source: ScrollSource::Wheel,
                    vert: if s.has_axis(Axis::Vertical) {
                        s.scroll_value(Axis::Vertical)
                    } else {
                        0.0
                    },
                    horiz: if s.has_axis(Axis::Horizontal) {
                        s.scroll_value(Axis::Horizontal)
                    } else {
                        0.0
                    },
                }),
                Event::Pointer(PointerEvent::ScrollFinger(s)) => Some(LibinputEvent::Scroll {
                    source: ScrollSource::Finger,
                    vert: if s.has_axis(Axis::Vertical) {
                        s.scroll_value(Axis::Vertical)
                    } else {
                        0.0
                    },
                    horiz: if s.has_axis(Axis::Horizontal) {
                        s.scroll_value(Axis::Horizontal)
                    } else {
                        0.0
                    },
                }),
                Event::Pointer(PointerEvent::ScrollContinuous(s)) => Some(LibinputEvent::Scroll {
                    source: ScrollSource::Continuous,
                    vert: if s.has_axis(Axis::Vertical) {
                        s.scroll_value(Axis::Vertical)
                    } else {
                        0.0
                    },
                    horiz: if s.has_axis(Axis::Horizontal) {
                        s.scroll_value(Axis::Horizontal)
                    } else {
                        0.0
                    },
                }),
                Event::Gesture(GestureEvent::Swipe(swipe)) => {
                    use input::event::gesture::GestureSwipeEvent;
                    match swipe {
                        GestureSwipeEvent::Begin(b) => {
                            use input::event::gesture::GestureEventTrait;
                            Some(LibinputEvent::GestureSwipeBegin {
                                fingers: GestureEventTrait::finger_count(&b),
                            })
                        }
                        GestureSwipeEvent::Update(u) => {
                            use input::event::gesture::GestureEventTrait;
                            Some(LibinputEvent::GestureSwipeUpdate {
                                fingers: GestureEventTrait::finger_count(&u),
                                dx: u.dx(),
                                dy: u.dy(),
                                dx_unaccel: u.dx_unaccelerated(),
                                dy_unaccel: u.dy_unaccelerated(),
                            })
                        }
                        GestureSwipeEvent::End(_) => Some(LibinputEvent::GestureSwipeEnd),
                        _ => None,
                    }
                }
                Event::Gesture(GestureEvent::Pinch(pinch)) => {
                    use input::event::gesture::GesturePinchEvent;
                    match pinch {
                        GesturePinchEvent::Begin(b) => {
                            use input::event::gesture::GestureEventTrait;
                            Some(LibinputEvent::GesturePinchBegin {
                                fingers: GestureEventTrait::finger_count(&b),
                            })
                        }
                        GesturePinchEvent::Update(u) => {
                            use input::event::gesture::GestureEventTrait;
                            Some(LibinputEvent::GesturePinchUpdate {
                                fingers: GestureEventTrait::finger_count(&u),
                                dx: u.dx(),
                                dy: u.dy(),
                                dx_unaccel: u.dx_unaccelerated(),
                                dy_unaccel: u.dy_unaccelerated(),
                                scale: u.scale(),
                                angle: u.angle_delta(),
                            })
                        }
                        GesturePinchEvent::End(_) => Some(LibinputEvent::GesturePinchEnd),
                        _ => None,
                    }
                }
                Event::Gesture(GestureEvent::Hold(hold)) => {
                    use input::event::gesture::GestureHoldEvent;
                    match hold {
                        GestureHoldEvent::Begin(b) => {
                            use input::event::gesture::GestureEventTrait;
                            Some(LibinputEvent::GestureHoldBegin {
                                fingers: GestureEventTrait::finger_count(&b),
                            })
                        }
                        GestureHoldEvent::End(e) => {
                            use input::event::gesture::GestureEndEvent;
                            Some(LibinputEvent::GestureHoldEnd {
                                cancelled: e.cancelled(),
                            })
                        }
                        _ => None,
                    }
                }
                _ => None,
            };

            if let Some(ev) = li_event {
                if tx.send(ev).is_err() {
                    return Ok(()); // UI closed
                }
            }
        }
    }
}
