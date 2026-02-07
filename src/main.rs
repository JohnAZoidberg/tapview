mod app;
mod dimensions;
mod discovery;
mod heatmap;
mod input;
mod libinput_backend;
mod libinput_state;
mod multitouch;
mod render;

use app::{GrabCommand, TapviewApp};
use clap::Parser;
use discovery::udev_discovery::UdevDiscovery;
use discovery::DeviceDiscovery;
use input::evdev_backend::EvdevBackend;
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

    /// Show libinput debug-events in a side panel
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
    let devices = match UdevDiscovery::find_touchpads() {
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
                    // No events available, sleep briefly
                    thread::sleep(Duration::from_millis(5));
                }
                Err(e) => {
                    eprintln!("Input error: {}", e);
                    break;
                }
            }
        }
    });

    // Optionally spawn libinput backend thread
    let libinput_rx = if cli.libinput {
        Some(libinput_backend::spawn_libinput_thread(&device.devnode))
    } else {
        None
    };

    // Optionally spawn heatmap backend thread
    let heatmap_rx = if cli.heatmap {
        match heatmap::discovery::find_sibling_hidraw(&device.devnode) {
            Ok(hidraw_path) => {
                eprintln!("heatmap: found hidraw device: {}", hidraw_path.display());
                match heatmap::discovery::determine_burst_report_length(&hidraw_path) {
                    Ok(burst_len) => {
                        eprintln!("heatmap: burst report length = {}", burst_len);
                        Some(heatmap::backend::spawn_heatmap_thread(
                            &hidraw_path,
                            burst_len,
                            cli.heatmap_cols,
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
