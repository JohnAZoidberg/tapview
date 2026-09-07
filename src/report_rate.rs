//! Estimate how fast the touchpad reports.
//!
//! Two rates, from two clocks, estimated differently on purpose:
//!
//! - the **pad** rate is the pad's scan cadence: the median step of the
//!   device's own stamp (PTP Scan Time, or the kernel's MSC_TIMESTAMP
//!   derived from it) between consecutive delivered frames. A median,
//!   because when the link skips a scan the step doubles, and the typical
//!   step is still the cadence the pad scans at;
//! - the **host** rate is throughput: frames that arrived divided by the
//!   host time they span. A count, not a median, because a bursty link (BLE
//!   handing frames over on its connection-interval grid) makes arrival
//!   intervals bimodal, and their median would read the grid rather than
//!   how many frames get through.
//!
//! So pad is never below host, apart from clock error, and the gap between
//! them is frames the link coalesced or dropped.
//!
//! Pads stop reporting when nothing touches them, so only frames with a
//! contact down count, intervals long enough to be an idle gap are left out,
//! and everything is taken over a short trailing window to keep the figure
//! current. The last good estimate is held while the pad is idle so the
//! number stays readable.

use crate::input::TouchState;
use std::collections::VecDeque;

/// Trailing window the estimate is taken over.
const WINDOW_US: u64 = 2_000_000;
/// Intervals longer than this are idle gaps (or a clock reset), not the
/// pad's cadence; they are left out.
const MAX_INTERVAL_US: u64 = 100_000;
/// Fewest intervals for an estimate.
const MIN_SAMPLES: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateEstimate {
    /// Frames per second reaching the host (count over elapsed time).
    pub host_hz: f32,
    /// The pad's scan cadence in Hz (median step of its own clock); `None`
    /// when the device gives no per-frame timestamp.
    pub pad_hz: Option<f32>,
}

impl RateEstimate {
    /// `pad 133 Hz · host 132 Hz`, or just `host 132 Hz` without a pad clock.
    pub fn label(&self) -> String {
        match self.pad_hz {
            Some(pad) => format!("pad {:.0} Hz · host {:.0} Hz", pad, self.host_hz),
            None => format!("host {:.0} Hz", self.host_hz),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Interval {
    /// Host stamp of the later frame; what the window is measured on.
    at: u64,
    host: u64,
    pad: Option<u64>,
}

#[derive(Debug, Default)]
pub struct RateEstimator {
    intervals: VecDeque<Interval>,
    /// Stamps of the previous frame with a contact down, if the one before
    /// this had one too.
    last: Option<(u64, Option<u64>)>,
    held: Option<RateEstimate>,
}

impl RateEstimator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold in one frame; a no-op for frames without a host stamp.
    pub fn push(&mut self, state: &TouchState) {
        let Some(now) = state.timestamp_us else {
            return;
        };
        let down = state.touches.iter().any(|t| t.used);
        if !down {
            // The frame after a lift-off must not measure the pause.
            self.last = None;
            return;
        }
        if let Some((prev_host, prev_pad)) = self.last {
            let host = now.saturating_sub(prev_host);
            if host > 0 && host <= MAX_INTERVAL_US {
                let pad = match (prev_pad, state.scan_time_us) {
                    (Some(a), Some(b)) if b > a && b - a <= MAX_INTERVAL_US => Some(b - a),
                    _ => None,
                };
                self.intervals.push_back(Interval { at: now, host, pad });
                while self
                    .intervals
                    .front()
                    .is_some_and(|i| i.at + WINDOW_US < now)
                {
                    self.intervals.pop_front();
                }
                if let Some(e) = self.compute() {
                    self.held = Some(e);
                }
            }
        }
        self.last = Some((now, state.scan_time_us));
    }

    /// The estimate over the frames pushed so far, or `None` for too few.
    pub fn estimate(&self) -> Option<RateEstimate> {
        self.held
    }

    /// Estimate over a batch of frames, oldest first, e.g. a slice of a
    /// recording.
    pub fn from_states<'a>(states: impl IntoIterator<Item = &'a TouchState>) -> Self {
        let mut e = Self::new();
        for s in states {
            e.push(s);
        }
        e
    }

    fn compute(&self) -> Option<RateEstimate> {
        if self.intervals.len() < MIN_SAMPLES {
            return None;
        }
        let span_us: u64 = self.intervals.iter().map(|i| i.host).sum();
        let host_hz = self.intervals.len() as f32 * 1_000_000.0 / span_us as f32;
        let pad_hz = median(self.intervals.iter().filter_map(|i| i.pad)).map(hz);
        Some(RateEstimate { host_hz, pad_hz })
    }
}

fn hz(interval_us: u64) -> f32 {
    1_000_000.0 / interval_us as f32
}

/// Median of at least [`MIN_SAMPLES`] values, else `None`.
fn median(values: impl Iterator<Item = u64>) -> Option<u64> {
    let mut v: Vec<u64> = values.collect();
    if v.len() < MIN_SAMPLES {
        return None;
    }
    let mid = v.len() / 2;
    let (_, m, _) = v.select_nth_unstable(mid);
    Some(*m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(t_us: u64, scan_us: Option<u64>, down: bool) -> TouchState {
        let mut s = TouchState {
            timestamp_us: Some(t_us),
            scan_time_us: scan_us,
            ..TouchState::default()
        };
        s.touches[0].used = down;
        s
    }

    #[test]
    fn steady_stream_gives_both_rates() {
        let mut e = RateEstimator::new();
        for i in 0..30u64 {
            // Host sees 8 ms, pad stamps 7.5 ms.
            e.push(&frame(i * 8_000, Some(i * 7_500), true));
        }
        let r = e.estimate().unwrap();
        assert_eq!(r.host_hz.round(), 125.0);
        assert_eq!(r.pad_hz.unwrap().round(), 133.0);
        assert_eq!(r.label(), "pad 133 Hz · host 125 Hz");
    }

    #[test]
    fn no_pad_clock() {
        let mut e = RateEstimator::new();
        for i in 0..30u64 {
            e.push(&frame(i * 10_000, None, true));
        }
        let r = e.estimate().unwrap();
        assert_eq!(r.host_hz.round(), 100.0);
        assert_eq!(r.pad_hz, None);
        assert_eq!(r.label(), "host 100 Hz");
    }

    #[test]
    fn needs_enough_samples_and_a_finger() {
        let mut e = RateEstimator::new();
        for i in 0..MIN_SAMPLES as u64 {
            e.push(&frame(i * 8_000, None, true));
        }
        // MIN_SAMPLES frames make MIN_SAMPLES - 1 intervals.
        assert!(e.estimate().is_none());
        e.push(&frame(MIN_SAMPLES as u64 * 8_000, None, true));
        assert!(e.estimate().is_some());

        let mut idle = RateEstimator::new();
        for i in 0..50u64 {
            idle.push(&frame(i * 8_000, None, false));
        }
        assert!(idle.estimate().is_none());
    }

    #[test]
    fn lift_off_gap_is_not_an_interval_and_last_estimate_is_held() {
        let mut e = RateEstimator::new();
        for i in 0..20u64 {
            e.push(&frame(i * 8_000, None, true));
        }
        let before = e.estimate().unwrap();
        // Lifted for a second, then straight back down.
        e.push(&frame(160_000, None, false));
        assert_eq!(e.estimate(), Some(before));
        e.push(&frame(1_160_000, None, true));
        e.push(&frame(1_168_000, None, true));
        assert_eq!(e.intervals.back().unwrap().host, 8_000);
        assert!(e.intervals.iter().all(|i| i.host <= MAX_INTERVAL_US));
        assert_eq!(e.estimate(), Some(before));
    }

    /// Every third scan never becomes a report: the pad's cadence is still
    /// one tick, the host gets fewer frames.
    #[test]
    fn skipped_scans_show_as_pad_above_host() {
        let mut e = RateEstimator::new();
        let (mut t, mut scan) = (0, 0);
        for i in 0..60u64 {
            t += 10_000;
            scan += if i % 3 == 2 { 15_000 } else { 7_500 };
            e.push(&frame(t, Some(scan), true));
        }
        let r = e.estimate().unwrap();
        assert_eq!(r.pad_hz.unwrap().round(), 133.0);
        assert_eq!(r.host_hz.round(), 100.0);
    }

    /// Frames come in clumps (two 7.5 ms apart, then a 15 ms gap): the
    /// median arrival interval would say 133 Hz, but only 100 frames a
    /// second get through, matching the pad.
    #[test]
    fn bursty_delivery_counts_frames_not_gaps() {
        let mut e = RateEstimator::new();
        let (mut t, mut scan) = (0, 0);
        for i in 0..60u64 {
            t += if i % 3 == 2 { 15_000 } else { 7_500 };
            scan += 10_000;
            e.push(&frame(t, Some(scan), true));
        }
        let r = e.estimate().unwrap();
        assert_eq!(r.host_hz.round(), 100.0);
        assert_eq!(r.pad_hz.unwrap().round(), 100.0);
    }

    #[test]
    fn window_slides() {
        let mut e = RateEstimator::new();
        let mut t = 0;
        for _ in 0..40u64 {
            t += 8_000;
            e.push(&frame(t, None, true));
        }
        assert_eq!(e.estimate().unwrap().host_hz.round(), 125.0);

        // Three more seconds at a slower rate: the old intervals age out.
        for _ in 0..150u64 {
            t += 20_000;
            e.push(&frame(t, None, true));
        }
        assert_eq!(e.estimate().unwrap().host_hz.round(), 50.0);
        assert!(e.intervals.iter().all(|i| i.host == 20_000));
    }

    #[test]
    fn pad_clock_reset_is_skipped() {
        let mut e = RateEstimator::new();
        for i in 0..20u64 {
            // The kernel restarts MSC_TIMESTAMP at 0; frame 10 goes backwards.
            let scan = if i < 10 {
                500_000 + i * 8_000
            } else {
                (i - 10) * 8_000
            };
            e.push(&frame(i * 8_000, Some(scan), true));
        }
        assert_eq!(e.intervals.iter().filter(|i| i.pad.is_none()).count(), 1);
        assert_eq!(e.estimate().unwrap().pad_hz.unwrap().round(), 125.0);
    }
}
