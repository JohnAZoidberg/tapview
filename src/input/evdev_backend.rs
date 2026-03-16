use super::{InputBackend, InputError, TouchState};
use crate::multitouch::{self, MTStateMachine};
use evdev::{AbsoluteAxisType, Device};
use std::path::Path;

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
    verbose: bool,
}

impl EvdevBackend {
    pub fn open_with_verbose(device_path: &Path, verbose: bool) -> Result<Self, InputError> {
        let device = Device::open(device_path)
            .map_err(|e| InputError::OpenFailed(format!("{}: {}", device_path.display(), e)))?;

        Ok(Self {
            device,
            machine: MTStateMachine::new(),
            verbose,
        })
    }
}

impl InputBackend for EvdevBackend {
    fn open(device_path: &Path) -> Result<Self, InputError> {
        Self::open_with_verbose(device_path, false)
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

    fn poll_events(&mut self) -> Result<Option<TouchState>, InputError> {
        match self.device.fetch_events() {
            Ok(events) => {
                for event in events {
                    if self.verbose {
                        multitouch::print_event(&event);
                    }
                    self.machine.process(&event);
                }
                Ok(Some(TouchState {
                    touches: self.machine.touches,
                    buttons: self.machine.buttons,
                }))
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(InputError::ReadError(e.to_string())),
        }
    }
}
