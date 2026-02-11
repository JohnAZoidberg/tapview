mod app;
mod dimensions;
mod discovery;
mod heatmap;
mod input;
#[cfg(target_os = "linux")]
mod libinput_backend;
mod libinput_state;
mod multitouch;
mod render;
#[cfg(target_os = "windows")]
mod windows_input_backend;

use app::{GrabCommand, TapviewApp};
use clap::Parser;
#[cfg(target_os = "linux")]
use discovery::udev_discovery::UdevDiscovery;
#[cfg(target_os = "windows")]
use discovery::windows_discovery::WindowsDiscovery;
use discovery::DeviceDiscovery;
#[cfg(target_os = "linux")]
use input::evdev_backend::EvdevBackend;
#[cfg(target_os = "windows")]
use input::windows_backend::WindowsBackend;
use input::InputBackend;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "tapview", about = "Touchpad Visualizer")]
struct Cli {
    /// Number of trail frames to show (max 20)
    #[arg(short, long, default_value_t = 20)]
    trails: usize,

    /// Enable verbose event logging to stderr
    #[arg(short, long)]
    verbose: bool,

    /// Show interpreted input in a side panel (libinput on Linux, mouse/scroll on Windows)
    #[arg(short, long)]
    libinput: bool,

    /// Show raw capacitive heatmap (PixArt touchpads only)
    #[arg(long)]
    heatmap: bool,

    /// Override heatmap column count (for debugging stride issues)
    #[arg(long)]
    heatmap_cols: Option<usize>,
}

fn main() {
    let cli = Cli::parse();
    let trails = cli.trails.min(20);

    // Discover touchpad
    #[cfg(target_os = "linux")]
    let devices = UdevDiscovery::find_touchpads();
    #[cfg(target_os = "windows")]
    let devices = WindowsDiscovery::find_touchpads();

    let devices = match devices {
        Ok(d) => d,
        Err(e) => {
            eprintln!("Unable to find touchpad: {}", e);
            std::process::exit(1);
        }
    };

    let device = &devices[0];
    eprintln!("Found touchpad: {}", device.devnode.display());

    // Create channels
    let (touch_tx, touch_rx) = mpsc::channel();
    let (grab_tx, grab_rx) = mpsc::channel::<GrabCommand>();

    // Spawn input thread
    let device_path = device.devnode.clone();
    let verbose = cli.verbose;

    #[cfg(target_os = "linux")]
    thread::spawn(move || {
        let mut backend = match EvdevBackend::open_with_verbose(&device_path, verbose) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Failed to open device: {}", e);
                return;
            }
        };

        loop {
            // Check for grab/ungrab commands
            if let Ok(cmd) = grab_rx.try_recv() {
                match cmd {
                    GrabCommand::Grab => {
                        if let Err(e) = backend.grab() {
                            eprintln!("Grab failed: {}", e);
                        }
                    }
                    GrabCommand::Ungrab => {
                        if let Err(e) = backend.ungrab() {
                            eprintln!("Ungrab failed: {}", e);
                        }
                    }
                }
            }

            match backend.poll_events() {
                Ok(Some(state)) => {
                    let _ = touch_tx.send(state);
                }
                Ok(None) => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => {
                    eprintln!("Input error: {}", e);
                    break;
                }
            }
        }
    });

    #[cfg(target_os = "windows")]
    thread::spawn(move || {
        let _ = verbose; // verbose logging not yet implemented for Windows
        let mut backend = match WindowsBackend::open(&device_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Failed to open device: {}", e);
                return;
            }
        };

        loop {
            if let Ok(cmd) = grab_rx.try_recv() {
                match cmd {
                    GrabCommand::Grab => {
                        if let Err(e) = backend.grab() {
                            eprintln!("Grab failed: {}", e);
                        }
                    }
                    GrabCommand::Ungrab => {
                        if let Err(e) = backend.ungrab() {
                            eprintln!("Ungrab failed: {}", e);
                        }
                    }
                }
            }

            match backend.poll_events() {
                Ok(Some(state)) => {
                    let _ = touch_tx.send(state);
                }
                Ok(None) => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => {
                    eprintln!("Input error: {}", e);
                    break;
                }
            }
        }
    });

    // Optionally spawn libinput/interpreted input backend thread
    #[cfg(target_os = "linux")]
    let libinput_rx = if cli.libinput {
        Some(libinput_backend::spawn_libinput_thread(&device.devnode))
    } else {
        None
    };

    #[cfg(target_os = "windows")]
    let libinput_rx = if cli.libinput {
        Some(windows_input_backend::spawn_windows_input_thread())
    } else {
        None
    };

    // Optionally spawn heatmap backend thread
    let heatmap_rx = if cli.heatmap {
        spawn_heatmap(device, cli.heatmap_cols)
    } else {
        None
    };

    // Run eframe
    let initial_width = if cli.libinput { 1100.0 } else { 672.0 };
    let initial_height = if cli.heatmap { 650.0 } else { 432.0 };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([initial_width, initial_height])
            .with_min_inner_size([320.0, 240.0])
            .with_title("Tapview - Touchpad Visualizer")
            .with_always_on_top(),
        ..Default::default()
    };

    eframe::run_native(
        "Tapview",
        options,
        Box::new(move |_cc| {
            Ok(Box::new(TapviewApp::new(
                touch_rx,
                grab_tx,
                libinput_rx,
                heatmap_rx,
                trails,
            )))
        }),
    )
    .expect("Failed to run eframe");
}

#[cfg(target_os = "linux")]
fn spawn_heatmap(
    device: &discovery::DeviceInfo,
    heatmap_cols: Option<usize>,
) -> Option<std::sync::mpsc::Receiver<heatmap::HeatmapFrame>> {
    match heatmap::discovery::find_sibling_hidraw(&device.devnode) {
        Ok(hidraw_path) => {
            eprintln!("heatmap: found hidraw device: {}", hidraw_path.display());
            match heatmap::discovery::determine_burst_report_length(&hidraw_path) {
                Ok(burst_len) => {
                    eprintln!("heatmap: burst report length = {}", burst_len);
                    Some(heatmap::backend::spawn_heatmap_thread(
                        &hidraw_path,
                        burst_len,
                        heatmap_cols,
                    ))
                }
                Err(e) => {
                    eprintln!("heatmap: failed to determine burst length: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("heatmap: failed to find sibling hidraw device: {}", e);
            std::process::exit(1);
        }
    }
}

#[cfg(target_os = "windows")]
fn spawn_heatmap(
    device: &discovery::DeviceInfo,
    heatmap_cols: Option<usize>,
) -> Option<std::sync::mpsc::Receiver<heatmap::HeatmapFrame>> {
    match heatmap::discovery::find_hid_device_for_heatmap(&device.devnode) {
        Ok((hid_path, burst_len)) => {
            eprintln!(
                "heatmap: found HID device: {}, burst_len={}",
                hid_path.display(),
                burst_len
            );
            Some(heatmap::backend::spawn_heatmap_thread(
                &hid_path,
                burst_len,
                heatmap_cols,
            ))
        }
        Err(e) => {
            eprintln!("heatmap: {}", e);
            std::process::exit(1);
        }
    }
}
