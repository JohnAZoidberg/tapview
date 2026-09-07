//! HID report-descriptor model shared by every transport.
//!
//! [`descriptor::ReportLayout`] is built from the raw descriptor bytes
//! (sysfs on Linux, `GET_DESCRIPTOR` on Android) or, later, from a browser's
//! parsed `HIDDevice.collections`, and answers the questions the rest of the
//! code has: where a usage lives in which report, how long a report is, and
//! what its logical/physical ranges are.

pub mod descriptor;
#[cfg(target_os = "linux")]
pub mod linux;

pub use descriptor::{
    decode_unit_exponent, extract_bits, extract_signed, insert_bits, physical_range_mm, Collection,
    ReportField, ReportKind, ReportLayout,
};
