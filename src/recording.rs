use crate::input::TouchState;
use crate::multitouch::{ButtonState, TouchData, MAX_TOUCH_POINTS};
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::time::Instant;

const MAGIC: &[u8; 4] = b"TAPV";
const VERSION: u32 = 1;

fn write_bool(w: &mut impl Write, v: bool) -> io::Result<()> {
    w.write_all(&[v as u8])
}

fn read_bool(r: &mut impl Read) -> io::Result<bool> {
    let mut buf = [0u8; 1];
    r.read_exact(&mut buf)?;
    Ok(buf[0] != 0)
}

fn write_i32(w: &mut impl Write, v: i32) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn read_i32(r: &mut impl Read) -> io::Result<i32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(i32::from_le_bytes(buf))
}

fn write_u32(w: &mut impl Write, v: u32) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn read_u32(r: &mut impl Read) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn write_u64(w: &mut impl Write, v: u64) -> io::Result<()> {
    w.write_all(&v.to_le_bytes())
}

fn read_u64(r: &mut impl Read) -> io::Result<u64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

fn write_touch_data(w: &mut impl Write, t: &TouchData) -> io::Result<()> {
    write_bool(w, t.used)?;
    write_bool(w, t.pressed)?;
    write_bool(w, t.pressed_double)?;
    write_i32(w, t.tracking_id)?;
    write_i32(w, t.position_x)?;
    write_i32(w, t.position_y)?;
    write_i32(w, t.pressure)?;
    write_i32(w, t.distance)?;
    write_i32(w, t.touch_major)?;
    write_i32(w, t.touch_minor)?;
    write_i32(w, t.width_major)?;
    write_i32(w, t.width_minor)?;
    write_i32(w, t.orientation)?;
    write_i32(w, t.tool_x)?;
    write_i32(w, t.tool_y)?;
    write_i32(w, t.tool_type)
}

fn read_touch_data(r: &mut impl Read) -> io::Result<TouchData> {
    Ok(TouchData {
        used: read_bool(r)?,
        pressed: read_bool(r)?,
        pressed_double: read_bool(r)?,
        tracking_id: read_i32(r)?,
        position_x: read_i32(r)?,
        position_y: read_i32(r)?,
        pressure: read_i32(r)?,
        distance: read_i32(r)?,
        touch_major: read_i32(r)?,
        touch_minor: read_i32(r)?,
        width_major: read_i32(r)?,
        width_minor: read_i32(r)?,
        orientation: read_i32(r)?,
        tool_x: read_i32(r)?,
        tool_y: read_i32(r)?,
        tool_type: read_i32(r)?,
    })
}

fn write_touch_state(w: &mut impl Write, state: &TouchState) -> io::Result<()> {
    for touch in &state.touches {
        write_touch_data(w, touch)?;
    }
    write_bool(w, state.buttons.left)?;
    write_bool(w, state.buttons.right)?;
    write_bool(w, state.buttons.middle)
}

fn read_touch_state(r: &mut impl Read) -> io::Result<TouchState> {
    let mut touches = [TouchData::default(); MAX_TOUCH_POINTS];
    for touch in &mut touches {
        *touch = read_touch_data(r)?;
    }
    let buttons = ButtonState {
        left: read_bool(r)?,
        right: read_bool(r)?,
        middle: read_bool(r)?,
    };
    Ok(TouchState { touches, buttons })
}

/// Records touch frames to a binary file with timestamps.
pub struct Recorder {
    writer: BufWriter<File>,
    start: Instant,
}

impl Recorder {
    pub fn new(path: &str, extent_x: i32, extent_y: i32) -> io::Result<Self> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        writer.write_all(MAGIC)?;
        write_u32(&mut writer, VERSION)?;
        write_i32(&mut writer, extent_x)?;
        write_i32(&mut writer, extent_y)?;
        Ok(Self {
            writer,
            start: Instant::now(),
        })
    }

    pub fn record(&mut self, state: &TouchState) -> io::Result<()> {
        let elapsed = self.start.elapsed();
        let timestamp_us = elapsed.as_micros() as u64;
        write_u64(&mut self.writer, timestamp_us)?;
        write_touch_state(&mut self.writer, state)
    }

    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

pub struct RecordedFrame {
    pub timestamp_us: u64,
    pub state: TouchState,
}

/// A loaded recording with all frames in memory.
pub struct Recording {
    pub frames: Vec<RecordedFrame>,
    pub extent_x: i32,
    pub extent_y: i32,
}

impl Recording {
    pub fn load(path: &str) -> io::Result<Self> {
        let file = File::open(path)?;
        let mut reader = BufReader::new(file);

        let mut magic = [0u8; 4];
        reader.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a TAPV file",
            ));
        }

        let version = read_u32(&mut reader)?;
        if version != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unsupported version: {}", version),
            ));
        }

        let extent_x = read_i32(&mut reader)?;
        let extent_y = read_i32(&mut reader)?;

        let mut frames = Vec::new();
        loop {
            match read_u64(&mut reader) {
                Ok(timestamp_us) => {
                    match read_touch_state(&mut reader) {
                        Ok(state) => {
                            frames.push(RecordedFrame {
                                timestamp_us,
                                state,
                            });
                        }
                        // Truncated final frame (e.g. Ctrl+C during recording)
                        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                        Err(e) => return Err(e),
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            }
        }

        Ok(Self {
            frames,
            extent_x,
            extent_y,
        })
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames
            .last()
            .map(|f| f.timestamp_us as f64 / 1_000_000.0)
            .unwrap_or(0.0)
    }

    /// Find the frame closest to the given time (binary search).
    pub fn frame_at(&self, time_secs: f64) -> Option<&RecordedFrame> {
        if self.frames.is_empty() {
            return None;
        }
        let target_us = (time_secs * 1_000_000.0) as u64;
        let idx = self
            .frames
            .binary_search_by_key(&target_us, |f| f.timestamp_us)
            .unwrap_or_else(|i| i.min(self.frames.len() - 1));
        Some(&self.frames[idx])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn sample_touch_data() -> TouchData {
        TouchData {
            used: true,
            pressed: true,
            pressed_double: false,
            tracking_id: 42,
            position_x: 1000,
            position_y: 2000,
            pressure: 128,
            distance: 0,
            touch_major: 50,
            touch_minor: 30,
            width_major: 60,
            width_minor: 40,
            orientation: 90,
            tool_x: 1001,
            tool_y: 2001,
            tool_type: 1,
        }
    }

    fn assert_touch_data_eq(a: &TouchData, b: &TouchData) {
        assert_eq!(a.used, b.used);
        assert_eq!(a.pressed, b.pressed);
        assert_eq!(a.pressed_double, b.pressed_double);
        assert_eq!(a.tracking_id, b.tracking_id);
        assert_eq!(a.position_x, b.position_x);
        assert_eq!(a.position_y, b.position_y);
        assert_eq!(a.pressure, b.pressure);
        assert_eq!(a.distance, b.distance);
        assert_eq!(a.touch_major, b.touch_major);
        assert_eq!(a.touch_minor, b.touch_minor);
        assert_eq!(a.width_major, b.width_major);
        assert_eq!(a.width_minor, b.width_minor);
        assert_eq!(a.orientation, b.orientation);
        assert_eq!(a.tool_x, b.tool_x);
        assert_eq!(a.tool_y, b.tool_y);
        assert_eq!(a.tool_type, b.tool_type);
    }

    fn assert_touch_state_eq(a: &TouchState, b: &TouchState) {
        for i in 0..MAX_TOUCH_POINTS {
            assert_touch_data_eq(&a.touches[i], &b.touches[i]);
        }
        assert_eq!(a.buttons.left, b.buttons.left);
        assert_eq!(a.buttons.right, b.buttons.right);
        assert_eq!(a.buttons.middle, b.buttons.middle);
    }

    #[test]
    fn test_round_trip_touch_data() {
        let original = sample_touch_data();
        let mut buf = Vec::new();
        write_touch_data(&mut buf, &original).unwrap();
        let mut cursor = Cursor::new(&buf);
        let loaded = read_touch_data(&mut cursor).unwrap();
        assert_touch_data_eq(&original, &loaded);
    }

    #[test]
    fn test_round_trip_touch_state() {
        let mut state = TouchState::default();
        state.touches[0] = sample_touch_data();
        state.touches[3] = TouchData {
            used: true,
            tracking_id: 7,
            position_x: 500,
            position_y: 600,
            ..TouchData::default()
        };
        state.buttons.left = true;
        state.buttons.middle = true;

        let mut buf = Vec::new();
        write_touch_state(&mut buf, &state).unwrap();
        let mut cursor = Cursor::new(&buf);
        let loaded = read_touch_state(&mut cursor).unwrap();
        assert_touch_state_eq(&state, &loaded);
    }

    #[test]
    fn test_load_sample_recording() {
        let rec = Recording::load("testdata/sample.tapv").unwrap();
        assert!(rec.frames.len() > 0, "expected frames, got 0");
        assert!(rec.duration_secs() > 0.0);
        assert_eq!(rec.extent_x, 3841);
        assert_eq!(rec.extent_y, 2392);

        // frame_at boundaries
        let first = rec.frame_at(0.0).unwrap();
        assert_eq!(first.timestamp_us, rec.frames[0].timestamp_us);
        let last = rec.frame_at(rec.duration_secs()).unwrap();
        assert_eq!(last.timestamp_us, rec.frames.last().unwrap().timestamp_us);
    }

    #[test]
    fn test_record_and_reload() {
        let dir = std::env::temp_dir().join("tapview_test_record_reload.tapv");
        let path = dir.to_str().unwrap();

        let state = {
            let mut s = TouchState::default();
            s.touches[0] = sample_touch_data();
            s.buttons.right = true;
            s
        };

        {
            let mut rec = Recorder::new(path, 1920, 1080).unwrap();
            rec.record(&state).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(10));
            rec.record(&state).unwrap();
            rec.flush().unwrap();
        }

        let loaded = Recording::load(path).unwrap();
        assert_eq!(loaded.extent_x, 1920);
        assert_eq!(loaded.extent_y, 1080);
        assert_eq!(loaded.frames.len(), 2);
        assert!(loaded.frames[1].timestamp_us > loaded.frames[0].timestamp_us);
        assert_touch_state_eq(&loaded.frames[0].state, &state);
        assert_touch_state_eq(&loaded.frames[1].state, &state);

        std::fs::remove_file(path).ok();
    }

    #[test]
    fn test_truncated_file() {
        let dir = std::env::temp_dir().join("tapview_test_truncated.tapv");
        let path = dir.to_str().unwrap();

        {
            let mut rec = Recorder::new(path, 800, 600).unwrap();
            let state = TouchState::default();
            for _ in 0..10 {
                rec.record(&state).unwrap();
            }
            rec.flush().unwrap();
        }

        let full = Recording::load(path).unwrap();
        assert_eq!(full.frames.len(), 10);

        // Truncate mid-frame: keep header + 5 full frames + partial 6th
        let file_len = std::fs::metadata(path).unwrap().len();
        let header_size: u64 = 4 + 4 + 4 + 4; // MAGIC + VERSION + extent_x + extent_y
        let frame_size = (file_len - header_size) / 10;
        let truncated_len = header_size + frame_size * 5 + frame_size / 2;
        let data = std::fs::read(path).unwrap();
        std::fs::write(path, &data[..truncated_len as usize]).unwrap();

        let partial = Recording::load(path).unwrap();
        assert_eq!(partial.frames.len(), 5);
        assert_eq!(partial.extent_x, 800);
        assert_eq!(partial.extent_y, 600);

        std::fs::remove_file(path).ok();
    }
}
