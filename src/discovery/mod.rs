#[cfg(target_os = "linux")]
pub mod udev_discovery;
#[cfg(target_os = "windows")]
pub mod windows_discovery;

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub devnode: PathBuf,
    /// Human-readable device name as reported by the kernel/HID stack.
    pub name: Option<String>,
    /// Transport the device is connected over.
    pub bus: Bus,
    /// Whether this is an internal (built-in) touchpad, external, or unknown.
    pub integration: Integration,
    /// USB/HID vendor ID (if available).
    pub vendor_id: Option<u16>,
    /// USB/HID product ID (if available).
    pub product_id: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Internal/External used on Linux only
pub enum Integration {
    Internal,
    External,
    Unknown,
}

/// Transport a device is attached over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Not every variant is produced on every platform
pub enum Bus {
    Usb,
    Bluetooth,
    I2c,
    Spi,
    Ps2,
    Rmi,
    Virtual,
    Other(u16),
    Unknown,
}

impl Bus {
    /// Map a Linux `BUS_*` constant (as found in sysfs `id/bustype`) to a `Bus`.
    #[allow(dead_code)]
    pub fn from_linux_bustype(bustype: u16) -> Bus {
        match bustype {
            0x03 => Bus::Usb,
            0x05 => Bus::Bluetooth,
            0x06 => Bus::Virtual,
            0x11 => Bus::Ps2,
            0x18 => Bus::I2c,
            0x1c => Bus::Spi,
            0x1d => Bus::Rmi,
            other => Bus::Other(other),
        }
    }
}

impl std::fmt::Display for Bus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Bus::Usb => write!(f, "USB"),
            Bus::Bluetooth => write!(f, "BT"),
            Bus::I2c => write!(f, "I2C"),
            Bus::Spi => write!(f, "SPI"),
            Bus::Ps2 => write!(f, "PS/2"),
            Bus::Rmi => write!(f, "RMI"),
            Bus::Virtual => write!(f, "virtual"),
            Bus::Other(b) => write!(f, "bus 0x{:02x}", b),
            Bus::Unknown => write!(f, "unknown bus"),
        }
    }
}

#[derive(Debug)]
pub enum DiscoveryError {
    UdevError(String),
    NotFound,
}

impl std::fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DiscoveryError::UdevError(msg) => write!(f, "udev error: {}", msg),
            DiscoveryError::NotFound => write!(f, "no touchpad found"),
        }
    }
}

impl std::error::Error for DiscoveryError {}

impl std::fmt::Display for DeviceInfo {
    /// Formats as e.g.
    /// `/dev/input/event8  "PIXA3854:00 093A:0343 Touchpad"  [I2C, 093a:0343, internal]`
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.devnode.display())?;
        if let Some(name) = &self.name {
            write!(f, "  \"{}\"", name)?;
        }

        let mut details = vec![self.bus.to_string()];
        if self.vendor_id.is_some() && self.product_id.is_some() {
            details.push(self.vid_pid_string());
        }
        if self.integration != Integration::Unknown {
            details.push(self.integration_str().to_string());
        }
        write!(f, "  [{}]", details.join(", "))
    }
}

impl DeviceInfo {
    fn vid_pid_string(&self) -> String {
        match (self.vendor_id, self.product_id) {
            (Some(vid), Some(pid)) => format!("{:04x}:{:04x}", vid, pid),
            _ => "-".to_string(),
        }
    }

    fn integration_str(&self) -> &'static str {
        match self.integration {
            Integration::Internal => "internal",
            Integration::External => "external",
            Integration::Unknown => "-",
        }
    }

    /// Short device identifier shown in `--list` and accepted by `--device`.
    ///
    /// On Linux this is the devnode basename (`event8`). Windows HID interface
    /// paths are a single long path component, so trim the parts that are the
    /// same for every device: the `\\?\` prefix and the trailing HID
    /// interface-class GUID.
    pub fn display_id(&self) -> String {
        let raw = self.devnode.to_string_lossy();
        if let Some(rest) = raw.strip_prefix(r"\\?\") {
            let trimmed = rest.rfind("#{").map_or(rest, |i| &rest[..i]);
            return trimmed.to_string();
        }
        self.devnode
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| raw.into_owned())
    }
}

/// Render the discovered devices as an aligned table for `--list`.
///
/// The index in the first column is what `--device` and the interactive prompt
/// take, so a device is always selectable with a single digit — Windows HID
/// paths are far too long to retype. They are also too long to read, so the
/// DEVICE column is only shown with `--verbose` there; on Linux the short
/// `eventN` name is always worth showing. It comes last either way, so a
/// 100-character path does not push the other columns off screen.
///
/// ```text
/// #  NAME                   BUS  VID:PID    DEVICE
/// 1  PIXA3854:00 093A:0343  I2C  093a:0343  event8
/// ```
pub fn format_device_table(devices: &[DeviceInfo], verbose: bool) -> String {
    let show_device = verbose || !cfg!(target_os = "windows");

    let mut header = vec!["#", "NAME", "BUS", "VID:PID"];
    if show_device {
        header.push("DEVICE");
    }

    let rows: Vec<Vec<String>> = devices
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let mut row = vec![
                (i + 1).to_string(),
                d.name.clone().unwrap_or_else(|| "-".to_string()),
                d.bus.to_string(),
                d.vid_pid_string(),
            ];
            if show_device {
                row.push(d.display_id());
            }
            row
        })
        .collect();

    let mut widths: Vec<usize> = header.iter().map(|h| h.chars().count()).collect();
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row.iter()) {
            *w = (*w).max(cell.chars().count());
        }
    }

    let render = |cells: &[&str]| -> String {
        let mut line = String::new();
        for (i, (cell, w)) in cells.iter().zip(&widths).enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            if i == cells.len() - 1 {
                // Don't pad the last column, avoids trailing whitespace.
                line.push_str(cell);
            } else {
                line.push_str(&format!("{:<width$}", cell, width = w));
            }
        }
        line.push('\n');
        line
    };

    let mut out = render(&header);
    for row in &rows {
        let cells: Vec<&str> = row.iter().map(String::as_str).collect();
        out.push_str(&render(&cells));
    }
    out
}

/// Look up a device by user-supplied identifier: its index in the `--list`
/// table (`1`), the identifier from the DEVICE column (`event8`), the full
/// devnode path (`/dev/input/event8`), or — where that is not already an
/// index — the bare event number (`8`).
pub fn find_device<'a>(devices: &'a [DeviceInfo], wanted: &str) -> Option<&'a DeviceInfo> {
    let wanted = wanted.trim();

    // Index first: it is what the table shows next to each device.
    if let Some(n) = wanted.parse::<usize>().ok().filter(|n| *n >= 1) {
        if let Some(d) = devices.get(n - 1) {
            return Some(d);
        }
    }

    let path = std::path::Path::new(wanted);
    let as_event = wanted.parse::<u32>().ok().map(|n| format!("event{}", n));
    devices.iter().find(|d| {
        d.devnode == path
            || d.display_id() == wanted
            || d.devnode.file_name() == Some(path.as_os_str())
            || matches!(
                (&as_event, d.devnode.file_name()),
                (Some(ev), Some(name)) if name.to_string_lossy() == *ev
            )
    })
}

/// Show the device table and ask the user to pick one by index (or by any
/// identifier `find_device` accepts) on the terminal.
/// Prompts again on invalid input; returns `None` on EOF (Ctrl-D).
/// Everything goes to stderr so stdout stays clean for program output.
pub fn prompt_for_device(devices: &[DeviceInfo], verbose: bool) -> Option<DeviceInfo> {
    use std::io::{BufRead, Write};

    eprintln!("Multiple touchpads found:\n");
    eprint!("{}", format_device_table(devices, verbose));
    eprintln!();

    let default = "1";

    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        eprint!("Select device by number [{}]: ", default);
        let _ = std::io::stderr().flush();

        line.clear();
        match stdin.lock().read_line(&mut line) {
            Ok(0) | Err(_) => {
                eprintln!();
                return None;
            }
            Ok(_) => {}
        }

        let input = line.trim();
        if input.is_empty() {
            return Some(devices[0].clone());
        }
        match find_device(devices, input) {
            Some(d) => return Some(d.clone()),
            None => eprintln!("No such device: {}", input),
        }
    }
}

pub trait DeviceDiscovery {
    fn find_touchpads() -> Result<Vec<DeviceInfo>, DiscoveryError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(devnode: &str) -> DeviceInfo {
        DeviceInfo {
            devnode: PathBuf::from(devnode),
            name: None,
            bus: Bus::Unknown,
            integration: Integration::Unknown,
            vendor_id: None,
            product_id: None,
        }
    }

    #[test]
    fn display_id_trims_windows_hid_paths() {
        let d =
            dev(r"\\?\hid#pixa3854&col02#4&10d8260e&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}");
        assert_eq!(d.display_id(), "hid#pixa3854&col02#4&10d8260e&0&0001");
    }

    #[test]
    fn display_id_is_basename_on_unix_paths() {
        assert_eq!(dev("/dev/input/event8").display_id(), "event8");
    }

    #[test]
    fn verbose_table_shows_the_device_column() {
        let devices = [dev("/dev/input/event8")];
        assert!(format_device_table(&devices, true).contains("DEVICE"));
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn windows_table_hides_the_device_column_by_default() {
        let devices = [dev(
            r"\\?\hid#pixa3854&col02#4&10d8260e&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}",
        )];
        let table = format_device_table(&devices, false);
        assert!(!table.contains("DEVICE"));
        assert!(!table.contains("pixa3854"));
    }

    #[test]
    fn find_device_by_index() {
        let devices = [dev("/dev/input/event8"), dev("/dev/input/event12")];
        assert_eq!(
            find_device(&devices, "2").map(|d| d.devnode.clone()),
            Some(PathBuf::from("/dev/input/event12"))
        );
        assert!(find_device(&devices, "0").is_none());
        assert!(find_device(&devices, "3").is_none());
    }

    #[test]
    fn find_device_by_path_and_display_id() {
        let win =
            r"\\?\hid#pixa3854&col02#4&10d8260e&0&0001#{4d1e55b2-f16f-11cf-88cb-001111000030}";
        let devices = [dev("/dev/input/event8"), dev(win)];
        assert!(find_device(&devices, "event8").is_some());
        assert!(find_device(&devices, "/dev/input/event8").is_some());
        assert!(find_device(&devices, win).is_some());
        assert!(find_device(&devices, "hid#pixa3854&col02#4&10d8260e&0&0001").is_some());
        assert!(find_device(&devices, "event9").is_none());
    }

    #[test]
    fn find_device_falls_back_to_event_number() {
        // "8" is not a valid index here, so it means event8.
        let devices = [dev("/dev/input/event8")];
        assert!(find_device(&devices, "8").is_some());
    }
}
