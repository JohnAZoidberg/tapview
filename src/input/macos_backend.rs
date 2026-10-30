//! macOS: touches read as raw input reports from the pad's IOHIDDevice and
//! decoded with the PTP report parser.
//!
//! The device is opened shared, so the system keeps driving the cursor and
//! grabbing is not offered.

use super::{InputBackend, InputError, TouchState};
use crate::hid::macos::MacHidDevice;
use crate::hid::{ReportKind, ReportLayout};
use crate::ptp::{PtpLayout, PtpParser};
use std::path::Path;

pub struct MacBackend {
    dev: MacHidDevice,
    parser: PtpParser,
    buf: Vec<u8>,
}

impl MacBackend {
    pub fn layout(&self) -> &PtpLayout {
        self.parser.layout()
    }

    /// Axis extents (x_max, y_max) from the report descriptor.
    pub fn extents(&self) -> (i32, i32) {
        (self.layout().x_max, self.layout().y_max)
    }
}

impl InputBackend for MacBackend {
    fn open(device_path: &Path) -> Result<Self, InputError> {
        let dev =
            MacHidDevice::open(device_path).map_err(|e| InputError::OpenFailed(e.to_string()))?;
        let desc = dev.report_descriptor().map_err(|e| {
            InputError::OpenFailed(format!(
                "{}: report descriptor: {}",
                device_path.display(),
                e
            ))
        })?;
        let layout = ReportLayout::parse(&desc);
        let ptp = PtpLayout::from_layout(&layout).ok_or_else(|| {
            InputError::OpenFailed(format!(
                "{}: no Touch Pad collection in report descriptor",
                device_path.display()
            ))
        })?;
        let max_report = layout
            .report_ids(ReportKind::Input)
            .iter()
            .map(|&id| layout.report_bytes(ReportKind::Input, id))
            .max()
            .unwrap_or(0);
        Ok(Self {
            dev,
            parser: PtpParser::new(ptp),
            buf: vec![0u8; (max_report + 1).max(64)],
        })
    }

    fn grab(&mut self) -> Result<(), InputError> {
        Err(InputError::GrabFailed(
            "not possible on a shared IOHIDDevice".to_string(),
        ))
    }

    fn ungrab(&mut self) -> Result<(), InputError> {
        Ok(())
    }

    fn poll_events(&mut self) -> Result<Option<TouchState>, InputError> {
        match self
            .dev
            .read_report(&mut self.buf, 5)
            .map_err(|e| InputError::ReadError(e.to_string()))?
        {
            Some(n) => Ok(self.parser.feed(&self.buf[..n])),
            None => Ok(None),
        }
    }
}
