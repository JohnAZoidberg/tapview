pub mod backend;
pub mod chips;
pub mod discovery;
pub mod hidraw;
pub mod protocol;

/// A single frame of raw capacitive heatmap data.
#[derive(Clone)]
pub struct HeatmapFrame {
    pub rows: usize,
    pub cols: usize,
    /// Row-major signed 16-bit capacitance values (rows * cols elements).
    pub data: Vec<i16>,
}
