//! The native command-line interface: argument parsing, device selection,
//! the `--info`/`--set-*` one-shot paths, and assembling the input, heatmap
//! and config threads before handing over to the egui UI. Without
//! `--device`, the window picks and switches pads itself ([`picker`]).

use std::io::IsTerminal;

use crate::app::TapviewApp;
use crate::config::{ConfigBackend, ConfigDescription, ConfigState, PlatformConfigBackend};
#[cfg(target_os = "linux")]
use crate::discovery::udev_discovery::UdevDiscovery;
#[cfg(target_os = "windows")]
use crate::discovery::windows_discovery::WindowsDiscovery;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use crate::discovery::DeviceDiscovery;
#[cfg(target_os = "linux")]
use crate::input::evdev_backend::EvdevBackend;
#[cfg(target_os = "linux")]
use crate::input::hidraw_backend::HidrawBackend;
#[cfg(target_os = "macos")]
use crate::input::macos_backend::MacBackend;
#[cfg(target_os = "windows")]
use crate::input::windows_backend::WindowsBackend;
use crate::input::InputBackend;
#[cfg(target_os = "linux")]
use crate::libinput_backend;
use crate::libinput_state::LibinputEvent;
use crate::session::{GrabCommand, Session};
#[cfg(target_os = "windows")]
use crate::windows_input_backend;
use crate::{config, discovery, heatmap, recording, render};
use clap::Parser;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

mod picker;

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

    /// Set haptic intensity (0, 25, 50, 75, or 100 — firmware supports 5 discrete levels) and exit
    #[arg(long, value_name = "INTENSITY")]
    set_haptic_intensity: Option<u8>,

    /// Set click force / button-press threshold level (typically 1=light .. 3=firm) and exit
    #[arg(long, value_name = "LEVEL")]
    set_click_force: Option<u8>,

    /// Use a specific device instead of auto-detection (number or DEVICE column from --list, e.g. 1 or event8)
    #[arg(long)]
    device: Option<String>,

    /// Record touch session to a binary file
    #[arg(long, conflicts_with = "play")]
    record: Option<String>,

    /// Read touches from the touchpad's hidraw node through the PTP report
    /// parser instead of evdev (needs hidraw access, like the heatmap). For
    /// checking the parser against the kernel's interpretation; grabbing is
    /// not possible in this mode.
    #[cfg(target_os = "linux")]
    #[arg(long, conflicts_with = "play")]
    hidraw: bool,

    /// Print the touchpad's HID report descriptor and then N raw hidraw input
    /// reports as hex lines, and exit (for capturing parser test fixtures)
    #[cfg(target_os = "linux")]
    #[arg(long, value_name = "N", conflicts_with = "play")]
    dump_hidraw: Option<usize>,

    /// Print the PixArt pad's run/rest-mode registers (report rate, idle
    /// frame rates and delays, current mode) and exit
    #[arg(long, conflicts_with = "play")]
    rates: bool,

    /// Like --rates, then print a line whenever the pad's run/rest mode,
    /// observed report rate or 0x12/0x13 change, until interrupted
    #[arg(long, conflicts_with_all = ["play", "rates"])]
    watch_rates: bool,

    /// Write a PixArt user register (bank 0 unless given; hex or decimal,
    /// e.g. 0x12=120 or 1:0x05=0x10) and exit, or go on to --rates /
    /// --watch-rates. Repeatable. RAM only: the firmware's value is back
    /// once the pad resets or loses power
    #[arg(long, value_name = "[BANK:]REG=VALUE", value_parser = parse_user_reg_write, conflicts_with = "play")]
    write_user_reg: Vec<UserRegWrite>,

    /// Print the PixArt pad's power state (run/rest mode, what wakes it from
    /// a host SET_POWER sleep, the HID sleep scan rate) and exit
    #[arg(long, conflicts_with = "play")]
    power: bool,

    /// Choose what wakes the PixArt pad from the host's next SET_POWER
    /// sleep: touch (HID sleep, keeps scanning) or click (HID suspend, a
    /// click wakes it and is swallowed). tapview cannot send SET_POWER
    /// itself: the kernel does on system suspend for an I2C pad, the
    /// keyboard firmware for an external one. RAM only, like
    /// --write-user-reg
    #[arg(long, value_name = "WAKE", conflicts_with = "play")]
    sleep_wake: Option<SleepWakeArg>,

    /// Play back a recorded touch session (no device needed)
    #[arg(long, conflicts_with_all = ["record", "device", "libinput", "heatmap", "config"])]
    play: Option<String>,
}

/// Entry point of the native command-line binary.
pub fn main() {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    let trails = cli.trails.min(20);

    // --- Playback mode: no device needed ---
    if let Some(ref play_path) = cli.play {
        let rec = match recording::Recording::load(play_path) {
            Ok(r) => r,
            Err(e) => {
                log::error!("Failed to load recording: {}", e);
                std::process::exit(1);
            }
        };
        log::info!(
            "Loaded recording: {} frames, {:.1}s",
            rec.frames.len(),
            rec.duration_secs()
        );

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
            Box::new(move |_cc| Ok(Box::new(TapviewApp::new(trails).with_playback(rec)))),
        )
        .expect("Failed to run eframe");
        return;
    }

    // --- Normal / Recording mode: need a device ---

    let devices = match find_touchpads() {
        Ok(d) => d,
        Err(e) => {
            log::error!("Unable to find touchpad: {}", e);
            std::process::exit(1);
        }
    };

    if cli.list {
        print!("{}", discovery::format_device_table(&devices, cli.verbose));
        std::process::exit(0);
    }

    let power = PowerArgs {
        show: cli.power,
        sleep_wake: cli.sleep_wake,
    };
    let pixart = cli.rates || cli.watch_rates || !cli.write_user_reg.is_empty() || power.any();
    #[cfg(target_os = "linux")]
    let dump = cli.dump_hidraw.is_some();
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let dump = false;
    // The paths that print and exit have no window to pick a device in.
    let one_shot = cli.info
        || cli.set_haptic_intensity.is_some()
        || cli.set_click_force.is_some()
        || pixart
        || dump;

    let mut opts = OpenOptions {
        config: Feature::from_flags(cli.config, cli.no_config),
        heatmap: Feature::from_flags(cli.heatmap, cli.no_heatmap),
        heatmap_cols: cli.heatmap_cols,
        // No interpreted-input source on macOS yet.
        libinput: !cli.no_libinput && cfg!(not(target_os = "macos")),
        #[cfg(target_os = "linux")]
        hidraw: cli.hidraw,
        record: cli.record.clone(),
    };
    let title = if opts.record.is_some() {
        "Tapview - Touchpad Visualizer (Recording)"
    } else {
        "Tapview - Touchpad Visualizer"
    };
    // Without --device the window can switch pads, so it gets the picker
    // even when the one pad there is opens straight away.
    let picker = cli.device.is_none().then(|| devices.clone());

    let device = if let Some(ref wanted) = cli.device {
        match discovery::find_device(&devices, wanted) {
            Some(d) => d.clone(),
            None => {
                log::error!("Device {} not found among detected touchpads. Use --list to see available devices.", wanted);
                std::process::exit(1);
            }
        }
    } else if devices.len() == 1 {
        devices[0].clone()
    } else if !one_shot {
        // Several pads and a window to choose in: its picker asks.
        #[cfg(target_os = "windows")]
        let libinput_rx = opts
            .libinput
            .then(windows_input_backend::spawn_windows_input_thread);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let libinput_rx = None;
        run_window(
            trails,
            title,
            None,
            libinput_rx,
            picker.map(|devices| picker::Picker::new(devices, opts)),
        );
        return;
    } else if !std::io::stdin().is_terminal() {
        // No terminal to ask on: take the first (internal touchpads sort
        // first).
        devices[0].clone()
    } else {
        match discovery::prompt_for_device(&devices, cli.verbose) {
            Some(d) => d,
            None => {
                log::error!("No device selected.");
                std::process::exit(1);
            }
        }
    };
    log::info!("Found touchpad: {}", device);

    #[cfg(target_os = "linux")]
    if let Some(n) = cli.dump_hidraw {
        if let Err(e) = dump_hidraw(&device, n) {
            log::error!("dump-hidraw: {}", e);
            std::process::exit(1);
        }
        std::process::exit(0);
    }

    if pixart {
        let show = cli.rates || cli.watch_rates;
        if let Err(e) =
            pixart_registers(&device, &cli.write_user_reg, &power, show, cli.watch_rates)
        {
            log::error!("pixart: {}", e);
            std::process::exit(1);
        }
        std::process::exit(0);
    }

    // --info shows the PTP configuration even with --no-config
    let config = if cli.info { Feature::Auto } else { opts.config };
    let probe = match probe(&device, config) {
        Ok(p) => p,
        Err(e) => {
            log::error!("{}", e);
            std::process::exit(1);
        }
    };

    // --info: print device info and exit without launching UI
    if cli.info {
        let Probe {
            evdev_extents,
            hid_axes,
            ptp_config,
            axis_swap_detected,
        } = &probe;
        println!("Device");
        println!("  Path:             {}", device.devnode.display());
        println!("  Integration:      {:?}", device.integration);
        if let Some(vid) = device.vendor_id {
            println!("  Vendor ID:        {:04x}", vid);
        }
        if let Some(pid) = device.product_id {
            println!("  Product ID:       {:04x}", pid);
        }
        println!();

        if let Some((ex, ey)) = evdev_extents {
            println!("Evdev axes");
            println!("  X range:          0..{}", ex);
            println!("  Y range:          0..{}", ey);
            println!();
        }

        if let Some(phys) = hid_axes {
            println!("HID descriptor");
            println!(
                "  X logical:        {}..{}",
                phys.x.logical_min, phys.x.logical_max
            );
            println!(
                "  Y logical:        {}..{}",
                phys.y.logical_min, phys.y.logical_max
            );
            println!(
                "  X physical:       {}..{}",
                phys.x.physical_min, phys.x.physical_max
            );
            println!(
                "  Y physical:       {}..{}",
                phys.y.physical_min, phys.y.physical_max
            );
            println!(
                "  X size:           {:.1} mm ({:.1} units/mm)",
                phys.x.size_mm, phys.x.resolution
            );
            println!(
                "  Y size:           {:.1} mm ({:.1} units/mm)",
                phys.y.size_mm, phys.y.resolution
            );
            println!();
        }

        if let Some(cfg) = ptp_config {
            println!("PTP config");
            if let Some(mode) = cfg.state.input_mode {
                println!(
                    "  Input Mode:       {} ({})",
                    render::input_mode_label(mode),
                    mode
                );
            }
            if let Some(pt) = cfg.state.pad_type {
                println!(
                    "  Pad Type:         {} ({})",
                    render::pad_type_label(pt),
                    pt
                );
            }
            if let Some(max) = cfg.state.contact_count_max {
                println!("  Max Contacts:     {}", max);
            }
            if cfg.desc.features.has_surface_switch {
                println!(
                    "  Surface Switch:   {}",
                    cfg.state
                        .surface_switch
                        .map_or("n/a".to_string(), |v| v.to_string())
                );
            }
            if cfg.desc.features.has_button_switch {
                println!(
                    "  Button Switch:    {}",
                    cfg.state
                        .button_switch
                        .map_or("n/a".to_string(), |v| v.to_string())
                );
            }
            if let Some(lat) = cfg.state.latency_mode {
                println!("  Latency Mode:     {}", if lat { "low" } else { "normal" });
            }
            if let Some(thresh) = cfg.state.button_press_threshold {
                let range = cfg.desc.button_press_threshold_range.as_ref();
                let range_str = range
                    .map(|r| format!(" (range {}..{})", r.logical_min, r.logical_max))
                    .unwrap_or_default();
                let phys_str = range
                    .and_then(|r| r.physical)
                    .map(|(lo, hi)| format!(", physical {}..{} g", lo, hi))
                    .unwrap_or_default();
                println!("  Click Force:      {}{}{}", thresh, range_str, phys_str);
            }
            if cfg.desc.features.has_haptic_intensity {
                let range_str = cfg
                    .desc
                    .haptic_intensity_range
                    .as_ref()
                    .map(|r| format!(" (range {}..{})", r.logical_min, r.logical_max))
                    .unwrap_or_default();
                println!(
                    "  Haptic Intensity: {}{}",
                    cfg.state
                        .haptic_intensity
                        .map_or("n/a".to_string(), |v| v.to_string()),
                    range_str
                );
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

    // --- Set-and-exit flags: apply config changes and exit before launching UI ---
    if cli.set_haptic_intensity.is_some() || cli.set_click_force.is_some() {
        let mut cfg = match probe.ptp_config {
            Some(c) => c,
            None => {
                log::error!("config: device has no PTP/haptic configuration features");
                std::process::exit(1);
            }
        };

        if let Some(value) = cli.set_haptic_intensity {
            check_set_value(
                "haptic intensity",
                value,
                cfg.desc.features.has_haptic_intensity,
                cfg.desc.features.haptic_intensity_writable,
                cfg.desc.haptic_intensity_range.as_ref(),
            );
            if !matches!(value, 0 | 25 | 50 | 75 | 100) {
                log::error!(
                    "config: haptic intensity must be one of 0, 25, 50, 75, 100 (got {})",
                    value
                );
                std::process::exit(1);
            }
            if let Err(e) = pollster::block_on(cfg.backend.write_haptic_intensity(value)) {
                log::error!("config: failed to set haptic intensity: {}", e);
                std::process::exit(1);
            }
            println!("haptic intensity set to {}", value);
        }

        if let Some(value) = cli.set_click_force {
            check_set_value(
                "click force",
                value,
                cfg.desc.features.has_button_press_threshold,
                cfg.desc.features.button_press_threshold_writable,
                cfg.desc.button_press_threshold_range.as_ref(),
            );
            if let Err(e) = pollster::block_on(cfg.backend.write_button_press_threshold(value)) {
                log::error!("config: failed to set click force: {}", e);
                std::process::exit(1);
            }
            println!("click force set to {}", value);
        }
        std::process::exit(0);
    }

    let session = match start_session(&device, probe, &mut opts) {
        Ok(s) => s,
        Err(e) => {
            log::error!("{}", e);
            std::process::exit(1);
        }
    };
    #[cfg(not(target_os = "macos"))]
    let libinput_rx = opts.libinput.then(|| spawn_libinput(&device));
    #[cfg(target_os = "macos")]
    let libinput_rx = None;
    run_window(
        trails,
        title,
        Some(session),
        libinput_rx,
        picker.map(|devices| picker::Picker::new(devices, opts)),
    );
}

#[cfg(target_os = "linux")]
fn find_touchpads() -> Result<Vec<discovery::DeviceInfo>, discovery::DiscoveryError> {
    UdevDiscovery::find_touchpads()
}

#[cfg(target_os = "windows")]
fn find_touchpads() -> Result<Vec<discovery::DeviceInfo>, discovery::DiscoveryError> {
    WindowsDiscovery::find_touchpads()
}

#[cfg(target_os = "macos")]
fn find_touchpads() -> Result<Vec<discovery::DeviceInfo>, discovery::DiscoveryError> {
    crate::hid::macos::find_touchpads()
}

/// Whether a panel's backend runs: `--no-*` turns it off, `--*` makes a
/// failure to start it an error, and by default it runs where it can.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Feature {
    Off,
    Auto,
    Forced,
}

impl Feature {
    fn from_flags(force: bool, off: bool) -> Self {
        if off {
            Feature::Off
        } else if force {
            Feature::Forced
        } else {
            Feature::Auto
        }
    }
}

/// What the command line asked of the window's session; the picker opens
/// every pad it is asked to with the same.
struct OpenOptions {
    config: Feature,
    heatmap: Feature,
    heatmap_cols: Option<usize>,
    libinput: bool,
    #[cfg(target_os = "linux")]
    hidraw: bool,
    /// `--record`, taken by the first session opened: a pad picked later
    /// must not overwrite the recording.
    record: Option<String>,
}

/// What a touchpad says about itself, read before the window opens so that
/// `--info`, `--set-*` and the first UI frame all see the same.
struct Probe {
    /// Axis extents as evdev reports them (post-kernel-swap, matches actual
    /// event coordinates).
    evdev_extents: Option<(i32, i32)>,
    /// The axes as the HID descriptor declares them.
    hid_axes: Option<config::TouchpadPhysicalSize>,
    ptp_config: Option<Ptp>,
    axis_swap_detected: Option<bool>,
}

fn probe(device: &discovery::DeviceInfo, config: Feature) -> Result<Probe, String> {
    // Read evdev axis extents (post-kernel-swap, matches actual event coordinates)
    #[cfg(target_os = "linux")]
    let evdev_extents = crate::input::evdev_backend::read_axis_extents(&device.devnode);
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let evdev_extents = None;

    // Discover PTP configuration features (auto-detected by default, forced with --config)
    let ptp_config = if config == Feature::Off {
        None
    } else {
        let cfg = config::discover(&device.devnode).map(Ptp::initialize);
        if cfg.is_none() && config == Feature::Forced {
            return Err("config: no PTP configuration features found".to_string());
        }
        cfg
    };

    // The touchpad's axes as its HID descriptor declares them. On Linux and
    // macOS the config backend parsed the descriptor; Windows exposes none,
    // so the ranges come from the Touch Pad collection's preparsed data, and
    // the config panel is handed them for its Physical Size line.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let hid_axes = ptp_config
        .as_ref()
        .and_then(|cfg| cfg.desc.physical_size.clone());
    #[cfg(target_os = "windows")]
    let hid_axes = crate::input::windows_backend::read_axis_info(&device.devnode);
    #[cfg(target_os = "windows")]
    let ptp_config = ptp_config.map(|mut cfg| {
        if cfg.desc.physical_size.is_none() {
            cfg.desc.physical_size = hid_axes.clone();
        }
        cfg
    });

    // Log and compare axis ranges from both sources
    if let Some((ex, ey)) = &evdev_extents {
        log::info!("axis: evdev extents: x=0..{}, y=0..{}", ex, ey);
    }
    let axis_swap_detected = if let Some(phys) = &hid_axes {
        log::info!(
            "axis: HID descriptor: x={}..{}, y={}..{}",
            phys.x.logical_min,
            phys.x.logical_max,
            phys.y.logical_min,
            phys.y.logical_max
        );
        if let Some((ex, ey)) = &evdev_extents {
            if *ex != phys.x.logical_max || *ey != phys.y.logical_max {
                log::warn!("axis: evdev and HID descriptor disagree!");
                if *ex == phys.y.logical_max && *ey == phys.x.logical_max {
                    log::warn!("axis: looks like a kernel axis swap");
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
    };

    Ok(Probe {
        evdev_extents,
        hid_axes,
        ptp_config,
        axis_swap_detected,
    })
}

/// Open a probed touchpad for the window: the recorder if one is still to
/// be made, the input thread, the heatmap and the config worker.
fn start_session(
    device: &discovery::DeviceInfo,
    probe: Probe,
    opts: &mut OpenOptions,
) -> Result<Session, String> {
    // Resolve axis extents for recording: prefer evdev, fall back to the HID descriptor
    let record_extents = probe.evdev_extents.or_else(|| {
        probe
            .hid_axes
            .as_ref()
            .map(|phys| (phys.x.logical_max, phys.y.logical_max))
    });

    // Create recorder if --record was specified
    let recorder = if let Some(record_path) = opts.record.take() {
        let (ex, ey) = record_extents.unwrap_or((0, 0));
        let r = recording::Recorder::create(&record_path, ex, ey)
            .map_err(|e| format!("Failed to create recording file: {}", e))?;
        log::info!("Recording to: {}", record_path);
        Some(r)
    } else {
        None
    };

    // Create channels
    let (touch_tx, touch_rx) = mpsc::channel();
    let (grab_tx, grab_rx) = mpsc::channel::<GrabCommand>();

    // Spawn input thread. `session_extents` are the coordinates the touches
    // arrive in; `can_grab` whether the backend supports exclusive access.
    #[cfg(target_os = "linux")]
    let (session_extents, can_grab) = if opts.hidraw {
        let hidraw_path = heatmap::discovery::find_sibling_hidraw(&device.devnode)
            .map_err(|e| format!("hidraw: failed to find sibling hidraw device: {}", e))?;
        let backend = HidrawBackend::open(&hidraw_path).map_err(|e| format!("hidraw: {}", e))?;
        let (x_max, y_max) = backend.extents();
        log::info!(
            "hidraw: reading touches from {} (report {}, {} finger slots, up to {} contacts)",
            hidraw_path.display(),
            backend.layout().report_id,
            backend.layout().fingers.len(),
            backend.layout().contact_count_max
        );
        log::info!("axis: HID report extents: x=0..{}, y=0..{}", x_max, y_max);
        thread::spawn(move || run_input_thread(backend, grab_rx, touch_tx));
        (Some((x_max, y_max)), false)
    } else {
        let device_path = device.devnode.clone();
        thread::spawn(move || match EvdevBackend::open(&device_path) {
            Ok(backend) => run_input_thread(backend, grab_rx, touch_tx),
            Err(e) => log::error!("Failed to open device: {}", e),
        });
        (probe.evdev_extents, true)
    };

    #[cfg(target_os = "windows")]
    let (session_extents, can_grab) = {
        let device_path = device.devnode.clone();
        thread::spawn(move || match WindowsBackend::open(&device_path) {
            Ok(backend) => run_input_thread(backend, grab_rx, touch_tx),
            Err(e) => log::error!("Failed to open device: {}", e),
        });
        // RawInput hands over the reports in the descriptor's coordinates.
        // Windows doesn't support touchpad grab.
        let extents = probe
            .hid_axes
            .as_ref()
            .map(|phys| (phys.x.logical_max, phys.y.logical_max));
        if let Some((x_max, y_max)) = extents {
            log::info!("axis: HID report extents: x=0..{}, y=0..{}", x_max, y_max);
        }
        (extents, false)
    };

    #[cfg(target_os = "macos")]
    let (session_extents, can_grab) = {
        let backend = MacBackend::open(&device.devnode)
            .map_err(|e| format!("Failed to open device: {}", e))?;
        let (x_max, y_max) = backend.extents();
        log::info!(
            "hid: touch report {}, {} finger slots, up to {} contacts",
            backend.layout().report_id,
            backend.layout().fingers.len(),
            backend.layout().contact_count_max
        );
        log::info!("axis: HID report extents: x=0..{}, y=0..{}", x_max, y_max);
        thread::spawn(move || run_input_thread(backend, grab_rx, touch_tx));
        (Some((x_max, y_max)), false)
    };

    // The input thread ends when the grab channel closes. A backend that
    // cannot grab gets no grab UI, so the sender is only kept alive, for as
    // long as the session.
    let (grab_tx, guard) = if can_grab {
        (Some(grab_tx), None)
    } else {
        (None, Some(Box::new(grab_tx) as Box<dyn std::any::Any>))
    };

    // Spawn heatmap backend thread (auto-detected by default, forced with --heatmap)
    let heatmap = if opts.heatmap == Feature::Off {
        None
    } else {
        spawn_heatmap(device, opts.heatmap_cols, opts.heatmap == Feature::Forced)?
    };

    Ok(Session {
        name: device.label(),
        touch_rx: Some(touch_rx),
        grab_tx,
        heatmap,
        config: probe.ptp_config.map(Ptp::into_handle),
        extents: session_extents,
        recorder,
        lost: None,
        guard,
    })
}

/// Probe and open a touchpad picked in the window.
fn open_for_window(
    device: &discovery::DeviceInfo,
    opts: &mut OpenOptions,
) -> Result<Session, String> {
    log::info!("Opening touchpad: {}", device);
    let probe = probe(device, opts.config)?;
    start_session(device, probe, opts)
}

/// The pointer panel's events: libinput's interpretation of the device on
/// Linux (one context per device, so it follows the session); Windows' mouse
/// hook sees every pointer, so this runs once per window.
#[cfg(target_os = "linux")]
fn spawn_libinput(device: &discovery::DeviceInfo) -> mpsc::Receiver<LibinputEvent> {
    libinput_backend::spawn_libinput_thread(&device.devnode)
}

#[cfg(target_os = "windows")]
fn spawn_libinput(_device: &discovery::DeviceInfo) -> mpsc::Receiver<LibinputEvent> {
    windows_input_backend::spawn_windows_input_thread()
}

/// A window size that fits what is on screen: the pointer panel and the
/// config panel widen it, the heatmap makes it taller.
fn window_size(session: Option<&Session>, libinput: bool) -> egui::Vec2 {
    let mut width = if libinput { 1100.0 } else { 672.0 };
    if session.is_some_and(|s| s.config.is_some()) {
        width += 220.0;
    }
    let height = if session.is_some_and(|s| s.heatmap.is_some()) {
        650.0
    } else {
        432.0
    };
    egui::vec2(width, height)
}

/// Run eframe on a session, a picker, or both.
fn run_window(
    trails: usize,
    title: &str,
    session: Option<Session>,
    libinput_rx: Option<mpsc::Receiver<LibinputEvent>>,
    picker: Option<picker::Picker>,
) {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(window_size(session.as_ref(), libinput_rx.is_some()))
            .with_min_inner_size([320.0, 240.0])
            .with_title(title)
            .with_always_on_top(),
        ..Default::default()
    };

    eframe::run_native(
        "Tapview",
        options,
        Box::new(move |_cc| {
            let mut app = TapviewApp::new(trails).with_libinput(libinput_rx);
            if let Some(session) = session {
                app = app.with_session(session);
            }
            if let Some(picker) = picker {
                app = picker.install(app);
            }
            Ok(Box::new(app))
        }),
    )
    .expect("Failed to run eframe");
}

/// The input thread body: poll the backend for touch frames, forward them to
/// the UI, and act on grab/ungrab requests in between.
fn run_input_thread<B: InputBackend>(
    mut backend: B,
    grab_rx: mpsc::Receiver<GrabCommand>,
    touch_tx: mpsc::Sender<crate::input::TouchState>,
) {
    loop {
        // Check for grab/ungrab commands
        match grab_rx.try_recv() {
            Ok(GrabCommand::Grab) => {
                if let Err(e) = backend.grab() {
                    log::error!("Grab failed: {}", e);
                }
            }
            Ok(GrabCommand::Ungrab) => {
                if let Err(e) = backend.ungrab() {
                    log::error!("Ungrab failed: {}", e);
                }
            }
            Err(mpsc::TryRecvError::Empty) => {}
            // The session is gone (switched away from): while the pad is
            // idle the touch channel would never tell.
            Err(mpsc::TryRecvError::Disconnected) => break,
        }

        match backend.poll_events() {
            Ok(Some(state)) => {
                if touch_tx.send(state).is_err() {
                    // UI gone
                    break;
                }
            }
            Ok(None) => {
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => {
                log::error!("Input error: {}", e);
                break;
            }
        }
    }
}

/// `--dump-hidraw N`: the report descriptor, then N raw input reports, as
/// hex lines on stdout. Redirect into a file to capture a parser fixture.
#[cfg(target_os = "linux")]
fn dump_hidraw(device: &discovery::DeviceInfo, count: usize) -> Result<(), String> {
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }
    let hidraw_path = heatmap::discovery::find_sibling_hidraw(&device.devnode)
        .map_err(|e| format!("failed to find sibling hidraw device: {}", e))?;
    let desc = crate::hid::linux::read_report_descriptor(&hidraw_path)
        .map_err(|e| format!("failed to read report descriptor: {}", e))?;
    let mut backend = HidrawBackend::open(&hidraw_path).map_err(|e| e.to_string())?;
    log::info!(
        "dump-hidraw: {} reports from {} (touch it now)",
        count,
        hidraw_path.display()
    );
    println!("# descriptor {}", hex(&desc));
    let mut dumped = 0;
    while dumped < count {
        if let Some(n) = backend.read_raw(1000).map_err(|e| e.to_string())? {
            println!("{}", hex(&backend.buffer()[..n]));
            dumped += 1;
        }
    }
    Ok(())
}

/// How often `--watch-rates` polls the mode and observed report rate.
const WATCH_RATES_PERIOD: Duration = Duration::from_millis(100);

/// The pad's PixArt vendor HID interface: the sibling hidraw node on Linux,
/// the heatmap-capable HID interface on Windows, the device itself on macOS.
#[cfg(target_os = "linux")]
fn pixart_hid_path(device: &discovery::DeviceInfo) -> std::io::Result<std::path::PathBuf> {
    heatmap::discovery::find_sibling_hidraw(&device.devnode)
}

#[cfg(target_os = "windows")]
fn pixart_hid_path(device: &discovery::DeviceInfo) -> std::io::Result<std::path::PathBuf> {
    heatmap::discovery::find_hid_device_for_heatmap(&device.devnode).map(|(path, _)| path)
}

#[cfg(target_os = "macos")]
fn pixart_hid_path(device: &discovery::DeviceInfo) -> std::io::Result<std::path::PathBuf> {
    Ok(device.devnode.clone())
}

/// One `--write-user-reg [BANK:]REG=VALUE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct UserRegWrite {
    bank: u8,
    reg: u8,
    value: u8,
}

fn parse_user_reg_write(s: &str) -> Result<UserRegWrite, String> {
    fn byte(what: &str, s: &str) -> Result<u8, String> {
        let s = s.trim();
        let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            Some(hex) => u8::from_str_radix(hex, 16),
            None => s.parse(),
        };
        parsed.map_err(|_| format!("{} {:?} is not a byte (0-255 or 0x00-0xff)", what, s))
    }
    let (target, value) = s
        .split_once('=')
        .ok_or_else(|| format!("{:?} is not [BANK:]REG=VALUE", s))?;
    let (bank, reg) = match target.split_once(':') {
        Some((bank, reg)) => (byte("bank", bank)?, byte("register", reg)?),
        None => (0, byte("register", target)?),
    };
    Ok(UserRegWrite {
        bank,
        reg,
        value: byte("value", value)?,
    })
}

/// `--sleep-wake`.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum SleepWakeArg {
    Touch,
    Click,
}

impl From<SleepWakeArg> for heatmap::power::SleepWake {
    fn from(a: SleepWakeArg) -> Self {
        match a {
            SleepWakeArg::Touch => heatmap::power::SleepWake::Touch,
            SleepWakeArg::Click => heatmap::power::SleepWake::Click,
        }
    }
}

/// `--power` and `--sleep-wake`.
struct PowerArgs {
    show: bool,
    sleep_wake: Option<SleepWakeArg>,
}

impl PowerArgs {
    fn any(&self) -> bool {
        self.show || self.sleep_wake.is_some()
    }
}

/// Print the power registers of a PixArt pad.
fn print_power(status: &heatmap::power::PowerStatus, chip: heatmap::chips::ChipVariant) {
    use heatmap::rates::op_mode_label;
    println!("{} power state (user bank 0)", chip);
    println!(
        "  0x15 {:<24} {} ({})",
        "OBS_OP_MODE",
        op_mode_label(status.op_mode),
        status.op_mode
    );
    match status.sleep_wake {
        Some(wake) => println!("       {:<24} {}", "SET_POWER sleep wakes on", wake),
        None => println!(
            "       {:<24} unknown (the {}'s suspend flag is not known)",
            "SET_POWER sleep wakes on", chip
        ),
    }
    if let Some(rate) = status.hid_sleep_rate {
        println!("       {:<24} {} Hz", "HID_SLEEP_REPORT_RATE", rate);
    }
}

/// `--write-user-reg`, `--sleep-wake`, `--power`,
/// `--rates` and `--watch-rates` on a PixArt pad: apply the writes, then
/// show the power registers if asked, the run/rest-mode registers if
/// `show`, then follow them if `watch`. Every register access is a
/// SetFeature + GetFeature pair, so don't run this alongside another tapview
/// that polls the heatmap of the same pad.
fn pixart_registers(
    device: &discovery::DeviceInfo,
    writes: &[UserRegWrite],
    power: &PowerArgs,
    show: bool,
    watch: bool,
) -> Result<(), String> {
    use heatmap::power as pw;
    use heatmap::protocol::{read_user_reg, write_user_reg};
    use heatmap::rates;

    let path =
        pixart_hid_path(device).map_err(|e| format!("no PixArt vendor HID interface: {}", e))?;
    let dev = heatmap::open_platform_hid_device(&path)
        .map_err(|e| format!("{}: {}", path.display(), e))?;
    // Also the check that this is a PixArt pad at all before writing to it.
    let chip = pollster::block_on(heatmap::chips::identify_chip(&dev))
        .map_err(|e| format!("identifying the chip: {}", e))?;

    for w in writes {
        let old = pollster::block_on(read_user_reg(&dev, w.bank, w.reg));
        pollster::block_on(write_user_reg(&dev, w.bank, w.reg, w.value))
            .map_err(|e| format!("writing bank {} 0x{:02x}: {}", w.bank, w.reg, e))?;
        let now = pollster::block_on(read_user_reg(&dev, w.bank, w.reg))
            .map_err(|e| format!("reading back bank {} 0x{:02x}: {}", w.bank, w.reg, e))?;
        let old = old.map_or("?".to_string(), |v| v.to_string());
        println!(
            "{} user bank {} 0x{:02x}: {} -> {}",
            chip, w.bank, w.reg, old, now
        );
        if now != w.value {
            log::warn!(
                "pixart: bank {} 0x{:02x} reads back {}, not the {} written (read-only, or the firmware changed it)",
                w.bank,
                w.reg,
                now,
                w.value
            );
        }
    }
    if let Some(wake) = power.sleep_wake {
        let wake = wake.into();
        pollster::block_on(pw::set_sleep_wake(&dev, chip, wake))
            .map_err(|e| format!("sleep wake: {}", e))?;
        println!("{} SET_POWER sleep now wakes on {}", chip, wake);
    }
    let wrote = !writes.is_empty() || power.sleep_wake.is_some();
    if power.show {
        if wrote {
            println!();
        }
        let status = pollster::block_on(pw::read_power(&dev, chip)).map_err(|e| e.to_string())?;
        print_power(&status, chip);
    }
    if !show {
        return Ok(());
    }
    if wrote || power.show {
        println!();
    }

    let regs = pollster::block_on(rates::read_rates(&dev, chip)).map_err(|e| e.to_string())?;
    println!(
        "{} run/rest registers (user bank 0, {})",
        chip,
        path.display()
    );
    for r in &regs {
        println!("  0x{:02x} {:<22} {}", r.addr, r.name, r.value);
    }
    if !watch {
        return Ok(());
    }

    println!();
    println!("Watching for changes, Ctrl-C to stop");
    println!(
        "  {:>8}    {:<5}  {:>8}  {:>4}  {:>4}",
        "time", "mode", "observed", "0x12", "0x13"
    );
    let start = std::time::Instant::now();
    let mut last = None;
    loop {
        let status = pollster::block_on(rates::read_status(&dev)).map_err(|e| e.to_string())?;
        if last != Some(status) {
            println!(
                "  {:8.2} s  {:<5}  {:>5} Hz  {:>4}  {:>4}",
                start.elapsed().as_secs_f32(),
                rates::op_mode_label(status.op_mode),
                status.obs_report_rate,
                status.report_rate_reg[0],
                status.report_rate_reg[1]
            );
            last = Some(status);
        }
        thread::sleep(WATCH_RATES_PERIOD);
    }
}

/// A discovered PTP configuration device, initialised on the main thread so
/// that `--info`, `--set-*` and the first UI frame all see the probed state.
struct Ptp {
    desc: ConfigDescription,
    state: ConfigState,
    backend: PlatformConfigBackend,
}

impl Ptp {
    fn initialize(d: config::Discovered) -> Ptp {
        let mut desc = d.description;
        let mut backend = d.backend;
        let state = pollster::block_on(config::initialize(&mut backend, &mut desc));
        Ptp {
            desc,
            state,
            backend,
        }
    }

    /// Move the backend onto its worker thread for the UI.
    fn into_handle(self) -> config::ConfigHandle {
        config::ConfigHandle::spawn_thread(self.desc, self.state, self.backend)
    }
}

/// Route `log` output to stderr as plain lines, the way the old `eprintln!`
/// diagnostics looked. `RUST_LOG` takes precedence when set; otherwise our
/// own crate logs at info, or at debug with `--verbose` (which is where the
/// raw evdev event dump lives).
fn init_logging(verbose: bool) {
    use std::io::Write;
    let mut builder = env_logger::Builder::new();
    builder.format(|buf, record| writeln!(buf, "{}", record.args()));
    if std::env::var_os("RUST_LOG").is_some() {
        builder.parse_default_env();
    } else {
        let level = if verbose {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        };
        builder.filter_module("tapview", level);
    }
    builder.init();
}

/// Validate a CLI-provided value against a feature's presence/writability/range.
/// Exits the process with a clear error message on any check failure.
fn check_set_value(
    label: &str,
    value: u8,
    has_feature: bool,
    writable: bool,
    range: Option<&config::ValueRange>,
) {
    if !has_feature {
        log::error!("config: device does not expose {}", label);
        std::process::exit(1);
    }
    if !writable {
        log::error!("config: {} is read-only on this device", label);
        std::process::exit(1);
    }
    if let Some(r) = range {
        let v = value as i32;
        if v < r.logical_min || v > r.logical_max {
            log::error!(
                "config: {} value {} out of range ({}..={})",
                label,
                value,
                r.logical_min,
                r.logical_max
            );
            std::process::exit(1);
        }
    }
}

/// Start the heatmap where the pad has one; with `force`, not finding it is
/// an error rather than a session without a heatmap.
#[cfg(target_os = "linux")]
fn spawn_heatmap(
    device: &discovery::DeviceInfo,
    heatmap_cols: Option<usize>,
    force: bool,
) -> Result<Option<heatmap::backend::HeatmapStream>, String> {
    match heatmap::discovery::find_sibling_hidraw(&device.devnode) {
        Ok(hidraw_path) => {
            log::info!("heatmap: found hidraw device: {}", hidraw_path.display());
            match heatmap::discovery::determine_burst_report_length(&hidraw_path) {
                Ok(burst_len) => {
                    log::info!("heatmap: burst report length = {}", burst_len);
                    Ok(Some(heatmap::backend::spawn_heatmap_thread(
                        &hidraw_path,
                        burst_len,
                        heatmap_cols,
                    )))
                }
                Err(e) if force => Err(format!("heatmap: failed to determine burst length: {}", e)),
                Err(_) => Ok(None),
            }
        }
        Err(e) if force => Err(format!(
            "heatmap: failed to find sibling hidraw device: {}",
            e
        )),
        Err(_) => Ok(None),
    }
}

#[cfg(target_os = "windows")]
fn spawn_heatmap(
    device: &discovery::DeviceInfo,
    heatmap_cols: Option<usize>,
    force: bool,
) -> Result<Option<heatmap::backend::HeatmapStream>, String> {
    match heatmap::discovery::find_hid_device_for_heatmap(&device.devnode) {
        Ok((hid_path, burst_len)) => {
            log::info!(
                "heatmap: found HID device: {}, burst_len={}",
                hid_path.display(),
                burst_len
            );
            Ok(Some(heatmap::backend::spawn_heatmap_thread(
                &hid_path,
                burst_len,
                heatmap_cols,
            )))
        }
        Err(e) if force => Err(format!("heatmap: {}", e)),
        Err(_) => Ok(None),
    }
}

#[cfg(target_os = "macos")]
fn spawn_heatmap(
    device: &discovery::DeviceInfo,
    heatmap_cols: Option<usize>,
    force: bool,
) -> Result<Option<heatmap::backend::HeatmapStream>, String> {
    let burst_len = crate::hid::macos::MacHidDevice::open(&device.devnode)
        .and_then(|d| d.report_descriptor())
        .and_then(|desc| {
            heatmap::discovery::burst_report_length(&crate::hid::ReportLayout::parse(&desc))
        });
    match burst_len {
        Ok(burst_len) => {
            log::info!("heatmap: burst report length = {}", burst_len);
            Ok(Some(heatmap::backend::spawn_heatmap_thread(
                &device.devnode,
                burst_len,
                heatmap_cols,
            )))
        }
        Err(e) if force => Err(format!("heatmap: {}", e)),
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(bank: u8, reg: u8, value: u8) -> UserRegWrite {
        UserRegWrite { bank, reg, value }
    }

    #[test]
    fn user_reg_write_defaults_to_bank_0_and_takes_hex_or_decimal() {
        assert_eq!(parse_user_reg_write("0x12=120"), Ok(w(0, 0x12, 120)));
        assert_eq!(parse_user_reg_write("18=0x78"), Ok(w(0, 18, 0x78)));
        assert_eq!(parse_user_reg_write("1:0X05=0xFF"), Ok(w(1, 5, 0xff)));
        assert_eq!(parse_user_reg_write(" 0:0x1d = 10 "), Ok(w(0, 0x1d, 10)));
    }

    #[test]
    fn user_reg_write_rejects_malformed_and_out_of_range() {
        assert!(parse_user_reg_write("0x12").is_err());
        assert!(parse_user_reg_write("0x12=256").is_err());
        assert!(parse_user_reg_write("0x100=1").is_err());
        assert!(parse_user_reg_write("x:0x12=1").is_err());
        assert!(parse_user_reg_write("0x12=").is_err());
    }
}
