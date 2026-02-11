use super::chips::{identify_chip, read_frame, read_matrix_dims, ChipVariant};
use super::protocol::{read_reg, read_user_reg};
use super::HeatmapFrame;
use super::HidDevice;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

/// Spawn a background thread that continuously reads raw capacitive frames
/// and sends them over a channel.
pub fn spawn_heatmap_thread(
    hidraw_path: &Path,
    burst_len: usize,
    cols_override: Option<usize>,
) -> mpsc::Receiver<HeatmapFrame> {
    let (tx, rx) = mpsc::channel();
    let path = hidraw_path.to_path_buf();

    thread::spawn(move || {
        let dev: Box<dyn HidDevice> = match open_hid_device(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("heatmap: failed to open {}: {}", path.display(), e);
                return;
            }
        };

        run_heatmap_loop(&*dev, burst_len, cols_override, &tx);
    });

    rx
}

#[cfg(target_os = "linux")]
fn open_hid_device(path: &Path) -> std::io::Result<Box<dyn HidDevice>> {
    Ok(Box::new(super::hidraw::HidrawDevice::open(path)?))
}

#[cfg(target_os = "windows")]
fn open_hid_device(path: &Path) -> std::io::Result<Box<dyn HidDevice>> {
    Ok(Box::new(super::windows_hid::WinHidDevice::open(path)?))
}

fn run_heatmap_loop(
    dev: &dyn HidDevice,
    burst_len: usize,
    cols_override: Option<usize>,
    tx: &mpsc::Sender<HeatmapFrame>,
) {
    let chip = match identify_chip(dev) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("heatmap: failed to identify chip: {}", e);
            return;
        }
    };

    let (rows, cols) = match read_matrix_dims(dev, chip) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("heatmap: failed to read matrix dimensions: {}", e);
            return;
        }
    };

    eprintln!(
        "heatmap: {} detected, {}x{} matrix, burst_len={}",
        chip, rows, cols, burst_len
    );

    // Dump candidate dimension registers for unknown/new chips
    if chip == ChipVariant::PJP343 {
        probe_dimension_registers(dev);
    }

    // Display cols can be overridden for stride debugging
    let display_cols = cols_override.unwrap_or(cols);
    if cols_override.is_some() {
        eprintln!("heatmap: display cols overridden to {}", display_cols);
    }

    loop {
        // Hardware read always uses register-derived dimensions
        match read_frame(dev, chip, rows, cols, burst_len) {
            Ok(data) => {
                let display_rows = data.len() / display_cols;
                let frame = HeatmapFrame {
                    rows: display_rows,
                    cols: display_cols,
                    data,
                };
                if tx.send(frame).is_err() {
                    // Receiver dropped, UI closed
                    break;
                }
            }
            Err(e) => {
                eprintln!("heatmap: frame read error: {}", e);
                break;
            }
        }
    }
}

fn probe_dimension_registers(dev: &dyn HidDevice) {
    eprintln!("heatmap: --- PJP343 register probe ---");

    // PJP274 style: UserBank 0, 0x6E/0x6F
    if let (Ok(s), Ok(d)) = (read_user_reg(dev, 0, 0x6E), read_user_reg(dev, 0, 0x6F)) {
        eprintln!("  UserBank0 0x6E(senses)={} 0x6F(drives)={}", s, d);
    }
    // Check adjacent registers for 16-bit values
    if let (Ok(a), Ok(b), Ok(c), Ok(d)) = (
        read_user_reg(dev, 0, 0x6C),
        read_user_reg(dev, 0, 0x6D),
        read_user_reg(dev, 0, 0x70),
        read_user_reg(dev, 0, 0x71),
    ) {
        eprintln!("  UserBank0 0x6C={} 0x6D={} 0x70={} 0x71={}", a, b, c, d);
    }

    // PJP255 style: UserBank 0, 0x59/0x5A
    if let (Ok(s), Ok(d)) = (read_user_reg(dev, 0, 0x59), read_user_reg(dev, 0, 0x5A)) {
        eprintln!("  UserBank0 0x59(senses)={} 0x5A(drives)={}", s, d);
    }

    // PLP239 style: Bank 9, 0x01/0x02
    if let (Ok(d), Ok(s)) = (read_reg(dev, 9, 0x01), read_reg(dev, 9, 0x02)) {
        eprintln!("  Bank9 0x01(drives)={} 0x02(senses)={}", d, s);
    }

    // Scan UserBank 0 around 0x60-0x7F for anything that looks like a dimension
    eprint!("  UserBank0 0x60..0x7F:");
    for addr in 0x60..=0x7F {
        if let Ok(v) = read_user_reg(dev, 0, addr) {
            eprint!(" {:02X}={}", addr, v);
        }
    }
    eprintln!();
    eprintln!("heatmap: --- end probe ---");
}
