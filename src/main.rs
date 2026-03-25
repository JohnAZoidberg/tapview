mod app;
mod config;
mod dimensions;
mod discovery;
mod heatmap;
mod input;
#[cfg(target_os = "linux")]
mod libinput_backend;
mod libinput_state;
mod multitouch;
mod recording;
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

    /// Force interpreted input panel (exit if unavailable). Auto-enabled by default.
    #[arg(short, long, conflicts_with = "no_libinput")]
    libinput: bool,

    /// Disable interpreted input panel
    #[arg(long)]
    no_libinput: bool,

    /// Force raw capacitive heatmap (exit if unavailable). Auto-enabled for compatible hardware.
    #[arg(long, conflicts_with = "no_heatmap")]
    heatmap: bool,

    /// Disable raw capacitive heatmap
    #[arg(long)]
    no_heatmap: bool,

    /// Force PTP configuration panel (exit if unavailable). Auto-enabled for compatible hardware.
    #[arg(long, conflicts_with = "no_config")]
    config: bool,

    /// Disable PTP configuration panel
    #[arg(long)]
    no_config: bool,

    /// Override heatmap column count (for debugging stride issues)
    #[arg(long)]
    heatmap_cols: Option<usize>,

    /// List detected touchpads and exit
    #[arg(long)]
    list: bool,

    /// Print device info (axis ranges, PTP config) and exit without launching the UI
    #[arg(long)]
    info: bool,

    /// Use a specific device path instead of auto-detection
    #[arg(long)]
    device: Option<String>,

    /// Record touch session to a binary file
    #[arg(long, conflicts_with = "play")]
    record: Option<String>,

    /// Play back a recorded touch session (no device needed)
    #[arg(long, conflicts_with_all = ["record", "device", "libinput", "heatmap", "config"])]
    play: Option<String>,
}

fn main() {
    let cli = Cli::parse();
    let trails = cli.trails.min(20);

    // --- Playback mode: no device needed ---
    if let Some(ref play_path) = cli.play {
        let rec = match recording::Recording::load(play_path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Failed to load recording: {}", e);
                std::process::exit(1);
            }
        };
        eprintln!(
            "Loaded recording: {} frames, {:.1}s",
            rec.frames.len(),
            rec.duration_secs()
        );

        let evdev_extents = if rec.extent_x > 0 && rec.extent_y > 0 {
            Some((rec.extent_x, rec.extent_y))
        } else {
            None
        };

        // Dummy channels (not used during playback)
        let (_touch_tx, touch_rx) = mpsc::channel();
        let (grab_tx, _grab_rx) = mpsc::channel::<GrabCommand>();

        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([672.0, 480.0])
                .with_min_inner_size([320.0, 240.0])
                .with_title("Tapview - Touchpad Visualizer (Playback)")
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
                    None,
                    None,
                    None,
                    evdev_extents,
                    trails,
                    None,
                    Some(rec),
                )))
            }),
        )
        .expect("Failed to run eframe");
        return;
    }

    // --- Normal / Recording mode: need a device ---

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

    if cli.list {
        for (i, d) in devices.iter().enumerate() {
            println!("{}: {}", i, d);
        }
        std::process::exit(0);
    }

    let device = if let Some(ref path) = cli.device {
        let path = std::path::PathBuf::from(path);
        match devices.iter().find(|d| d.devnode == path) {
            Some(d) => d.clone(),
            None => {
                eprintln!("Device {} not found among detected touchpads. Use --list to see available devices.", path.display());
                std::process::exit(1);
            }
        }
    } else {
        devices[0].clone()
    };
    eprintln!("Found touchpad: {}", device);

    // Read evdev axis extents (post-kernel-swap, matches actual event coordinates)
    #[cfg(target_os = "linux")]
    let evdev_extents = input::evdev_backend::read_axis_extents(&device.devnode);
    #[cfg(target_os = "windows")]
    let evdev_extents = None;

    // Discover PTP configuration features (auto-detected by default, forced with --config)
    let ptp_config = if cli.no_config && !cli.info {
        None
    } else {
        let cfg = config::discover(&device.devnode);
        if cfg.is_none() && cli.config {
            eprintln!("config: no PTP configuration features found");
            std::process::exit(1);
        }
        cfg
    };

    // Log and compare axis ranges from both sources
    if let Some((ex, ey)) = &evdev_extents {
        eprintln!("axis: evdev extents: x=0..{}, y=0..{}", ex, ey);
    }
    let axis_swap_detected = if let Some(cfg) = &ptp_config {
        if let Some(phys) = &cfg.physical_size {
            eprintln!(
                "axis: HID descriptor: x={}..{}, y={}..{}",
                phys.x.logical_min, phys.x.logical_max, phys.y.logical_min, phys.y.logical_max
            );
            if let Some((ex, ey)) = &evdev_extents {
                if *ex != phys.x.logical_max || *ey != phys.y.logical_max {
                    eprintln!("axis: evdev and HID descriptor disagree!");
                    if *ex == phys.y.logical_max && *ey == phys.x.logical_max {
                        eprintln!("axis: looks like a kernel axis swap");
                        Some(true)
                    } else {
                        Some(false)
                    }
                } else {
                    Some(false)
                }
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    // --info: print device info and exit without launching UI
    if cli.info {
        println!("Device");
        println!("  Path:             {}", device.devnode.display());
        println!("  Integration:      {:?}", device.integration);
        println!();

        if let Some((ex, ey)) = &evdev_extents {
            println!("Evdev axes");
            println!("  X range:          0..{}", ex);
            println!("  Y range:          0..{}", ey);
            println!();
        }

        if let Some(cfg) = &ptp_config {
            if let Some(phys) = &cfg.physical_size {
                println!("HID descriptor");
                println!(
                    "  X logical:        {}..{}",
                    phys.x.logical_min, phys.x.logical_max
                );
                println!(
                    "  X physical:       {}..{}",
                    phys.x.physical_min, phys.x.physical_max
                );
                println!(
                    "  X size:           {:.1} mm ({:.1} units/mm)",
                    phys.x.size_mm, phys.x.resolution
                );
                println!(
                    "  Y logical:        {}..{}",
                    phys.y.logical_min, phys.y.logical_max
                );
                println!(
                    "  Y physical:       {}..{}",
                    phys.y.physical_min, phys.y.physical_max
                );
                println!(
                    "  Y size:           {:.1} mm ({:.1} units/mm)",
                    phys.y.size_mm, phys.y.resolution
                );
                println!();
            }

            println!("PTP config");
            if let Some(mode) = cfg.input_mode {
                println!(
                    "  Input Mode:       {} ({})",
                    render::input_mode_label(mode),
                    mode
                );
            }
            if let Some(pt) = cfg.pad_type {
                println!(
                    "  Pad Type:         {} ({})",
                    render::pad_type_label(pt),
                    pt
                );
            }
            if let Some(max) = cfg.contact_count_max {
                println!("  Max Contacts:     {}", max);
            }
            if cfg.features.has_surface_switch {
                println!(
                    "  Surface Switch:   {}",
                    cfg.surface_switch
                        .map_or("n/a".to_string(), |v| v.to_string())
                );
            }
            if cfg.features.has_button_switch {
                println!(
                    "  Button Switch:    {}",
                    cfg.button_switch
                        .map_or("n/a".to_string(), |v| v.to_string())
                );
            }
            if let Some(lat) = cfg.latency_mode {
                println!("  Latency Mode:     {}", if lat { "low" } else { "normal" });
            }
            if let Some(thresh) = cfg.button_press_threshold {
                println!("  Btn Threshold:    {}", thresh);
            }
            println!();
        } else {
            println!("PTP config:         not available");
            println!();
        }

        print!("Axis swap:          ");
        match axis_swap_detected {
            Some(true) => println!("detected (evdev axes swapped vs HID descriptor)"),
            Some(false) => println!("none"),
            None => println!("unknown (insufficient data)"),
        }
        std::process::exit(0);
    }

    // Create recorder if --record was specified
    let recorder = if let Some(ref record_path) = cli.record {
        let (ex, ey) = evdev_extents.unwrap();
        match recording::Recorder::new(record_path, ex, ey) {
            Ok(r) => {
                eprintln!("Recording to: {}", record_path);
                Some(r)
            }
            Err(e) => {
                eprintln!("Failed to create recording file: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        None
    };

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

    // Spawn libinput/interpreted input backend thread (enabled by default)
    #[cfg(target_os = "linux")]
    let libinput_rx = if !cli.no_libinput {
        Some(libinput_backend::spawn_libinput_thread(&device.devnode))
    } else {
        None
    };

    #[cfg(target_os = "windows")]
    let libinput_rx = if !cli.no_libinput {
        Some(windows_input_backend::spawn_windows_input_thread())
    } else {
        None
    };

    // Spawn heatmap backend thread (auto-detected by default, forced with --heatmap)
    let heatmap_rx = if cli.no_heatmap {
        None
    } else {
        spawn_heatmap(&device, cli.heatmap_cols, cli.heatmap)
    };

    // Run eframe
    let is_recording = recorder.is_some();
    let mut initial_width = if libinput_rx.is_some() { 1100.0 } else { 672.0 };
    if ptp_config.is_some() {
        initial_width += 220.0;
    }
    let initial_height = if heatmap_rx.is_some() { 650.0 } else { 432.0 };
    let title = if is_recording {
        "Tapview - Touchpad Visualizer (Recording)"
    } else {
        "Tapview - Touchpad Visualizer"
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([initial_width, initial_height])
            .with_min_inner_size([320.0, 240.0])
            .with_title(title)
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
                ptp_config,
                evdev_extents,
                trails,
                recorder,
                None,
            )))
        }),
    )
    .expect("Failed to run eframe");
}

#[cfg(target_os = "linux")]
fn spawn_heatmap(
    device: &discovery::DeviceInfo,
    heatmap_cols: Option<usize>,
    force: bool,
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
                    if force {
                        eprintln!("heatmap: failed to determine burst length: {}", e);
                        std::process::exit(1);
                    }
                    None
                }
            }
        }
        Err(e) => {
            if force {
                eprintln!("heatmap: failed to find sibling hidraw device: {}", e);
                std::process::exit(1);
            }
            None
        }
    }
}

#[cfg(target_os = "windows")]
fn spawn_heatmap(
    device: &discovery::DeviceInfo,
    heatmap_cols: Option<usize>,
    force: bool,
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
            if force {
                eprintln!("heatmap: {}", e);
                std::process::exit(1);
            }
            None
        }
    }
}
