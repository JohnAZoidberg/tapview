//! Linux: touches read from the touchpad's `/dev/hidraw*` node and decoded
//! with the PTP report parser, instead of from evdev.
//!
//! hidraw is a tee — the kernel's hid-multitouch keeps driving the pointer —
//! so this backend exists to develop and check [`crate::ptp`] against the
//! kernel's interpretation on a laptop, with no phone or browser involved.
//! It is also what the raw-HID ports (Android, WebHID) effectively do.
//! Grabbing is impossible over hidraw.

use super::{InputBackend, InputError, TouchState};
use crate::hid::{ReportKind, ReportLayout};
use crate::ptp::{PtpLayout, PtpParser};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub struct HidrawBackend {
    file: File,
    parser: PtpParser,
    /// Sized for the largest Input report plus its ID byte; one `read`
    /// returns exactly one report.
    buf: Vec<u8>,
}

impl HidrawBackend {
    /// Parse the device's report descriptor (from sysfs) and open the node
    /// non-blocking. Fails if the descriptor has no Touch Pad collection.
    pub fn open(hidraw_path: &Path) -> Result<Self, InputError> {
        let desc = crate::hid::linux::read_report_descriptor(hidraw_path).map_err(|e| {
            InputError::OpenFailed(format!(
                "{}: report descriptor: {}",
                hidraw_path.display(),
                e
            ))
        })?;
        let layout = ReportLayout::parse(&desc);
        let ptp = PtpLayout::from_layout(&layout).ok_or_else(|| {
            InputError::OpenFailed(format!(
                "{}: no Touch Pad collection in report descriptor",
                hidraw_path.display()
            ))
        })?;
        let max_report = layout
            .report_ids(ReportKind::Input)
            .iter()
            .map(|&id| layout.report_bytes(ReportKind::Input, id))
            .max()
            .unwrap_or(0);

        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(hidraw_path)
            .map_err(|e| InputError::OpenFailed(format!("{}: {}", hidraw_path.display(), e)))?;

        Ok(Self {
            file,
            parser: PtpParser::new(ptp),
            buf: vec![0u8; (max_report + 1).max(64)],
        })
    }

    pub fn layout(&self) -> &PtpLayout {
        self.parser.layout()
    }

    /// Axis extents (x_max, y_max) from the report descriptor. These are the
    /// device's own coordinates, before any axis swap the kernel might apply
    /// to its evdev node.
    pub fn extents(&self) -> (i32, i32) {
        (self.layout().x_max, self.layout().y_max)
    }

    /// Wait up to `timeout_ms` for one raw report and read it into the
    /// internal buffer; returns its length, or `None` on timeout.
    pub fn read_raw(&mut self, timeout_ms: i32) -> Result<Option<usize>, InputError> {
        let mut pfd = libc::pollfd {
            fd: self.file.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if ready < 0 {
            let err = std::io::Error::last_os_error();
            return if err.kind() == std::io::ErrorKind::Interrupted {
                Ok(None)
            } else {
                Err(InputError::ReadError(err.to_string()))
            };
        }
        if ready == 0 {
            return Ok(None);
        }
        match self.file.read(&mut self.buf) {
            Ok(0) => Err(InputError::ReadError("device closed".to_string())),
            Ok(n) => Ok(Some(n)),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => Ok(None),
            Err(e) => Err(InputError::ReadError(e.to_string())),
        }
    }

    /// The last report read by [`read_raw`](Self::read_raw).
    pub fn buffer(&self) -> &[u8] {
        &self.buf
    }
}

impl InputBackend for HidrawBackend {
    fn open(device_path: &Path) -> Result<Self, InputError> {
        Self::open(device_path)
    }

    fn grab(&mut self) -> Result<(), InputError> {
        Err(InputError::GrabFailed(
            "not possible over hidraw".to_string(),
        ))
    }

    fn ungrab(&mut self) -> Result<(), InputError> {
        Ok(())
    }

    /// One report per call, at most; `Ok(None)` when nothing arrived within
    /// 5 ms so the input thread stays responsive to commands.
    fn poll_events(&mut self) -> Result<Option<TouchState>, InputError> {
        match self.read_raw(5)? {
            Some(n) => Ok(self.parser.feed(&self.buf[..n])),
            None => Ok(None),
        }
    }
}
