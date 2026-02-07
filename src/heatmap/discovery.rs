use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Given an evdev device path (e.g. /dev/input/event5), find the sibling
/// hidraw device by walking udev parents to the HID device level and
/// enumerating hidraw children.
pub fn find_sibling_hidraw(evdev_path: &Path) -> io::Result<PathBuf> {
    // Resolve the evdev sysfs path via udev
    let evdev_name = evdev_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad evdev path"))?;

    // Find the sysfs path for this evdev device
    let mut enumerator = udev::Enumerator::new().map_err(io::Error::other)?;
    enumerator
        .match_subsystem("input")
        .map_err(io::Error::other)?;
    enumerator
        .match_sysname(evdev_name)
        .map_err(io::Error::other)?;

    let evdev_dev = enumerator
        .scan_devices()
        .map_err(io::Error::other)?
        .next()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("evdev device not found in udev: {}", evdev_name),
            )
        })?;

    // Walk parents until we find one in the "hid" subsystem
    let mut current_path = evdev_dev.syspath().to_path_buf();
    let hid_path = loop {
        current_path = current_path
            .parent()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "no HID parent found for evdev device",
                )
            })?;

        // Check if this directory has a subsystem symlink pointing to "hid"
        let subsystem_link = current_path.join("subsystem");
        if let Ok(target) = fs::read_link(&subsystem_link) {
            if let Some(name) = target.file_name().and_then(|n| n.to_str()) {
                if name == "hid" {
                    break current_path;
                }
            }
        }

        // Stop at /sys
        if current_path.as_os_str() == "/sys" || current_path.parent().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no HID parent found for evdev device",
            ));
        }
    };

    // Now enumerate hidraw children under this HID device
    let mut hidraw_enum = udev::Enumerator::new().map_err(io::Error::other)?;
    hidraw_enum
        .match_subsystem("hidraw")
        .map_err(io::Error::other)?;
    hidraw_enum
        .match_parent(&udev::Device::from_syspath(&hid_path).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;

    for hidraw_dev in hidraw_enum.scan_devices().map_err(io::Error::other)? {
        if let Some(devnode) = hidraw_dev.devnode() {
            return Ok(devnode.to_path_buf());
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no sibling hidraw device found",
    ))
}

/// Determine the burst report length for Report ID 0x41 by parsing the
/// HID report descriptor from sysfs.
pub fn determine_burst_report_length(hidraw_path: &Path) -> io::Result<usize> {
    let hidraw_name = hidraw_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad hidraw path"))?;

    let desc_path = format!("/sys/class/hidraw/{}/device/report_descriptor", hidraw_name);
    let desc = fs::read(&desc_path)?;

    parse_report_descriptor_for_burst_len(&desc).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "could not find Report ID 0x41 ReportCount in HID descriptor",
        )
    })
}

/// Minimal HID descriptor parser: find the Feature report with Report ID 0x41
/// and return its ReportCount value (which is the burst payload length).
fn parse_report_descriptor_for_burst_len(desc: &[u8]) -> Option<usize> {
    let mut i = 0;
    let mut current_report_id: Option<u8> = None;
    let mut current_report_count: Option<usize> = None;

    while i < desc.len() {
        let prefix = desc[i];

        // Long item
        if prefix == 0xFE {
            if i + 2 >= desc.len() {
                break;
            }
            let data_size = desc[i + 1] as usize;
            i += 3 + data_size;
            continue;
        }

        // Short item
        let size = match prefix & 0x03 {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 4,
            _ => unreachable!(),
        };

        if i + 1 + size > desc.len() {
            break;
        }

        let tag = prefix & 0xFC;
        let data = &desc[i + 1..i + 1 + size];

        match tag {
            // Report ID (Global, tag = 0x84)
            0x84 => {
                if let Some(&id) = data.first() {
                    current_report_id = Some(id);
                    current_report_count = None;
                }
            }
            // Report Count (Global, tag = 0x94)
            0x94 => {
                let count = match size {
                    1 => data[0] as usize,
                    2 => u16::from_le_bytes([data[0], data[1]]) as usize,
                    4 => u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize,
                    _ => 0,
                };
                current_report_count = Some(count);
            }
            // Feature (Main, tag = 0xB0)
            0xB0 => {
                if current_report_id == Some(0x41) {
                    if let Some(count) = current_report_count {
                        return Some(count);
                    }
                }
            }
            _ => {}
        }

        i += 1 + size;
    }

    None
}
