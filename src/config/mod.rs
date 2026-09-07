//! PTP (Precision Touchpad) configuration: the Feature-report fields a pad
//! exposes (input mode, selective reporting, latency mode, click force,
//! haptic intensity) and how the UI drives them.
//!
//! Device I/O never happens on the UI thread. [`ConfigHandle`] is the UI's
//! side: it caches what the panel shows, applies writes optimistically and
//! reverts them if the worker reports failure. [`run_config_worker`] owns the
//! [`ConfigBackend`] (async, like [`crate::heatmap::HidDevice`]) and services
//! commands over channels — on native from a thread under
//! `pollster::block_on`, in the browser from a `spawn_local` task.

pub mod layout_backend;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "windows")]
mod windows;

use std::collections::VecDeque;
use std::io;
use std::sync::mpsc;

/// Per-axis range and physical size from the HID descriptor.
#[derive(Clone, Debug)]
pub struct AxisPhysicalInfo {
    pub logical_min: i32,
    pub logical_max: i32,
    pub physical_min: i32,
    pub physical_max: i32,
    pub size_mm: f64,
    pub resolution: f64, // logical units per mm
}

/// Physical dimensions of the touchpad surface, extracted from the HID descriptor.
#[derive(Clone, Debug)]
pub struct TouchpadPhysicalSize {
    pub x: AxisPhysicalInfo,
    pub y: AxisPhysicalInfo,
}

/// Which PTP configuration features the device supports.
#[derive(Clone, Copy, Debug, Default)]
pub struct PtpFeatures {
    pub has_input_mode: bool,
    pub has_surface_switch: bool,
    pub has_button_switch: bool,
    pub has_contact_count_max: bool,
    pub has_pad_type: bool,
    pub has_latency_mode: bool,
    pub has_button_press_threshold: bool,
    pub has_haptic_intensity: bool,
    // Writable flags — false when the descriptor marks the field as Constant,
    // or when a probe write at startup was rejected by the kernel driver.
    pub input_mode_writable: bool,
    pub surface_switch_writable: bool,
    pub button_switch_writable: bool,
    pub latency_mode_writable: bool,
    pub button_press_threshold_writable: bool,
    pub haptic_intensity_writable: bool,
}

/// Snapshot of current PTP configuration values as read from the device.
/// Excludes write-only fields (click force, haptic intensity) — those are seeded
/// at startup and only updated when the user writes via the UI/CLI.
#[derive(Clone, Copy, Debug, Default)]
pub struct ConfigValues {
    pub input_mode: Option<u8>,
    pub surface_switch: Option<bool>,
    pub button_switch: Option<bool>,
    pub contact_count_max: Option<u8>,
    pub pad_type: Option<u8>,
    pub latency_mode: Option<bool>,
}

/// Logical (and optional physical) range for a numeric feature field,
/// extracted from the HID descriptor. Used so sliders can show the
/// actual valid range instead of guessing.
#[derive(Clone, Copy, Debug)]
pub struct ValueRange {
    pub logical_min: i32,
    pub logical_max: i32,
    /// Physical min/max in the unit declared by the descriptor (e.g. grams
    /// for the click-force threshold). Only set when the descriptor declared
    /// a non-empty physical range distinct from the logical one.
    pub physical: Option<(i32, i32)>,
}

/// Platform-specific backend for reading/writing PTP feature reports.
///
/// Async for the same reason as [`crate::heatmap::HidDevice`]: the browser
/// transport cannot block. Native backends complete immediately.
#[allow(async_fn_in_trait)]
pub trait ConfigBackend {
    async fn read_all(&mut self) -> ConfigValues;
    async fn write_input_mode(&mut self, value: u8) -> io::Result<()>;
    async fn write_selective_reporting(&mut self, surface: bool, button: bool) -> io::Result<()>;
    async fn write_latency_mode(&mut self, high: bool) -> io::Result<()>;
    async fn write_button_press_threshold(&mut self, value: u8) -> io::Result<()>;
    async fn write_haptic_intensity(&mut self, value: u8) -> io::Result<()>;
}

/// The native config backend of this platform.
#[cfg(target_os = "linux")]
pub type PlatformConfigBackend =
    layout_backend::LayoutConfigBackend<crate::heatmap::hidraw::HidrawDevice>;
#[cfg(target_os = "windows")]
pub type PlatformConfigBackend = windows::WindowsConfigBackend;

/// What the descriptor says about the device: which features exist, their
/// ranges, and the pad's physical size. Known before any device I/O.
#[derive(Clone, Debug)]
pub struct ConfigDescription {
    pub features: PtpFeatures,
    pub button_press_threshold_range: Option<ValueRange>,
    pub haptic_intensity_range: Option<ValueRange>,
    pub physical_size: Option<TouchpadPhysicalSize>,
}

/// The values the panel shows: what was read from the device plus the
/// write-only fields' assumed values.
#[derive(Clone, Copy, Debug, Default)]
pub struct ConfigState {
    pub input_mode: Option<u8>,
    pub surface_switch: Option<bool>,
    pub button_switch: Option<bool>,
    pub contact_count_max: Option<u8>,
    pub pad_type: Option<u8>,
    pub latency_mode: Option<bool>,
    pub button_press_threshold: Option<u8>,
    pub haptic_intensity: Option<u8>,
}

impl ConfigState {
    fn apply_values(&mut self, v: ConfigValues) {
        self.input_mode = v.input_mode;
        self.surface_switch = v.surface_switch;
        self.button_switch = v.button_switch;
        self.contact_count_max = v.contact_count_max;
        self.pad_type = v.pad_type;
        self.latency_mode = v.latency_mode;
        // button_press_threshold and haptic_intensity are write-only on the
        // firmware (read returns garbage), so a refresh leaves them alone.
    }
}

/// A device found by [`discover`]: its description plus an opened backend
/// that has not been touched yet.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub struct Discovered {
    pub description: ConfigDescription,
    pub backend: PlatformConfigBackend,
}

/// Find the PTP configuration interface belonging to a touchpad and open it.
/// Does no reads or writes; follow with [`initialize`].
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub fn discover(device_path: &std::path::Path) -> Option<Discovered> {
    #[cfg(target_os = "linux")]
    {
        linux::discover(device_path)
    }
    #[cfg(target_os = "windows")]
    {
        windows::discover(device_path)
    }
}

/// First contact with the device: read the current values, seed the
/// write-only fields with their startup defaults, and probe which fields
/// are actually writable (updating `description.features`).
pub async fn initialize<B: ConfigBackend>(
    backend: &mut B,
    description: &mut ConfigDescription,
) -> ConfigState {
    let mut state = ConfigState::default();
    state.apply_values(backend.read_all().await);
    // Click force / haptic intensity are write-only on this firmware; seed startup defaults.
    state.button_press_threshold = description.features.has_button_press_threshold.then_some(2);
    state.haptic_intensity = description.features.has_haptic_intensity.then_some(50);
    probe_writable(backend, &mut description.features, &state).await;
    state
}

/// Probe which fields are actually writable by attempting no-op writes.
/// Disables writable flags for fields the kernel rejects.
///
/// On Linux, writes can fail (EINVAL) when the heatmap module also has
/// the same hidraw device open.  The kernel's hid-multitouch driver
/// manages latency mode automatically (low on open, high on close),
/// so losing write access here is harmless.
pub async fn probe_writable<B: ConfigBackend>(
    backend: &mut B,
    features: &mut PtpFeatures,
    state: &ConfigState,
) {
    if features.input_mode_writable {
        if let Some(v) = state.input_mode {
            if backend.write_input_mode(v).await.is_err() {
                features.input_mode_writable = false;
            }
        }
    }
    if features.surface_switch_writable || features.button_switch_writable {
        let s = state.surface_switch.unwrap_or(true);
        let b = state.button_switch.unwrap_or(true);
        if backend.write_selective_reporting(s, b).await.is_err() {
            features.surface_switch_writable = false;
            features.button_switch_writable = false;
        }
    }
    if features.latency_mode_writable {
        if let Some(v) = state.latency_mode {
            if backend.write_latency_mode(v).await.is_err() {
                features.latency_mode_writable = false;
            }
        }
    }
    if features.button_press_threshold_writable {
        if let Some(v) = state.button_press_threshold {
            if backend.write_button_press_threshold(v).await.is_err() {
                features.button_press_threshold_writable = false;
            }
        }
    }
    if features.haptic_intensity_writable {
        if let Some(v) = state.haptic_intensity {
            if backend.write_haptic_intensity(v).await.is_err() {
                features.haptic_intensity_writable = false;
            }
        }
    }
}

/// One configuration write, as the UI requests it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigWrite {
    InputMode(u8),
    SelectiveReporting { surface: bool, button: bool },
    LatencyMode(bool),
    ButtonPressThreshold(u8),
    HapticIntensity(u8),
}

impl ConfigWrite {
    /// Human-readable field name for error messages.
    pub fn label(&self) -> &'static str {
        match self {
            ConfigWrite::InputMode(_) => "input mode",
            ConfigWrite::SelectiveReporting { .. } => "selective reporting",
            ConfigWrite::LatencyMode(_) => "latency mode",
            ConfigWrite::ButtonPressThreshold(_) => "click force",
            ConfigWrite::HapticIntensity(_) => "haptic intensity",
        }
    }

    fn apply_to(&self, state: &mut ConfigState) {
        match *self {
            ConfigWrite::InputMode(v) => state.input_mode = Some(v),
            ConfigWrite::SelectiveReporting { surface, button } => {
                state.surface_switch = Some(surface);
                state.button_switch = Some(button);
            }
            ConfigWrite::LatencyMode(v) => state.latency_mode = Some(v),
            ConfigWrite::ButtonPressThreshold(v) => state.button_press_threshold = Some(v),
            ConfigWrite::HapticIntensity(v) => state.haptic_intensity = Some(v),
        }
    }

    /// Copy only the field(s) this write touched from `from` into `state`.
    fn revert_in(&self, state: &mut ConfigState, from: &ConfigState) {
        match self {
            ConfigWrite::InputMode(_) => state.input_mode = from.input_mode,
            ConfigWrite::SelectiveReporting { .. } => {
                state.surface_switch = from.surface_switch;
                state.button_switch = from.button_switch;
            }
            ConfigWrite::LatencyMode(_) => state.latency_mode = from.latency_mode,
            ConfigWrite::ButtonPressThreshold(_) => {
                state.button_press_threshold = from.button_press_threshold
            }
            ConfigWrite::HapticIntensity(_) => state.haptic_intensity = from.haptic_intensity,
        }
    }
}

/// Perform one write on a backend.
pub async fn apply_write<B: ConfigBackend>(backend: &mut B, write: ConfigWrite) -> io::Result<()> {
    match write {
        ConfigWrite::InputMode(v) => backend.write_input_mode(v).await,
        ConfigWrite::SelectiveReporting { surface, button } => {
            backend.write_selective_reporting(surface, button).await
        }
        ConfigWrite::LatencyMode(v) => backend.write_latency_mode(v).await,
        ConfigWrite::ButtonPressThreshold(v) => backend.write_button_press_threshold(v).await,
        ConfigWrite::HapticIntensity(v) => backend.write_haptic_intensity(v).await,
    }
}

/// UI → worker.
#[derive(Clone, Copy, Debug)]
pub enum ConfigCommand {
    Write(ConfigWrite),
    Refresh,
}

/// Worker → UI.
#[derive(Clone, Debug)]
pub enum ConfigEvent {
    /// Result of a `Refresh`.
    Values(ConfigValues),
    /// Result of a `Write`, in the order the writes were sent.
    WriteResult {
        write: ConfigWrite,
        result: Result<(), String>,
    },
}

/// The UI thread's view of the configuration.
pub struct ConfigHandle {
    pub description: ConfigDescription,
    pub state: ConfigState,
    /// Writes in flight, oldest first, each with the state as it was before
    /// the optimistic update — so a failure can put back exactly that field.
    pending: VecDeque<(ConfigWrite, ConfigState)>,
    cmd_tx: mpsc::Sender<ConfigCommand>,
    evt_rx: mpsc::Receiver<ConfigEvent>,
}

impl ConfigHandle {
    pub fn new(
        description: ConfigDescription,
        state: ConfigState,
        cmd_tx: mpsc::Sender<ConfigCommand>,
        evt_rx: mpsc::Receiver<ConfigEvent>,
    ) -> Self {
        Self {
            description,
            state,
            pending: VecDeque::new(),
            cmd_tx,
            evt_rx,
        }
    }

    /// Run the worker on a native thread and return the handle to it.
    pub fn spawn_thread<B: ConfigBackend + Send + 'static>(
        description: ConfigDescription,
        state: ConfigState,
        backend: B,
    ) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (evt_tx, evt_rx) = mpsc::channel();
        std::thread::spawn(move || {
            pollster::block_on(run_config_worker(backend, cmd_rx, evt_tx));
        });
        Self::new(description, state, cmd_tx, evt_rx)
    }

    fn write(&mut self, write: ConfigWrite) {
        self.pending.push_back((write, self.state));
        write.apply_to(&mut self.state);
        let _ = self.cmd_tx.send(ConfigCommand::Write(write));
    }

    pub fn set_input_mode(&mut self, value: u8) {
        self.write(ConfigWrite::InputMode(value));
    }

    pub fn set_selective_reporting(&mut self, surface: bool, button: bool) {
        self.write(ConfigWrite::SelectiveReporting { surface, button });
    }

    pub fn set_latency_mode(&mut self, high: bool) {
        self.write(ConfigWrite::LatencyMode(high));
    }

    pub fn set_button_press_threshold(&mut self, value: u8) {
        self.write(ConfigWrite::ButtonPressThreshold(value));
    }

    pub fn set_haptic_intensity(&mut self, value: u8) {
        self.write(ConfigWrite::HapticIntensity(value));
    }

    /// Re-read the readable fields from the device.
    pub fn refresh(&mut self) {
        let _ = self.cmd_tx.send(ConfigCommand::Refresh);
    }

    /// Drain worker events; call once per frame. Failed writes are reverted
    /// (that field only, so later queued writes stand) and logged.
    pub fn pump(&mut self) {
        while let Ok(event) = self.evt_rx.try_recv() {
            match event {
                ConfigEvent::Values(v) => self.state.apply_values(v),
                ConfigEvent::WriteResult { write, result } => {
                    // The worker is sequential and the channel is FIFO, so
                    // results arrive in the order the writes were queued.
                    let snapshot = match self.pending.pop_front() {
                        Some((pending_write, snapshot)) if pending_write == write => snapshot,
                        other => {
                            log::warn!(
                                "config: write result out of order ({:?} vs {:?})",
                                write,
                                other.map(|(w, _)| w)
                            );
                            continue;
                        }
                    };
                    if let Err(e) = result {
                        log::error!("config: failed to set {}: {}", write.label(), e);
                        write.revert_in(&mut self.state, &snapshot);
                    }
                }
            }
        }
    }
}

/// The worker loop: owns the backend, services commands until the UI side
/// goes away.
pub async fn run_config_worker<B: ConfigBackend>(
    mut backend: B,
    cmd_rx: mpsc::Receiver<ConfigCommand>,
    evt_tx: mpsc::Sender<ConfigEvent>,
) {
    while let Ok(cmd) = cmd_rx.recv() {
        let event = match cmd {
            ConfigCommand::Write(write) => ConfigEvent::WriteResult {
                write,
                result: apply_write(&mut backend, write)
                    .await
                    .map_err(|e| e.to_string()),
            },
            ConfigCommand::Refresh => ConfigEvent::Values(backend.read_all().await),
        };
        if evt_tx.send(event).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A backend that records writes and fails those whose value is 0xFF.
    #[derive(Default)]
    struct FakeBackend {
        writes: Vec<ConfigWrite>,
        values: ConfigValues,
    }

    impl ConfigBackend for FakeBackend {
        async fn read_all(&mut self) -> ConfigValues {
            self.values
        }
        async fn write_input_mode(&mut self, value: u8) -> io::Result<()> {
            self.record(ConfigWrite::InputMode(value), value)
        }
        async fn write_selective_reporting(
            &mut self,
            surface: bool,
            button: bool,
        ) -> io::Result<()> {
            self.record(ConfigWrite::SelectiveReporting { surface, button }, 0)
        }
        async fn write_latency_mode(&mut self, high: bool) -> io::Result<()> {
            self.record(ConfigWrite::LatencyMode(high), 0)
        }
        async fn write_button_press_threshold(&mut self, value: u8) -> io::Result<()> {
            self.record(ConfigWrite::ButtonPressThreshold(value), value)
        }
        async fn write_haptic_intensity(&mut self, value: u8) -> io::Result<()> {
            self.record(ConfigWrite::HapticIntensity(value), value)
        }
    }

    impl FakeBackend {
        fn record(&mut self, w: ConfigWrite, value: u8) -> io::Result<()> {
            self.writes.push(w);
            if value == 0xFF {
                Err(io::Error::other("rejected"))
            } else {
                Ok(())
            }
        }
    }

    fn description() -> ConfigDescription {
        ConfigDescription {
            features: PtpFeatures {
                has_input_mode: true,
                has_haptic_intensity: true,
                has_button_press_threshold: true,
                input_mode_writable: true,
                haptic_intensity_writable: true,
                button_press_threshold_writable: true,
                ..Default::default()
            },
            button_press_threshold_range: None,
            haptic_intensity_range: None,
            physical_size: None,
        }
    }

    #[test]
    fn initialize_reads_seeds_and_probes() {
        let mut backend = FakeBackend {
            values: ConfigValues {
                input_mode: Some(3),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut desc = description();
        let state = pollster::block_on(initialize(&mut backend, &mut desc));
        assert_eq!(state.input_mode, Some(3));
        assert_eq!(state.button_press_threshold, Some(2));
        assert_eq!(state.haptic_intensity, Some(50));
        // Probe writes: input mode, click force, haptic (no selective/latency fields)
        assert_eq!(
            backend.writes,
            vec![
                ConfigWrite::InputMode(3),
                ConfigWrite::ButtonPressThreshold(2),
                ConfigWrite::HapticIntensity(50),
            ]
        );
        assert!(desc.features.input_mode_writable);
    }

    #[test]
    fn failed_write_reverts_only_its_field() {
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (evt_tx, evt_rx) = mpsc::channel();
        let mut handle = ConfigHandle::new(description(), ConfigState::default(), cmd_tx, evt_rx);
        handle.state.haptic_intensity = Some(50);
        handle.state.button_press_threshold = Some(2);

        // Optimistic updates
        handle.set_haptic_intensity(0xFF); // will be rejected
        handle.set_button_press_threshold(3); // will succeed
        assert_eq!(handle.state.haptic_intensity, Some(0xFF));
        assert_eq!(handle.state.button_press_threshold, Some(3));

        // Drive the worker over the two queued commands, then hang up.
        drop(handle.cmd_tx.clone());
        let worker_cmds = cmd_rx;
        let mut backend = FakeBackend::default();
        pollster::block_on(async {
            for _ in 0..2 {
                let cmd = worker_cmds.recv().unwrap();
                let ConfigCommand::Write(write) = cmd else {
                    panic!()
                };
                let result = apply_write(&mut backend, write)
                    .await
                    .map_err(|e| e.to_string());
                evt_tx
                    .send(ConfigEvent::WriteResult { write, result })
                    .unwrap();
            }
        });

        handle.pump();
        assert_eq!(handle.state.haptic_intensity, Some(50), "reverted");
        assert_eq!(handle.state.button_press_threshold, Some(3), "kept");
        assert!(handle.pending.is_empty());
    }

    #[test]
    fn refresh_applies_readable_values_only() {
        let mut backend = FakeBackend {
            values: ConfigValues {
                input_mode: Some(0),
                latency_mode: Some(true),
                ..Default::default()
            },
            ..Default::default()
        };
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let (evt_tx, evt_rx) = mpsc::channel();
        let mut handle = ConfigHandle::new(description(), ConfigState::default(), cmd_tx, evt_rx);
        handle.state.haptic_intensity = Some(75);
        handle.refresh();
        drop(handle.cmd_tx.clone());
        // Run the real worker loop for exactly the queued command.
        let (tx2, rx2) = mpsc::channel();
        tx2.send(cmd_rx.recv().unwrap()).unwrap();
        drop(tx2);
        pollster::block_on(run_config_worker(&mut backend, rx2, evt_tx));
        handle.pump();
        assert_eq!(handle.state.input_mode, Some(0));
        assert_eq!(handle.state.latency_mode, Some(true));
        assert_eq!(handle.state.haptic_intensity, Some(75));
    }

    impl<B: ConfigBackend> ConfigBackend for &mut B {
        async fn read_all(&mut self) -> ConfigValues {
            (**self).read_all().await
        }
        async fn write_input_mode(&mut self, value: u8) -> io::Result<()> {
            (**self).write_input_mode(value).await
        }
        async fn write_selective_reporting(
            &mut self,
            surface: bool,
            button: bool,
        ) -> io::Result<()> {
            (**self).write_selective_reporting(surface, button).await
        }
        async fn write_latency_mode(&mut self, high: bool) -> io::Result<()> {
            (**self).write_latency_mode(high).await
        }
        async fn write_button_press_threshold(&mut self, value: u8) -> io::Result<()> {
            (**self).write_button_press_threshold(value).await
        }
        async fn write_haptic_intensity(&mut self, value: u8) -> io::Result<()> {
            (**self).write_haptic_intensity(value).await
        }
    }
}
