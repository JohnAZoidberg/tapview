//! Parser for Precision Touchpad (PTP) HID input reports.
//!
//! Turns the raw touch reports a pad sends (over hidraw, USB or WebHID)
//! into the same [`TouchState`] the evdev backend produces, so every
//! visualizer front end sees identical data. The report layout comes from the
//! device's descriptor via [`PtpLayout::from_layout`]; the semantics mirror
//! the kernel's hid-multitouch driver plus `input_mt`, so a hidraw-fed view
//! diffs cleanly against the evdev one:
//!
//! - contacts are tracked by Contact ID and assigned to the lowest free slot,
//!   with a monotonically increasing tracking id per slot allocation;
//! - Confidence = 0 marks a palm (`tool_type = MT_TOOL_PALM`);
//! - width/height become touch major/minor with orientation = width > height;
//! - "hybrid mode" (Contact Count only in the first of several reports per
//!   frame) is reassembled into one frame;
//! - slot 0 carries the BTN_TOUCH / BTN_TOOL_DOUBLETAP emulation (`pressed`,
//!   `pressed_double`) like the evdev state machine does.

use crate::hid::{
    decode_unit_exponent, extract_bits, extract_signed, Collection, ReportField, ReportKind,
    ReportLayout,
};
use crate::input::{host_now_us, TouchState};
use crate::multitouch::{ButtonState, TouchData, MAX_TOUCH_POINTS};

/// `tool_type` value marking a palm (Linux `MT_TOOL_PALM`).
pub const MT_TOOL_PALM: i32 = 0x02;

const GENERIC_DESKTOP: u16 = 0x01;
const BUTTON: u16 = 0x09;
const DIGITIZER: u16 = 0x0D;

const USAGE_X: u16 = 0x30;
const USAGE_Y: u16 = 0x31;
const USAGE_TOUCH_SCREEN: u16 = 0x04;
const USAGE_TOUCH_PAD: u16 = 0x05;
const USAGE_FINGER: u16 = 0x22;
const USAGE_TIP_PRESSURE: u16 = 0x30;
const USAGE_IN_RANGE: u16 = 0x32;
const USAGE_TIP_SWITCH: u16 = 0x42;
const USAGE_CONFIDENCE: u16 = 0x47;
const USAGE_WIDTH: u16 = 0x48;
const USAGE_HEIGHT: u16 = 0x49;
const USAGE_CONTACT_ID: u16 = 0x51;
const USAGE_CONTACT_COUNT: u16 = 0x54;
const USAGE_CONTACT_COUNT_MAX: u16 = 0x55;
const USAGE_SCAN_TIME: u16 = 0x56;

/// The fields of one Finger collection.
#[derive(Clone, Debug, Default)]
pub struct FingerFields {
    pub tip_switch: Option<ReportField>,
    pub confidence: Option<ReportField>,
    pub contact_id: Option<ReportField>,
    pub x: Option<ReportField>,
    pub y: Option<ReportField>,
    pub width: Option<ReportField>,
    pub height: Option<ReportField>,
    pub pressure: Option<ReportField>,
}

/// Where everything lives in the pad's touch report.
#[derive(Clone, Debug)]
pub struct PtpLayout {
    /// Report ID of the touch report (0 if unnumbered).
    pub report_id: u8,
    /// Payload bytes, excluding the report-ID byte.
    pub report_bytes: usize,
    pub contact_count: Option<ReportField>,
    pub scan_time: Option<ReportField>,
    /// Microseconds per Scan Time tick, from the field's Unit and Unit
    /// Exponent; 100 µs (what the Precision Touchpad spec prescribes) when
    /// the descriptor declares no time unit.
    pub scan_time_unit_us: f64,
    /// Button-page fields outside the Finger collections; usage 1/2/3 =
    /// left/right/middle.
    pub buttons: Vec<ReportField>,
    /// One per Finger collection, in descriptor order.
    pub fingers: Vec<FingerFields>,
    /// Contact Count Maximum from the Feature report, else `fingers.len()`.
    pub contact_count_max: usize,
    /// Logical maxima of the first finger's X/Y: the pad's coordinate extents.
    pub x_max: i32,
    pub y_max: i32,
}

impl PtpLayout {
    /// Locate the touch report inside a parsed descriptor: the Input report
    /// of the Touch Pad application collection (Digitizer page, usage 0x05).
    /// `None` if the descriptor has no such collection or no Finger fields.
    pub fn from_layout(layout: &ReportLayout) -> Option<Self> {
        let touchpad = layout.collections.iter().position(|c| {
            c.kind == Collection::APPLICATION
                && c.usage_page == DIGITIZER
                && c.usage == USAGE_TOUCH_PAD
        })?;

        // Is `field` (transitively) inside collection `idx`? Returns the
        // innermost Finger collection on the way, if any.
        let chain = |field: &ReportField| -> Option<Option<usize>> {
            let mut finger = None;
            let mut cur = field.collection;
            while let Some(i) = cur {
                let c = &layout.collections[i];
                if c.usage_page == DIGITIZER && c.usage == USAGE_FINGER && finger.is_none() {
                    finger = Some(i);
                }
                if i == touchpad {
                    return Some(finger);
                }
                cur = c.parent;
            }
            None
        };

        let inputs: Vec<(&ReportField, Option<usize>)> = layout
            .fields
            .iter()
            .filter(|f| f.kind == ReportKind::Input)
            .filter_map(|f| chain(f).map(|finger| (f, finger)))
            .collect();

        // The touch report is the one carrying Contact Count, else the first
        // report with any field in the collection.
        let report_id = inputs
            .iter()
            .find(|(f, _)| f.usage_page == DIGITIZER && f.usage == USAGE_CONTACT_COUNT)
            .or(inputs.first())
            .map(|(f, _)| f.report_id)?;

        let mut ptp = PtpLayout {
            report_id,
            report_bytes: layout.report_bytes(ReportKind::Input, report_id),
            contact_count: None,
            scan_time: None,
            scan_time_unit_us: DEFAULT_SCAN_TIME_UNIT_US,
            buttons: Vec::new(),
            fingers: Vec::new(),
            contact_count_max: 0,
            x_max: 0,
            y_max: 0,
        };

        let mut finger_indices: Vec<usize> = Vec::new();
        for (f, finger) in inputs.into_iter().filter(|(f, _)| f.report_id == report_id) {
            match finger {
                Some(fi) => {
                    let pos = match finger_indices.iter().position(|&i| i == fi) {
                        Some(p) => p,
                        None => {
                            finger_indices.push(fi);
                            ptp.fingers.push(FingerFields::default());
                            ptp.fingers.len() - 1
                        }
                    };
                    let ff = &mut ptp.fingers[pos];
                    let slot = match (f.usage_page, f.usage) {
                        (DIGITIZER, USAGE_TIP_SWITCH) => &mut ff.tip_switch,
                        (DIGITIZER, USAGE_CONFIDENCE) => &mut ff.confidence,
                        (DIGITIZER, USAGE_CONTACT_ID) => &mut ff.contact_id,
                        (DIGITIZER, USAGE_TIP_PRESSURE) => &mut ff.pressure,
                        (DIGITIZER, USAGE_WIDTH) => &mut ff.width,
                        (DIGITIZER, USAGE_HEIGHT) => &mut ff.height,
                        (GENERIC_DESKTOP, USAGE_X) => &mut ff.x,
                        (GENERIC_DESKTOP, USAGE_Y) => &mut ff.y,
                        _ => continue,
                    };
                    if slot.is_none() {
                        *slot = Some(f.clone());
                    }
                }
                None => match (f.usage_page, f.usage) {
                    (BUTTON, _) if !f.constant => ptp.buttons.push(f.clone()),
                    (DIGITIZER, USAGE_CONTACT_COUNT) => ptp.contact_count = Some(f.clone()),
                    (DIGITIZER, USAGE_SCAN_TIME) => ptp.scan_time = Some(f.clone()),
                    _ => {}
                },
            }
        }

        if ptp.fingers.is_empty() {
            return None;
        }

        ptp.contact_count_max = layout
            .find(ReportKind::Feature, DIGITIZER, USAGE_CONTACT_COUNT_MAX)
            .map(|f| f.logical_max.max(0) as usize)
            .filter(|&n| n > 0)
            .unwrap_or(ptp.fingers.len());
        ptp.x_max = ptp.fingers[0]
            .x
            .as_ref()
            .map(|f| f.logical_max)
            .unwrap_or(0);
        ptp.y_max = ptp.fingers[0]
            .y
            .as_ref()
            .map(|f| f.logical_max)
            .unwrap_or(0);
        if let Some(f) = &ptp.scan_time {
            ptp.scan_time_unit_us = scan_time_unit_us(f);
        }

        Some(ptp)
    }
}

const DEFAULT_SCAN_TIME_UNIT_US: f64 = 100.0;

/// Microseconds per tick of a Scan Time field. HID Unit nibble 0 is the
/// system and nibble 3 the exponent of the time dimension (seconds in every
/// system); `0x1001` with Unit Exponent `-4` is the usual 100 µs.
fn scan_time_unit_us(f: &ReportField) -> f64 {
    let system = f.unit & 0xF;
    let time_dim = decode_unit_exponent(((f.unit >> 12) & 0xF) as i32);
    if system == 0 || time_dim != 1 {
        return DEFAULT_SCAN_TIME_UNIT_US;
    }
    10f64.powi(decode_unit_exponent(f.unit_exponent) + 6)
}

/// Finger-level usages: one of these repeating inside a report means the
/// next finger's fields have begun.
fn is_finger_usage(usage_page: u16, usage: u16) -> bool {
    matches!(
        (usage_page, usage),
        (DIGITIZER, USAGE_TIP_SWITCH)
            | (DIGITIZER, USAGE_CONFIDENCE)
            | (DIGITIZER, USAGE_CONTACT_ID)
            | (DIGITIZER, USAGE_TIP_PRESSURE)
            | (DIGITIZER, USAGE_IN_RANGE)
            | (DIGITIZER, USAGE_WIDTH)
            | (DIGITIZER, USAGE_HEIGHT)
            | (GENERIC_DESKTOP, USAGE_X)
            | (GENERIC_DESKTOP, USAGE_Y)
    )
}

/// Put back the per-finger Logical collections a layout has lost.
///
/// A browser's parsed `HIDDevice.collections` lists every report item on its
/// top-level collection, in descriptor order, with no trace of the nested
/// Finger collections — yet [`PtpLayout::from_layout`] tells fingers apart by
/// exactly those. So: walking each Input report's finger-level fields (tip
/// switch, contact id, X, Y, ...) that are not already inside a Finger
/// collection, a new synthetic Finger collection begins whenever a usage
/// repeats. Padding and report-level fields (contact count, scan time,
/// buttons) are left where they are. A layout parsed from raw descriptor
/// bytes has its Finger collections and comes back unchanged.
pub fn synthesize_finger_collections(layout: &mut ReportLayout) {
    /// The synthetic Finger collection being filled.
    struct Group {
        owner: usize,
        report_id: u8,
        collection: usize,
        seen: Vec<(u16, u16)>,
    }
    let mut current: Option<Group> = None;
    for i in 0..layout.fields.len() {
        let f = layout.fields[i].clone();
        if f.kind != ReportKind::Input || !is_finger_usage(f.usage_page, f.usage) {
            continue;
        }
        let Some(owner) = f.collection else {
            continue;
        };
        // Only digitizer reports: a mouse collection has X/Y too.
        let touch = layout.is_inside(&f, DIGITIZER, USAGE_TOUCH_PAD)
            || layout.is_inside(&f, DIGITIZER, USAGE_TOUCH_SCREEN);
        if !touch || layout.is_inside(&f, DIGITIZER, USAGE_FINGER) {
            continue;
        }
        let key = (f.usage_page, f.usage);
        let start_new = match &current {
            Some(g) => g.owner != owner || g.report_id != f.report_id || g.seen.contains(&key),
            None => true,
        };
        if start_new {
            layout.collections.push(Collection {
                parent: Some(owner),
                kind: 2, // Logical, as PTP descriptors declare them
                usage_page: DIGITIZER,
                usage: USAGE_FINGER,
            });
            current = Some(Group {
                owner,
                report_id: f.report_id,
                collection: layout.collections.len() - 1,
                seen: Vec::new(),
            });
        }
        let group = current.as_mut().expect("just set");
        group.seen.push(key);
        layout.fields[i].collection = Some(group.collection);
    }
}

fn field_value(payload: &[u8], f: &ReportField) -> i32 {
    if f.logical_min < 0 {
        extract_signed(payload, f.bit_offset, f.bit_size)
    } else {
        extract_bits(payload, f.bit_offset, f.bit_size) as i32
    }
}

fn opt_value(payload: &[u8], f: &Option<ReportField>) -> Option<i32> {
    f.as_ref().map(|f| field_value(payload, f))
}

/// Stateful report-to-frame converter: keeps the slot table between reports
/// and reassembles hybrid-mode frames.
pub struct PtpParser {
    layout: PtpLayout,
    slots: [TouchData; MAX_TOUCH_POINTS],
    /// Contact ID currently occupying each slot.
    slot_ids: [Option<u32>; MAX_TOUCH_POINTS],
    /// Slots that appeared in the frame being assembled.
    touched: [bool; MAX_TOUCH_POINTS],
    next_tracking_id: i32,
    buttons: ButtonState,
    /// Hybrid-mode frame assembly: contacts announced, contacts seen so far.
    expected: usize,
    received: usize,
    in_frame: bool,
    /// Scan Time unwrapping: the last raw value, and the ticks since the
    /// first report.
    last_scan_raw: Option<u32>,
    scan_ticks: u64,
    /// The current frame's Scan Time in microseconds.
    scan_time_us: Option<u64>,
}

impl PtpParser {
    pub fn new(layout: PtpLayout) -> Self {
        Self {
            layout,
            slots: [TouchData::default(); MAX_TOUCH_POINTS],
            slot_ids: [None; MAX_TOUCH_POINTS],
            touched: [false; MAX_TOUCH_POINTS],
            next_tracking_id: 0,
            buttons: ButtonState::default(),
            expected: 0,
            received: 0,
            in_frame: false,
            last_scan_raw: None,
            scan_ticks: 0,
            scan_time_us: None,
        }
    }

    pub fn layout(&self) -> &PtpLayout {
        &self.layout
    }

    /// Feed one report as read from the device (including the report-ID byte
    /// when the layout is numbered). Reports with another ID (e.g. the Mouse
    /// collection's) are ignored. Returns a frame when one is complete.
    pub fn feed(&mut self, report: &[u8]) -> Option<TouchState> {
        let payload = if self.layout.report_id != 0 {
            if report.first() != Some(&self.layout.report_id) {
                return None;
            }
            &report[1..]
        } else {
            report
        };
        if payload.len() < self.layout.report_bytes {
            return None;
        }

        for b in &self.layout.buttons {
            let down = field_value(payload, b) != 0;
            match b.usage {
                1 => self.buttons.left = down,
                2 => self.buttons.right = down,
                3 => self.buttons.middle = down,
                _ => {}
            }
        }

        let count = opt_value(payload, &self.layout.contact_count).map(|c| c.max(0) as usize);
        let starts_frame = match count {
            // A count starts a frame; a partial one still in progress is dropped.
            Some(c) if c > 0 => {
                if self.in_frame {
                    log::warn!("ptp: new frame started before the previous one completed");
                }
                Some(c)
            }
            // Count 0 mid-frame: hybrid continuation. Count 0 otherwise: an
            // empty frame (all contacts lifted).
            Some(_) if self.in_frame => None,
            Some(_) => Some(0),
            // No Contact Count field: every report is a whole frame.
            None => Some(self.layout.fingers.len()),
        };
        if let Some(expected) = starts_frame {
            self.start_frame(expected);
            self.read_scan_time(payload);
        }

        let take = self
            .layout
            .fingers
            .len()
            .min(self.expected.saturating_sub(self.received));
        for i in 0..take {
            self.apply_finger(payload, i);
        }
        self.received += take;

        if self.received >= self.expected {
            Some(self.finish_frame())
        } else {
            None
        }
    }

    fn start_frame(&mut self, expected: usize) {
        self.expected = expected;
        self.received = 0;
        self.touched = [false; MAX_TOUCH_POINTS];
        self.in_frame = true;
    }

    /// Unwrap the frame's Scan Time (the field is typically 16 bits, so it
    /// wraps every 6.5 s) into a clock that only moves forward.
    fn read_scan_time(&mut self, payload: &[u8]) {
        let Some(f) = &self.layout.scan_time else {
            return;
        };
        let raw = extract_bits(payload, f.bit_offset, f.bit_size);
        if let Some(last) = self.last_scan_raw {
            let mask = if f.bit_size >= 32 {
                u32::MAX
            } else {
                (1u32 << f.bit_size) - 1
            };
            self.scan_ticks += (raw.wrapping_sub(last) & mask) as u64;
        }
        self.last_scan_raw = Some(raw);
        self.scan_time_us = Some((self.scan_ticks as f64 * self.layout.scan_time_unit_us) as u64);
    }

    fn apply_finger(&mut self, payload: &[u8], i: usize) {
        let ff = &self.layout.fingers[i];
        let tip = opt_value(payload, &ff.tip_switch)
            .map(|v| v != 0)
            .unwrap_or(true);
        let confidence = opt_value(payload, &ff.confidence)
            .map(|v| v != 0)
            .unwrap_or(true);
        let id = opt_value(payload, &ff.contact_id).unwrap_or(i as i32) as u32;

        // input_mt_get_slot_by_key: the slot already tracking this id, else
        // (for a contact that is down) the first free slot not yet used in
        // this frame.
        let existing =
            (0..MAX_TOUCH_POINTS).find(|&s| self.slots[s].used && self.slot_ids[s] == Some(id));
        let slot = match existing {
            Some(s) => s,
            None if tip => {
                match (0..MAX_TOUCH_POINTS).find(|&s| !self.slots[s].used && !self.touched[s]) {
                    Some(s) => s,
                    None => return, // more contacts than slots
                }
            }
            None => return, // a lifted contact we never tracked
        };

        if !tip {
            self.slots[slot] = TouchData::default();
            self.slot_ids[slot] = None;
            return;
        }

        let t = &mut self.slots[slot];
        if !t.used {
            *t = TouchData::default();
            t.tracking_id = self.next_tracking_id;
            self.next_tracking_id += 1;
            t.used = true;
            self.slot_ids[slot] = Some(id);
        }
        self.touched[slot] = true;
        t.position_x = opt_value(payload, &ff.x).unwrap_or(t.position_x);
        t.position_y = opt_value(payload, &ff.y).unwrap_or(t.position_y);
        t.pressure = opt_value(payload, &ff.pressure).unwrap_or(0);
        let w = opt_value(payload, &ff.width).unwrap_or(0);
        let h = opt_value(payload, &ff.height).unwrap_or(0);
        t.touch_major = w.max(h);
        t.touch_minor = w.min(h);
        t.orientation = (w > h) as i32;
        t.tool_type = if confidence { 0 } else { MT_TOOL_PALM };
    }

    fn finish_frame(&mut self) -> TouchState {
        // INPUT_MT_DROP_UNUSED: contacts not reported this frame are gone.
        for s in 0..MAX_TOUCH_POINTS {
            if self.slots[s].used && !self.touched[s] {
                self.slots[s] = TouchData::default();
                self.slot_ids[s] = None;
            }
        }
        self.in_frame = false;

        // Pointer emulation as the kernel does it, carried on slot 0 like the
        // evdev state machine's BTN_TOUCH / BTN_TOOL_DOUBLETAP handling.
        let active = self.slots.iter().filter(|t| t.used).count();
        let mut touches = self.slots;
        for t in &mut touches {
            t.pressed = false;
            t.pressed_double = false;
        }
        touches[0].pressed = active > 0;
        touches[0].pressed_double = active == 2;

        TouchState {
            touches,
            buttons: self.buttons,
            // Stamped when the report completing the frame was fed, i.e.
            // as the backend received it.
            timestamp_us: Some(host_now_us()),
            scan_time_us: self.scan_time_us,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hid::insert_bits;

    const FRAMEWORK13: &[u8] = include_bytes!("../testdata/ptp/framework13_pixa3854.desc");

    fn framework_layout() -> PtpLayout {
        PtpLayout::from_layout(&ReportLayout::parse(FRAMEWORK13)).unwrap()
    }

    /// What a browser hands over: the same fields, in order, but every one
    /// attached to its top-level collection — the Finger collections gone.
    fn flattened(layout: &ReportLayout) -> ReportLayout {
        // Keep only top-level collections, renumbered densely.
        let mut new_index = vec![None; layout.collections.len()];
        let mut collections = Vec::new();
        for (i, c) in layout.collections.iter().enumerate() {
            if c.parent.is_none() {
                new_index[i] = Some(collections.len());
                collections.push(c.clone());
            }
        }
        let top_level = |mut idx: usize| {
            while let Some(parent) = layout.collections[idx].parent {
                idx = parent;
            }
            new_index[idx].unwrap()
        };
        let fields = layout
            .fields
            .iter()
            .cloned()
            .map(|mut f| {
                f.collection = f.collection.map(top_level);
                f
            })
            .collect();
        ReportLayout::from_parts(fields, collections)
    }

    #[test]
    fn synthesized_finger_collections_match_the_descriptor() {
        let original = ReportLayout::parse(FRAMEWORK13);
        let expected = PtpLayout::from_layout(&original).unwrap();

        let mut flat = flattened(&original);
        assert!(
            PtpLayout::from_layout(&flat).is_none(),
            "the flattened layout must have lost its fingers"
        );
        synthesize_finger_collections(&mut flat);
        let rebuilt = PtpLayout::from_layout(&flat).unwrap();

        assert_eq!(rebuilt.report_id, expected.report_id);
        assert_eq!(rebuilt.report_bytes, expected.report_bytes);
        assert_eq!(rebuilt.fingers.len(), expected.fingers.len());
        assert_eq!(
            (rebuilt.x_max, rebuilt.y_max),
            (expected.x_max, expected.y_max)
        );
        assert_eq!(rebuilt.contact_count_max, expected.contact_count_max);
        let pos = |f: &Option<ReportField>| f.as_ref().map(|f| (f.bit_offset, f.bit_size));
        for (r, e) in rebuilt.fingers.iter().zip(&expected.fingers) {
            assert_eq!(pos(&r.tip_switch), pos(&e.tip_switch));
            assert_eq!(pos(&r.confidence), pos(&e.confidence));
            assert_eq!(pos(&r.contact_id), pos(&e.contact_id));
            assert_eq!(pos(&r.x), pos(&e.x));
            assert_eq!(pos(&r.y), pos(&e.y));
            assert_eq!(pos(&r.width), pos(&e.width));
            assert_eq!(pos(&r.height), pos(&e.height));
        }
        assert_eq!(pos(&rebuilt.contact_count), pos(&expected.contact_count));
        assert_eq!(pos(&rebuilt.scan_time), pos(&expected.scan_time));
        assert_eq!(rebuilt.buttons.len(), expected.buttons.len());

        // Idempotent, and a no-op on a layout that still has its collections.
        let before = flat.fields.clone();
        synthesize_finger_collections(&mut flat);
        assert_eq!(flat.fields, before);
        let mut intact = original.clone();
        synthesize_finger_collections(&mut intact);
        assert_eq!(intact.fields, original.fields);
        assert_eq!(intact.collections, original.collections);
    }

    /// Set a field in a report payload (report-ID byte at index 0 excluded);
    /// no-op for fields the descriptor does not have.
    fn put(payload: &mut [u8], f: &Option<ReportField>, value: u32) {
        if let Some(f) = f {
            insert_bits(payload, f.bit_offset, f.bit_size, value);
        }
    }

    /// Build a numbered report: ID byte + payload with the given contacts
    /// `(id, x, y, tip, confidence)` in finger order and the contact count.
    fn report(
        layout: &PtpLayout,
        count: u32,
        contacts: &[(u32, i32, i32, bool, bool)],
        button: bool,
    ) -> Vec<u8> {
        let mut payload = vec![0u8; layout.report_bytes];
        put(&mut payload, &layout.contact_count, count);
        if let Some(b) = layout.buttons.first() {
            insert_bits(&mut payload, b.bit_offset, b.bit_size, button as u32);
        }
        for (i, &(id, x, y, tip, conf)) in contacts.iter().enumerate() {
            let ff = &layout.fingers[i];
            put(&mut payload, &ff.contact_id, id);
            put(&mut payload, &ff.x, x as u32);
            put(&mut payload, &ff.y, y as u32);
            put(&mut payload, &ff.tip_switch, tip as u32);
            put(&mut payload, &ff.confidence, conf as u32);
        }
        let mut r = vec![layout.report_id];
        r.extend(payload);
        r
    }

    /// The report with its Scan Time field set to `ticks`.
    fn with_scan_time(layout: &PtpLayout, mut report: Vec<u8>, ticks: u32) -> Vec<u8> {
        put(&mut report[1..], &layout.scan_time, ticks);
        report
    }

    #[test]
    fn scan_time_unit_from_descriptor() {
        let l = framework_layout();
        // Seconds, exponent -4: the PTP spec's 100 µs.
        let f = l.scan_time.as_ref().unwrap();
        assert_eq!(f.unit & 0xF, 1, "SI linear");
        assert_eq!((f.unit >> 12) & 0xF, 1, "seconds^1");
        assert_eq!(l.scan_time_unit_us, 100.0);

        let mut ms = f.clone();
        ms.unit_exponent = 0x0D; // -3
        assert_eq!(scan_time_unit_us(&ms), 1000.0);
        let mut none = f.clone();
        none.unit = 0;
        assert_eq!(scan_time_unit_us(&none), 100.0);
    }

    #[test]
    fn scan_time_unwraps_and_stamps_frames() {
        let l = framework_layout();
        let mut p = PtpParser::new(l.clone());
        let down = |ticks| {
            with_scan_time(
                &l,
                report(&l, 1, &[(7, 100, 100, true, true)], false),
                ticks,
            )
        };

        // The first frame anchors the clock at 0, right before the 16-bit wrap.
        let s = p.feed(&down(0xFFF0)).unwrap();
        assert_eq!(s.scan_time_us, Some(0));
        assert!(s.timestamp_us.is_some());
        // +80 ticks across the wrap = 8 ms
        let s = p.feed(&down(0x0040)).unwrap();
        assert_eq!(s.scan_time_us, Some(8_000));
        let s = p.feed(&down(0x00B0)).unwrap();
        assert_eq!(s.scan_time_us, Some(19_200));

        // A hybrid frame takes the scan time of its first report.
        let l = PtpLayout::from_layout(&ReportLayout::parse(&hybrid_descriptor())).unwrap();
        let mut p = PtpParser::new(l.clone());
        let first = with_scan_time(
            &l,
            report(
                &l,
                3,
                &[(1, 10, 10, true, true), (2, 20, 20, true, true)],
                false,
            ),
            10,
        );
        assert!(p.feed(&first).is_none());
        let second = with_scan_time(&l, report(&l, 0, &[(3, 30, 30, true, true)], false), 999);
        let s = p.feed(&second).unwrap();
        assert_eq!(used(&s), vec![0, 1, 2]);
        assert_eq!(s.scan_time_us, Some(0));
        let next = with_scan_time(&l, report(&l, 1, &[(1, 11, 11, true, true)], false), 90);
        assert_eq!(p.feed(&next).unwrap().scan_time_us, Some(8_000));
    }

    fn used(state: &TouchState) -> Vec<usize> {
        (0..MAX_TOUCH_POINTS)
            .filter(|&s| state.touches[s].used)
            .collect()
    }

    #[test]
    fn layout_from_framework_descriptor() {
        let l = framework_layout();
        assert_eq!(l.report_id, 1);
        assert_eq!(l.report_bytes, 28);
        assert_eq!(l.fingers.len(), 5);
        assert_eq!(l.contact_count_max, 5);
        assert_eq!((l.x_max, l.y_max), (2833, 1723));
        let cc = l.contact_count.as_ref().unwrap();
        assert_eq!((cc.bit_offset, cc.bit_size), (4, 4));
        assert!(l.scan_time.is_some());
        assert_eq!(l.buttons.len(), 1);
        assert_eq!(l.buttons[0].usage, 1);
        let f0 = &l.fingers[0];
        assert_eq!(f0.confidence.as_ref().unwrap().bit_offset, 24);
        assert_eq!(f0.tip_switch.as_ref().unwrap().bit_offset, 25);
        assert_eq!(f0.contact_id.as_ref().unwrap().bit_offset, 28);
        assert_eq!(f0.x.as_ref().unwrap().bit_offset, 32);
        assert_eq!(f0.y.as_ref().unwrap().bit_offset, 48);
        assert!(f0.width.is_none() && f0.pressure.is_none());
        // Second finger is 40 bits further along
        assert_eq!(l.fingers[1].x.as_ref().unwrap().bit_offset, 72);
    }

    #[test]
    fn single_report_frames() {
        let l = framework_layout();
        let mut p = PtpParser::new(l.clone());

        // Other report IDs (the Mouse report) are ignored
        assert!(p.feed(&[2, 0, 0, 0, 0]).is_none());
        // Short reports are ignored
        assert!(p.feed(&[1, 0, 0]).is_none());

        // One finger down
        let s = p
            .feed(&report(&l, 1, &[(7, 1000, 500, true, true)], false))
            .unwrap();
        assert_eq!(used(&s), vec![0]);
        assert_eq!(
            (s.touches[0].position_x, s.touches[0].position_y),
            (1000, 500)
        );
        assert_eq!(s.touches[0].tracking_id, 0);
        assert_eq!(s.touches[0].tool_type, 0);
        assert!(s.touches[0].pressed && !s.touches[0].pressed_double);
        assert!(!s.buttons.left);

        // Second finger joins, first moves
        let s = p
            .feed(&report(
                &l,
                2,
                &[(7, 1010, 510, true, true), (9, 2000, 900, true, true)],
                true,
            ))
            .unwrap();
        assert_eq!(used(&s), vec![0, 1]);
        assert_eq!(s.touches[0].position_x, 1010);
        assert_eq!(s.touches[1].tracking_id, 1);
        assert!(s.touches[0].pressed_double);
        assert!(s.buttons.left);

        // First finger lifts (reported with tip = 0), second stays and moves
        let s = p
            .feed(&report(
                &l,
                2,
                &[(7, 1010, 510, false, true), (9, 2100, 950, true, true)],
                false,
            ))
            .unwrap();
        assert_eq!(used(&s), vec![1]);
        assert_eq!(s.touches[1].tracking_id, 1, "slot kept its tracking id");
        assert_eq!(s.touches[1].position_x, 2100);
        assert!(s.touches[0].pressed && !s.touches[0].pressed_double);
        assert!(!s.buttons.left);

        // A contact simply missing from the next frame is dropped too
        let s = p
            .feed(&report(&l, 1, &[(11, 5, 5, true, false)], false))
            .unwrap();
        assert_eq!(used(&s), vec![0], "lowest free slot reused");
        assert_eq!(s.touches[0].tracking_id, 2);
        assert_eq!(s.touches[0].tool_type, MT_TOOL_PALM, "confidence 0 = palm");

        // Empty frame: everything lifted
        let s = p.feed(&report(&l, 0, &[], false)).unwrap();
        assert!(used(&s).is_empty());
        assert!(!s.touches[0].pressed);
    }

    /// Two Finger collections but up to four contacts: frames span reports.
    fn hybrid_descriptor() -> Vec<u8> {
        let mut d: Vec<u8> = vec![
            0x05, 0x0D, // Usage Page (Digitizer)
            0x09, 0x05, // Usage (Touch Pad)
            0xA1, 0x01, // Collection (Application)
            0x85, 0x04, //   Report ID (4)
            0x09, 0x54, //   Usage (Contact Count)
            0x15, 0x00, 0x25, 0x0F, //   Logical 0..15
            0x75, 0x04, 0x95, 0x01, //   4 bits x 1
            0x81, 0x02, //   Input
            0x75, 0x04, 0x81, 0x03, //   4 bits padding
        ];
        for _ in 0..2 {
            d.extend_from_slice(&[
                0x09, 0x22, //   Usage (Finger)
                0xA1, 0x02, //   Collection (Logical)
                0x09, 0x42, //     Usage (Tip Switch)
                0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02, 0x75, 0x03, 0x81,
                0x03, //     3 bits padding
                0x09, 0x51, //     Usage (Contact ID)
                0x25, 0x0F, 0x75, 0x04, 0x95, 0x01, 0x81, 0x02, 0x05,
                0x01, //     Usage Page (Generic Desktop)
                0x09, 0x30, //     Usage (X)
                0x26, 0xFF, 0x0F, 0x75, 0x10, 0x95, 0x01, 0x81, 0x02, 0x09,
                0x31, //     Usage (Y)
                0x81, 0x02, 0x05, 0x0D, //     Usage Page (Digitizer)
                0xC0, //   End Collection
            ]);
        }
        d.extend_from_slice(&[
            0x09, 0x56, //   Usage (Scan Time)
            0x55, 0x0C, //   Unit Exponent (-4)
            0x66, 0x01, 0x10, //   Unit (seconds)
            0x27, 0xFF, 0xFF, 0x00, 0x00, //   Logical Max 65535
            0x75, 0x10, 0x95, 0x01, //   16 bits x 1
            0x81, 0x02, //   Input
            0x55, 0x00, 0x65, 0x00, //   Unit Exponent 0, Unit none
            0x85, 0x05, //   Report ID (5)
            0x09, 0x55, //   Usage (Contact Count Maximum)
            0x25, 0x04, 0x75, 0x08, 0x95, 0x01, //   value 4
            0xB1, 0x02, //   Feature
            0xC0, // End Collection
        ]);
        d
    }

    #[test]
    fn hybrid_mode_reassembles_frames() {
        let l = PtpLayout::from_layout(&ReportLayout::parse(&hybrid_descriptor())).unwrap();
        assert_eq!(l.report_id, 4);
        assert_eq!(l.fingers.len(), 2);
        assert_eq!(l.contact_count_max, 4);
        assert_eq!(l.report_bytes, 1 + 2 * 5 + 2);
        assert!(l.buttons.is_empty());
        assert_eq!(l.scan_time_unit_us, 100.0);

        let mut p = PtpParser::new(l.clone());
        // Three contacts: first report announces 3 and carries two
        let r1 = report(
            &l,
            3,
            &[(0, 10, 10, true, true), (1, 20, 20, true, true)],
            false,
        );
        assert!(p.feed(&r1).is_none(), "frame incomplete");
        // Continuation: count 0, third contact plus a stale fourth slot
        let r2 = report(
            &l,
            0,
            &[(2, 30, 30, true, true), (3, 99, 99, true, true)],
            false,
        );
        let s = p.feed(&r2).unwrap();
        assert_eq!(used(&s), vec![0, 1, 2], "stale fourth finger ignored");
        assert_eq!(s.touches[2].position_x, 30);
        assert!(s.touches[0].pressed && !s.touches[0].pressed_double);

        // Next frame: one contact remains, one report
        let s = p
            .feed(&report(&l, 1, &[(1, 21, 21, true, true)], false))
            .unwrap();
        assert_eq!(used(&s), vec![1]);
        assert_eq!(s.touches[1].tracking_id, 1);
    }

    #[test]
    fn no_touchpad_collection() {
        let mouse_only: Vec<u8> = vec![
            0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x30, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02,
            0xC0,
        ];
        assert!(PtpLayout::from_layout(&ReportLayout::parse(&mouse_only)).is_none());
    }
}
