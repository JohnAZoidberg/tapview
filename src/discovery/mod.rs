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
}

/// Render the discovered devices as an aligned table for `--list`.
///
/// Kept narrow on purpose (only the devnode basename, no index column) so
/// it fits in a small terminal.
///
/// ```text
/// DEVICE   NAME                   BUS  VID:PID
/// event8   PIXA3854:00 093A:0343  I2C  093a:0343
/// ```
pub fn format_device_table(devices: &[DeviceInfo]) -> String {
    let header = ["DEVICE", "NAME", "BUS", "VID:PID"];
    let rows: Vec<[String; 4]> = devices
        .iter()
        .map(|d| {
            let devnode = d
                .devnode
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| d.devnode.display().to_string());
            [
                devnode,
                d.name.clone().unwrap_or_else(|| "-".to_string()),
                d.bus.to_string(),
                d.vid_pid_string(),
            ]
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

pub trait DeviceDiscovery {
    fn find_touchpads() -> Result<Vec<DeviceInfo>, DiscoveryError>;
}
