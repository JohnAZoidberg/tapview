//! Linux glue: the kernel exposes each HID device's raw report descriptor in
//! sysfs, next to its hidraw node.

use std::io;
use std::path::Path;

/// Read the raw report descriptor of the HID device behind a `/dev/hidraw*`
/// node from `/sys/class/hidraw/<name>/device/report_descriptor`.
pub fn read_report_descriptor(hidraw_path: &Path) -> io::Result<Vec<u8>> {
    let name = hidraw_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad hidraw path"))?;
    std::fs::read(format!(
        "/sys/class/hidraw/{}/device/report_descriptor",
        name
    ))
}
