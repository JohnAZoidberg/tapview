//! The native command-line interface: argument parsing, device selection,
//! the `--info`/`--set-*` one-shot paths, and assembling the input, heatmap
//! and config threads before handing over to the egui UI.

use std::io::IsTerminal;

use crate::app::TapviewApp;
use crate::config::{ConfigBackend, ConfigDescription, ConfigState, PlatformConfigBackend};
#[cfg(target_os = "linux")]
use crate::discovery::udev_discovery::UdevDiscovery;
#[cfg(target_os = "windows")]
use crate::discovery::windows_discovery::WindowsDiscovery;
use crate::discovery::DeviceDiscovery;
#[cfg(target_os = "linux")]
use crate::input::evdev_backend::EvdevBackend;
#[cfg(target_os = "linux")]
use crate::input::hidraw_backend::HidrawBackend;
#[cfg(target_os = "windows")]
use crate::input::windows_backend::WindowsBackend;
use crate::input::InputBackend;
#[cfg(target_os = "linux")]
use crate::libinput_backend;
use crate::session::{GrabCommand, Session};
#[cfg(target_os = "windows")]
use crate::windows_input_backend;
use crate::{config, discovery, heatmap, recording, render};
use clap::Parser;
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

    // Discover touchpad
    #[cfg(target_os = "linux")]
    let devices = UdevDiscovery::find_touchpads();
    #[cfg(target_os = "windows")]
    let devices = WindowsDiscovery::find_touchpads();

    let devices = match devices {
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

    let device = if let Some(ref wanted) = cli.device {
        match discovery::find_device(&devices, wanted) {
            Some(d) => d.clone(),
            None => {
                log::error!("Device {} not found among detected touchpads. Use --list to see available devices.", wanted);
                std::process::exit(1);
            }
        }
    } else if devices.len() == 1 || !std::io::stdin().is_terminal() {
        // Single device, or no terminal to ask on (e.g. launched from a
        // desktop menu): take the first (internal touchpads sort first).
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

    // Read evdev axis extents (post-kernel-swap, matches actual event coordinates)
    #[cfg(target_os = "linux")]
    let evdev_extents = crate::input::evdev_backend::read_axis_extents(&device.devnode);
    #[cfg(target_os = "windows")]
    let evdev_extents = None;

    // Discover PTP configuration features (auto-detected by default, forced with --config)
    let ptp_config = if cli.no_config && !cli.info {
        None
    } else {
        let cfg = config::discover(&device.devnode).map(Ptp::initialize);
        if cfg.is_none() && cli.config {
            log::error!("config: no PTP configuration features found");
            std::process::exit(1);
        }
        cfg
    };

    // Log and compare axis ranges from both sources
    if let Some((ex, ey)) = &evdev_extents {
        log::info!("axis: evdev extents: x=0..{}, y=0..{}", ex, ey);
    }
    let axis_swap_detected = if let Some(cfg) = &ptp_config {
        if let Some(phys) = &cfg.desc.physical_size {
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
        }
    } else {
        None
    };

    // --info: print device info and exit without launching UI
    if cli.info {
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

        if let Some((ex, ey)) = &evdev_extents {
            println!("Evdev axes");
            println!("  X range:          0..{}", ex);
            println!("  Y range:          0..{}", ey);
            println!();
        }

        if let Some(cfg) = &ptp_config {
            if let Some(phys) = &cfg.desc.physical_size {
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
        let mut cfg = match ptp_config {
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

    // Create recorder if --record was specified
    // Resolve axis extents for recording: prefer evdev, fall back to PTP logical extents
    let record_extents = evdev_extents.or_else(|| {
        ptp_config.as_ref().and_then(|cfg| {
            cfg.desc
                .physical_size
                .as_ref()
                .map(|phys| (phys.x.logical_max, phys.y.logical_max))
        })
    });

    // Create recorder if --record was specified
    let recorder = if let Some(ref record_path) = cli.record {
        let (ex, ey) = record_extents.unwrap_or((0, 0));
        match recording::Recorder::create(record_path, ex, ey) {
            Ok(r) => {
                log::info!("Recording to: {}", record_path);
                Some(r)
            }
            Err(e) => {
                log::error!("Failed to create recording file: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    // Create channels
    let (touch_tx, touch_rx) = mpsc::channel();
    let (grab_tx, grab_rx) = mpsc::channel::<GrabCommand>();

    // Spawn input thread. `session_extents` are the coordinates the touches
    // arrive in; `can_grab` whether the backend supports exclusive access.
    #[cfg(target_os = "linux")]
    let (session_extents, can_grab) = if cli.hidraw {
        let hidraw_path = match heatmap::discovery::find_sibling_hidraw(&device.devnode) {
            Ok(p) => p,
            Err(e) => {
                log::error!("hidraw: failed to find sibling hidraw device: {}", e);
                std::process::exit(1);
            }
        };
        let backend = match HidrawBackend::open(&hidraw_path) {
            Ok(b) => b,
            Err(e) => {
                log::error!("hidraw: {}", e);
                std::process::exit(1);
            }
        };
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
        (evdev_extents, true)
    };

    #[cfg(target_os = "windows")]
    let (session_extents, can_grab) = {
        let device_path = device.devnode.clone();
        thread::spawn(move || match WindowsBackend::open(&device_path) {
            Ok(backend) => run_input_thread(backend, grab_rx, touch_tx),
            Err(e) => log::error!("Failed to open device: {}", e),
        });
        // Windows doesn't support touchpad grab
        (evdev_extents, false)
    };

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
    let heatmap = if cli.no_heatmap {
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
    let initial_height = if heatmap.is_some() { 650.0 } else { 432.0 };
    let config_handle = ptp_config.map(Ptp::into_handle);
    let session = Session {
        name: device.label(),
        touch_rx,
        grab_tx: can_grab.then_some(grab_tx),
        heatmap,
        config: config_handle,
        extents: session_extents,
        recorder,
        lost: None,
        guard: None,
    };
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
            Ok(Box::new(
                TapviewApp::new(trails)
                    .with_session(session)
                    .with_libinput(libinput_rx),
            ))
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
        if let Ok(cmd) = grab_rx.try_recv() {
            match cmd {
                GrabCommand::Grab => {
                    if let Err(e) = backend.grab() {
                        log::error!("Grab failed: {}", e);
                    }
                }
                GrabCommand::Ungrab => {
                    if let Err(e) = backend.ungrab() {
                        log::error!("Ungrab failed: {}", e);
                    }
                }
            }
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

#[cfg(target_os = "linux")]
fn spawn_heatmap(
    device: &discovery::DeviceInfo,
    heatmap_cols: Option<usize>,
    force: bool,
) -> Option<heatmap::backend::HeatmapStream> {
    match heatmap::discovery::find_sibling_hidraw(&device.devnode) {
        Ok(hidraw_path) => {
            log::info!("heatmap: found hidraw device: {}", hidraw_path.display());
            match heatmap::discovery::determine_burst_report_length(&hidraw_path) {
                Ok(burst_len) => {
                    log::info!("heatmap: burst report length = {}", burst_len);
                    Some(heatmap::backend::spawn_heatmap_thread(
                        &hidraw_path,
                        burst_len,
                        heatmap_cols,
                    ))
                }
                Err(e) => {
                    if force {
                        log::error!("heatmap: failed to determine burst length: {}", e);
                        std::process::exit(1);
                    }
                    None
                }
            }
        }
        Err(e) => {
            if force {
                log::error!("heatmap: failed to find sibling hidraw device: {}", e);
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
) -> Option<heatmap::backend::HeatmapStream> {
    match heatmap::discovery::find_hid_device_for_heatmap(&device.devnode) {
        Ok((hid_path, burst_len)) => {
            log::info!(
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
                log::error!("heatmap: {}", e);
                std::process::exit(1);
            }
            None
        }
    }
}
