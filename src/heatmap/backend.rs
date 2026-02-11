use super::chips::{
    alc_disable, alc_enable, alc_is_enabled, alc_reset, identify_chip, read_frame,
    read_matrix_dims, ChipVariant,
};
use super::hidraw::HidrawDevice;
use super::protocol::{read_reg, read_user_reg};
use super::{AlcCommand, HeatmapFrame};
use std::path::Path;
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

/// EMA smoothing factor for the baseline tracker.
/// Small alpha = slow response, filters out transient touches.
/// At ~100 Hz frame rate, alpha=0.005 gives ~200-frame (~2s) smoothing.
const EMA_ALPHA: f64 = 0.005;

/// Number of frames over which to measure baseline drift rate.
/// At ~100 Hz this is ~5 seconds worth of smoothed baseline history.
const DRIFT_WINDOW: usize = 500;

/// Drift rate threshold (smoothed-mean units per frame) to flag active calibration.
/// If the smoothed baseline drifts more than this per frame, sustained over
/// DRIFT_WINDOW frames, we flag it as firmware calibration.
const DRIFT_THRESHOLD: f64 = 0.02;

/// Spawn a background thread that continuously reads raw capacitive frames
/// and sends them over a channel. Accepts ALC commands on `cmd_rx`.
pub fn spawn_heatmap_thread(
    hidraw_path: &Path,
    burst_len: usize,
    cols_override: Option<usize>,
    cmd_rx: mpsc::Receiver<AlcCommand>,
) -> mpsc::Receiver<HeatmapFrame> {
    let (tx, rx) = mpsc::channel();
    let path = hidraw_path.to_path_buf();

    thread::spawn(move || {
        let dev = match HidrawDevice::open(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("heatmap: failed to open {}: {}", path.display(), e);
                return;
            }
        };

        let chip = match identify_chip(&dev) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("heatmap: failed to identify chip: {}", e);
                return;
            }
        };

        let (rows, cols) = match read_matrix_dims(&dev, chip) {
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
            probe_dimension_registers(&dev);
        }

        // Display cols can be overridden for stride debugging
        let display_cols = cols_override.unwrap_or(cols);
        if cols_override.is_some() {
            eprintln!("heatmap: display cols overridden to {}", display_cols);
        }

        // Log initial ALC state
        match alc_is_enabled(&dev, chip) {
            Ok(enabled) => eprintln!("heatmap: ALC is {}", if enabled { "enabled" } else { "disabled" }),
            Err(e) => eprintln!("heatmap: failed to read ALC state: {}", e),
        }

        let start_time = Instant::now();
        let mut frame_count: u64 = 0;
        let mut ema: Option<f64> = None;
        // Ring buffer of smoothed means for drift rate computation
        let mut ema_history = Vec::with_capacity(DRIFT_WINDOW);
        let mut was_calibrating = false;

        loop {
            // Process any pending ALC commands
            while let Ok(cmd) = cmd_rx.try_recv() {
                let elapsed = start_time.elapsed().as_secs_f64();
                match cmd {
                    AlcCommand::Reset => {
                        eprintln!("heatmap: ALC reset at {:.1}s", elapsed);
                        if let Err(e) = alc_reset(&dev, chip) {
                            eprintln!("heatmap: ALC reset failed: {}", e);
                        }
                    }
                    AlcCommand::Enable => {
                        eprintln!("heatmap: ALC enable at {:.1}s", elapsed);
                        if let Err(e) = alc_enable(&dev, chip) {
                            eprintln!("heatmap: ALC enable failed: {}", e);
                        }
                    }
                    AlcCommand::Disable => {
                        eprintln!("heatmap: ALC disable at {:.1}s", elapsed);
                        if let Err(e) = alc_disable(&dev, chip) {
                            eprintln!("heatmap: ALC disable failed: {}", e);
                        }
                    }
                }
            }

            // Hardware read always uses register-derived dimensions
            match read_frame(&dev, chip, rows, cols, burst_len) {
                Ok(data) => {
                    frame_count += 1;
                    let display_rows = data.len() / display_cols;

                    // Compute raw mean
                    let sum: f64 = data.iter().map(|&v| v as f64).sum();
                    let mean = sum / data.len() as f64;

                    // Update slow EMA (smoothed baseline that ignores transient touches)
                    let smoothed_mean = match ema {
                        Some(prev) => prev + EMA_ALPHA * (mean - prev),
                        None => mean,
                    };
                    ema = Some(smoothed_mean);

                    // Track EMA history for drift rate
                    if ema_history.len() >= DRIFT_WINDOW {
                        ema_history.remove(0);
                    }
                    ema_history.push(smoothed_mean);

                    // Drift rate: change in smoothed baseline over the window
                    let drift_rate = if ema_history.len() >= 2 {
                        let oldest = ema_history[0];
                        (smoothed_mean - oldest) / ema_history.len() as f64
                    } else {
                        0.0
                    };

                    let calibrating = ema_history.len() >= DRIFT_WINDOW
                        && drift_rate.abs() > DRIFT_THRESHOLD;

                    // Log transitions
                    if calibrating && !was_calibrating {
                        let elapsed = start_time.elapsed().as_secs_f64();
                        eprintln!(
                            "heatmap: CALIBRATING started at {:.1}s (frame {}): drift_rate={:.4}/frame, smoothed_mean={:.1}",
                            elapsed, frame_count, drift_rate, smoothed_mean
                        );
                    } else if !calibrating && was_calibrating {
                        let elapsed = start_time.elapsed().as_secs_f64();
                        eprintln!(
                            "heatmap: CALIBRATING stopped at {:.1}s (frame {}): drift_rate={:.4}/frame, smoothed_mean={:.1}",
                            elapsed, frame_count, drift_rate, smoothed_mean
                        );
                    }
                    was_calibrating = calibrating;

                    let frame = HeatmapFrame {
                        rows: display_rows,
                        cols: display_cols,
                        data,
                        mean,
                        smoothed_mean,
                        drift_rate,
                        calibrating,
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
    });

    rx
}

fn probe_dimension_registers(dev: &HidrawDevice) {
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
