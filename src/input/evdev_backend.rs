use super::{InputBackend, InputError, TouchState};
use crate::multitouch::{self, MTStateMachine};
use evdev::{AbsoluteAxisType, Device, EventType};
use std::collections::VecDeque;
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::UNIX_EPOCH;

/// `EVIOCSCLOCKID`: `_IOW('E', 0xa0, int)`.
const EVIOCSCLOCKID: libc::c_ulong = (1 << 30) | (4 << 16) | (b'E' as libc::c_ulong) << 8 | 0xa0;
/// `SYN_REPORT`: end of one frame.
const SYN_REPORT: u16 = 0;

/// Read ABS_MT_POSITION_X/Y axis extents from evdev absinfo.
/// Returns (x_max, y_max).  The kernel applies any axis swaps before
/// exposing the evdev device, so these always match the event coordinates.
pub fn read_axis_extents(device_path: &Path) -> Option<(i32, i32)> {
    let device = Device::open(device_path).ok()?;
    let abs = device.get_abs_state().ok()?;
    let x = abs[AbsoluteAxisType::ABS_MT_POSITION_X.0 as usize];
    let y = abs[AbsoluteAxisType::ABS_MT_POSITION_Y.0 as usize];
    if x.maximum > 0 && y.maximum > 0 {
        Some((x.maximum, y.maximum))
    } else {
        None
    }
}

pub struct EvdevBackend {
    device: Device,
    machine: MTStateMachine,
    /// Frames from the last read not yet handed out: one read can return
    /// several complete frames, and each one matters for the report-rate
    /// estimate.
    pending: VecDeque<TouchState>,
}

impl InputBackend for EvdevBackend {
    fn open(device_path: &Path) -> Result<Self, InputError> {
        let device = Device::open(device_path)
            .map_err(|e| InputError::OpenFailed(format!("{}: {}", device_path.display(), e)))?;

        // Kernel event stamps default to CLOCK_REALTIME; ask for the
        // monotonic clock so frame intervals survive a wall-clock step.
        // Best effort: only differences between stamps are ever used.
        let clock: libc::c_int = libc::CLOCK_MONOTONIC;
        // SAFETY: a valid ioctl on an open fd, with a pointer to an int the
        // kernel only reads.
        let r = unsafe { libc::ioctl(device.as_raw_fd(), EVIOCSCLOCKID as _, &clock) };
        if r != 0 {
            log::debug!(
                "EVIOCSCLOCKID failed on {}: {}",
                device_path.display(),
                std::io::Error::last_os_error()
            );
        }

        Ok(Self {
            device,
            machine: MTStateMachine::new(),
            pending: VecDeque::new(),
        })
    }

    fn grab(&mut self) -> Result<(), InputError> {
        self.device
            .grab()
            .map_err(|e| InputError::GrabFailed(e.to_string()))
    }

    fn ungrab(&mut self) -> Result<(), InputError> {
        self.device
            .ungrab()
            .map_err(|e| InputError::GrabFailed(e.to_string()))
    }

    /// One frame per call: the state as of each SYN_REPORT, stamped with the
    /// kernel's time for that event.
    fn poll_events(&mut self) -> Result<Option<TouchState>, InputError> {
        if let Some(state) = self.pending.pop_front() {
            return Ok(Some(state));
        }
        match self.device.fetch_events() {
            Ok(events) => {
                for event in events {
                    // Raw event dump; a no-op unless debug logging is on (--verbose).
                    multitouch::print_event(&event);
                    self.machine.process(&event);
                    if event.event_type() == EventType::SYNCHRONIZATION
                        && event.code() == SYN_REPORT
                    {
                        // With the monotonic clock set above this is not a
                        // Unix time, but the crate wraps every stamp as one.
                        let timestamp_us = event
                            .timestamp()
                            .duration_since(UNIX_EPOCH)
                            .ok()
                            .map(|d| d.as_micros() as u64);
                        self.pending.push_back(TouchState {
                            touches: self.machine.touches,
                            buttons: self.machine.buttons,
                            timestamp_us,
                            scan_time_us: self.machine.scan_time_us,
                        });
                    }
                }
                Ok(self.pending.pop_front())
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(InputError::ReadError(e.to_string())),
        }
    }
}
