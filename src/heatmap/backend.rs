use super::chips::{identify_chip, read_frame, read_matrix_dims, ChipVariant};
use super::power::{self, PowerStatus, SleepWake};
use super::protocol::{read_reg, read_user_reg};
use super::{HeatmapFrame, HidDevice};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::{mpsc, Mutex};
use std::thread;
use std::time::Duration;
use web_time::Instant;

/// How long a paused native loop sleeps between checks of its switch.
const PAUSE_POLL: Duration = Duration::from_millis(50);

/// How often the loop reads the pad's power registers, between frames and
/// also while paused (a handful of register reads, cheap next to a frame).
const POWER_POLL: Duration = Duration::from_millis(500);

/// The switches a running heatmap loop watches, shared with its
/// [`HeatmapStream`].
///
/// Polling the heatmap is a stream of feature-report round trips that
/// competes with everything else on the link (and on Bluetooth all but
/// saturates it), so the UI can pause it without tearing the loop down.
#[derive(Clone, Debug, Default)]
pub struct HeatmapControl {
    inner: Arc<Flags>,
}

#[derive(Debug)]
struct Flags {
    paused: AtomicBool,
    closed: AtomicBool,
    /// The last power registers read, `None` until read or on chips
    /// without them.
    power: Mutex<Option<PowerStatus>>,
    /// A sleep-wake change picked in the UI, for the loop (which owns the
    /// device) to write before its next power read.
    sleep_wake: Mutex<Option<SleepWake>>,
}

impl Default for Flags {
    fn default() -> Self {
        Self {
            paused: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            power: Mutex::new(None),
            sleep_wake: Mutex::new(None),
        }
    }
}

impl HeatmapControl {
    pub fn enabled(&self) -> bool {
        !self.inner.paused.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, on: bool) {
        self.inner.paused.store(!on, Ordering::Relaxed);
    }

    fn closed(&self) -> bool {
        self.inner.closed.load(Ordering::Relaxed)
    }

    fn take_sleep_wake(&self) -> Option<SleepWake> {
        self.inner.sleep_wake.lock().unwrap().take()
    }

    fn set_power(&self, status: Option<PowerStatus>) {
        *self.inner.power.lock().unwrap() = status;
    }
}

/// The UI's end of a heatmap loop: its frames and its on/off switch. The
/// loop ends when this is dropped.
pub struct HeatmapStream {
    pub rx: mpsc::Receiver<HeatmapFrame>,
    control: HeatmapControl,
}

impl HeatmapStream {
    pub fn new(rx: mpsc::Receiver<HeatmapFrame>, control: HeatmapControl) -> Self {
        Self { rx, control }
    }

    /// Whether the loop is polling frames.
    pub fn enabled(&self) -> bool {
        self.control.enabled()
    }

    /// Pause or resume polling. Takes effect before the next frame read.
    pub fn set_enabled(&self, on: bool) {
        self.control.set_enabled(on);
    }

    /// The pad's power registers as last read (at most [`POWER_POLL`]
    /// old), `None` before the first read or on chips without them.
    pub fn power(&self) -> Option<PowerStatus> {
        *self.control.inner.power.lock().unwrap()
    }

    /// Choose what wakes the pad from a host SET_POWER sleep; the loop
    /// writes it before its next power read.
    pub fn set_sleep_wake(&self, wake: SleepWake) {
        *self.control.inner.sleep_wake.lock().unwrap() = Some(wake);
    }
}

impl Drop for HeatmapStream {
    fn drop(&mut self) {
        // A paused loop never sends, so it would not notice the receiver
        // going away on its own.
        self.control.inner.closed.store(true, Ordering::Relaxed);
    }
}

/// Spawn a background thread that continuously reads raw capacitive frames
/// from the platform HID device at `hidraw_path` and sends them over a channel.
#[cfg(any(target_os = "linux", target_os = "windows"))]
pub fn spawn_heatmap_thread(
    hidraw_path: &std::path::Path,
    burst_len: usize,
    cols_override: Option<usize>,
) -> HeatmapStream {
    let (tx, rx) = mpsc::channel();
    let control = HeatmapControl::default();
    let path = hidraw_path.to_path_buf();

    let loop_control = control.clone();
    thread::spawn(move || {
        let dev = match super::open_platform_hid_device(&path) {
            Ok(d) => d,
            Err(e) => {
                log::error!("heatmap: failed to open {}: {}", path.display(), e);
                return;
            }
        };

        // The HID layer is async for the browser's sake; on a native thread
        // block_on simply blocks where the ioctl used to.
        pollster::block_on(run_heatmap_loop(
            &dev,
            burst_len,
            cols_override,
            &tx,
            loop_control,
            native_pause,
        ));
    });

    HeatmapStream::new(rx, control)
}

/// Like [`spawn_heatmap_thread`] for an already opened device (the Android
/// USB transport, or anything else that is not addressed by a path).
pub fn spawn_heatmap_thread_with<D: HidDevice + Send + 'static>(
    dev: D,
    burst_len: usize,
    cols_override: Option<usize>,
) -> HeatmapStream {
    let (tx, rx) = mpsc::channel();
    let control = HeatmapControl::default();
    let loop_control = control.clone();
    thread::spawn(move || {
        pollster::block_on(run_heatmap_loop(
            &dev,
            burst_len,
            cols_override,
            &tx,
            loop_control,
            native_pause,
        ));
    });
    HeatmapStream::new(rx, control)
}

/// The `pause` for a loop on a native thread of its own: just sleep.
async fn native_pause() {
    thread::sleep(PAUSE_POLL);
}

/// Identify the chip, read its matrix dimensions, then stream frames into
/// `tx` until the [`HeatmapStream`] is dropped or a read fails. While the
/// stream is disabled the loop reads nothing and awaits `pause()` between
/// checks: a sleep on a native thread, a timer future that yields to the
/// event loop in the browser.
pub async fn run_heatmap_loop<D, F, Fut>(
    dev: &D,
    burst_len: usize,
    cols_override: Option<usize>,
    tx: &mpsc::Sender<HeatmapFrame>,
    control: HeatmapControl,
    mut pause: F,
) where
    D: HidDevice,
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    let chip = match identify_chip(dev).await {
        Ok(c) => c,
        Err(e) => {
            log::error!("heatmap: failed to identify chip: {}", e);
            return;
        }
    };

    let (rows, cols) = match read_matrix_dims(dev, chip).await {
        Ok(d) => d,
        Err(e) => {
            log::error!("heatmap: failed to read matrix dimensions: {}", e);
            return;
        }
    };

    log::info!(
        "heatmap: {} detected, {}x{} matrix, burst_len={}",
        chip,
        rows,
        cols,
        burst_len
    );

    // Dump candidate dimension registers for unknown/new chips
    if chip == ChipVariant::PJP343 {
        probe_dimension_registers(dev).await;
    }

    // Display cols can be overridden for stride debugging
    let display_cols = cols_override.unwrap_or(cols);
    if cols_override.is_some() {
        log::info!("heatmap: display cols overridden to {}", display_cols);
    }

    // When the power registers are next read; `None` on chips without them
    // or once a read has failed.
    let mut power_due = power::has_op_mode(chip).then(Instant::now);
    loop {
        if control.closed() {
            break;
        }
        if power_due.is_some_and(|due| Instant::now() >= due) {
            power_due = poll_power(dev, chip, &control)
                .await
                .then(|| Instant::now() + POWER_POLL);
        }
        if !control.enabled() {
            pause().await;
            continue;
        }
        // Hardware read always uses register-derived dimensions
        match read_frame(dev, chip, rows, cols, burst_len).await {
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
                log::error!("heatmap: frame read error: {}", e);
                break;
            }
        }
    }
}

/// Write a pending sleep-wake change, then read the power registers into
/// `control`. Returns whether to keep polling: a read failure is logged and
/// the power display dropped, but the heatmap goes on.
async fn poll_power<D: HidDevice>(dev: &D, chip: ChipVariant, control: &HeatmapControl) -> bool {
    if let Some(wake) = control.take_sleep_wake() {
        match power::set_sleep_wake(dev, chip, wake).await {
            Ok(()) => log::info!("heatmap: SET_POWER sleep now wakes on {}", wake),
            Err(e) => log::warn!("heatmap: setting sleep wake to {}: {}", wake, e),
        }
    }
    match power::read_power(dev, chip).await {
        Ok(status) => {
            control.set_power(Some(status));
            true
        }
        Err(e) => {
            log::warn!(
                "heatmap: reading the power registers failed, not polling them again: {}",
                e
            );
            control.set_power(None);
            false
        }
    }
}

async fn probe_dimension_registers<D: HidDevice>(dev: &D) {
    log::info!("heatmap: --- PJP343 register probe ---");

    // PJP274 style: UserBank 0, 0x6E/0x6F
    if let (Ok(s), Ok(d)) = (
        read_user_reg(dev, 0, 0x6E).await,
        read_user_reg(dev, 0, 0x6F).await,
    ) {
        log::info!("  UserBank0 0x6E(senses)={} 0x6F(drives)={}", s, d);
    }
    // Check adjacent registers for 16-bit values
    if let (Ok(a), Ok(b), Ok(c), Ok(d)) = (
        read_user_reg(dev, 0, 0x6C).await,
        read_user_reg(dev, 0, 0x6D).await,
        read_user_reg(dev, 0, 0x70).await,
        read_user_reg(dev, 0, 0x71).await,
    ) {
        log::info!("  UserBank0 0x6C={} 0x6D={} 0x70={} 0x71={}", a, b, c, d);
    }

    // PJP255 style: UserBank 0, 0x59/0x5A
    if let (Ok(s), Ok(d)) = (
        read_user_reg(dev, 0, 0x59).await,
        read_user_reg(dev, 0, 0x5A).await,
    ) {
        log::info!("  UserBank0 0x59(senses)={} 0x5A(drives)={}", s, d);
    }

    // PLP239 style: Bank 9, 0x01/0x02
    if let (Ok(d), Ok(s)) = (read_reg(dev, 9, 0x01).await, read_reg(dev, 9, 0x02).await) {
        log::info!("  Bank9 0x01(drives)={} 0x02(senses)={}", d, s);
    }

    // Scan UserBank 0 around 0x60-0x7F for anything that looks like a dimension
    let mut scan = String::from("  UserBank0 0x60..0x7F:");
    for addr in 0x60..=0x7F {
        if let Ok(v) = read_user_reg(dev, 0, addr).await {
            scan.push_str(&format!(" {:02X}={}", addr, v));
        }
    }
    log::info!("{}", scan);
    log::info!("heatmap: --- end probe ---");
}
