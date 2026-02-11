pub mod backend;
pub mod chips;
pub mod discovery;
pub mod hidraw;
pub mod protocol;

/// Commands that can be sent to the heatmap backend thread.
pub enum AlcCommand {
    /// Force ALC IIR filter reset (clears learned baseline).
    Reset,
    /// Enable ALC (automatic gain adjustment).
    Enable,
    /// Disable ALC (freeze current gain values).
    Disable,
}

/// A single frame of raw capacitive heatmap data.
#[derive(Clone)]
pub struct HeatmapFrame {
    pub rows: usize,
    pub cols: usize,
    /// Row-major signed 16-bit capacitance values (rows * cols elements).
    pub data: Vec<i16>,
    /// Mean value across all cells.
    pub mean: f64,
    /// Slow EMA of mean, smoothing out touches to reveal baseline drift.
    pub smoothed_mean: f64,
    /// Drift rate: change in smoothed baseline per frame (slow EMA derivative).
    pub drift_rate: f64,
    /// True if sustained drift detected (firmware calibration in progress).
    pub calibrating: bool,
}
