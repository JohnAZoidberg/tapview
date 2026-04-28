use super::{
    AxisPhysicalInfo, ConfigBackend, ConfigValues, PtpConfig, PtpFeatures, TouchpadPhysicalSize,
    ValueRange,
};
use crate::heatmap::discovery::find_sibling_hidraw;
use crate::heatmap::hidraw::HidrawDevice;
use crate::heatmap::HidDevice;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

// PTP usage IDs on Usage Page 0x0D (Digitizer)
const USAGE_INPUT_MODE: u16 = 0x0052;
const USAGE_CONTACT_COUNT_MAX: u16 = 0x0055;
const USAGE_SURFACE_SWITCH: u16 = 0x0057;
const USAGE_BUTTON_SWITCH: u16 = 0x0058;
const USAGE_PAD_TYPE: u16 = 0x0059;
const USAGE_LATENCY_MODE: u16 = 0x0060;
const USAGE_BUTTON_PRESS_THRESHOLD: u16 = 0x00B0;

const DIGITIZER_PAGE: u16 = 0x000D;
const HAPTIC_PAGE: u16 = 0x000E;
const USAGE_HAPTIC_INTENSITY: u16 = 0x0023;

/// Key combining usage page and usage ID, so usages on different pages
/// (e.g. Digitizer 0xB0 vs Haptic 0x23) don't collide in the field table.
type FieldKey = (u16, u16);
const KEY_INPUT_MODE: FieldKey = (DIGITIZER_PAGE, USAGE_INPUT_MODE);
const KEY_CONTACT_COUNT_MAX: FieldKey = (DIGITIZER_PAGE, USAGE_CONTACT_COUNT_MAX);
const KEY_SURFACE_SWITCH: FieldKey = (DIGITIZER_PAGE, USAGE_SURFACE_SWITCH);
const KEY_BUTTON_SWITCH: FieldKey = (DIGITIZER_PAGE, USAGE_BUTTON_SWITCH);
const KEY_PAD_TYPE: FieldKey = (DIGITIZER_PAGE, USAGE_PAD_TYPE);
const KEY_LATENCY_MODE: FieldKey = (DIGITIZER_PAGE, USAGE_LATENCY_MODE);
const KEY_BUTTON_PRESS_THRESHOLD: FieldKey = (DIGITIZER_PAGE, USAGE_BUTTON_PRESS_THRESHOLD);
const KEY_HAPTIC_INTENSITY: FieldKey = (HAPTIC_PAGE, USAGE_HAPTIC_INTENSITY);

#[derive(Debug, Clone)]
struct FeatureField {
    report_id: u8,
    bit_offset: usize,
    bit_size: usize,
    read_only: bool,
    logical_min: i32,
    logical_max: i32,
    physical_min: i32,
    physical_max: i32,
}

impl FeatureField {
    fn range(&self) -> ValueRange {
        let physical = if self.physical_min != self.physical_max
            && (self.physical_min, self.physical_max) != (self.logical_min, self.logical_max)
        {
            Some((self.physical_min, self.physical_max))
        } else {
            None
        };
        ValueRange {
            logical_min: self.logical_min,
            logical_max: self.logical_max,
            physical,
        }
    }
}

struct LinuxConfigBackend {
    device: HidrawDevice,
    fields: HashMap<FieldKey, FeatureField>,
    report_sizes: HashMap<u8, usize>, // report_id -> byte count (excluding report ID byte)
}

impl LinuxConfigBackend {
    fn read_field(&self, key: FieldKey) -> Option<u32> {
        let field = self.fields.get(&key)?;
        let report_byte_size = self
            .report_sizes
            .get(&field.report_id)
            .copied()
            .unwrap_or(0);
        let mut buf = vec![0u8; 1 + report_byte_size];
        buf[0] = field.report_id;
        self.device.get_feature(&mut buf).ok()?;
        Some(extract_bits(&buf[1..], field.bit_offset, field.bit_size))
    }

    fn write_field(&self, key: FieldKey, value: u32) -> io::Result<()> {
        let field = self
            .fields
            .get(&key)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "feature field not found"))?;
        let report_byte_size = self
            .report_sizes
            .get(&field.report_id)
            .copied()
            .unwrap_or(0);
        let mut buf = vec![0u8; 1 + report_byte_size];
        buf[0] = field.report_id;
        // Read-modify-write
        self.device.get_feature(&mut buf)?;
        insert_bits(&mut buf[1..], field.bit_offset, field.bit_size, value);
        self.device.set_feature(&buf)
    }
}

impl ConfigBackend for LinuxConfigBackend {
    fn read_all(&mut self) -> ConfigValues {
        ConfigValues {
            input_mode: self.read_field(KEY_INPUT_MODE).map(|v| v as u8),
            surface_switch: self.read_field(KEY_SURFACE_SWITCH).map(|v| v != 0),
            button_switch: self.read_field(KEY_BUTTON_SWITCH).map(|v| v != 0),
            contact_count_max: self.read_field(KEY_CONTACT_COUNT_MAX).map(|v| v as u8),
            pad_type: self.read_field(KEY_PAD_TYPE).map(|v| v as u8),
            latency_mode: self.read_field(KEY_LATENCY_MODE).map(|v| v != 0),
            button_press_threshold: self.read_field(KEY_BUTTON_PRESS_THRESHOLD).map(|v| v as u8),
            haptic_intensity: self.read_field(KEY_HAPTIC_INTENSITY).map(|v| v as u8),
        }
    }

    fn write_input_mode(&mut self, value: u8) -> io::Result<()> {
        self.write_field(KEY_INPUT_MODE, value as u32)
    }

    fn write_selective_reporting(&mut self, surface: bool, button: bool) -> io::Result<()> {
        if self.fields.contains_key(&KEY_SURFACE_SWITCH) {
            self.write_field(KEY_SURFACE_SWITCH, surface as u32)?;
        }
        if self.fields.contains_key(&KEY_BUTTON_SWITCH) {
            self.write_field(KEY_BUTTON_SWITCH, button as u32)?;
        }
        Ok(())
    }

    fn write_latency_mode(&mut self, high: bool) -> io::Result<()> {
        self.write_field(KEY_LATENCY_MODE, high as u32)
    }

    fn write_button_press_threshold(&mut self, value: u8) -> io::Result<()> {
        self.write_field(KEY_BUTTON_PRESS_THRESHOLD, value as u32)
    }

    fn write_haptic_intensity(&mut self, value: u8) -> io::Result<()> {
        self.write_field(KEY_HAPTIC_INTENSITY, value as u32)
    }
}

// ── Bit manipulation ──────────────────────────────────────────────────────────

fn extract_bits(data: &[u8], bit_offset: usize, bit_size: usize) -> u32 {
    if bit_size == 0 {
        return 0;
    }
    let byte_offset = bit_offset / 8;
    let bit_shift = bit_offset % 8;
    let bytes_needed = (bit_shift + bit_size).div_ceil(8);
    let mut value: u32 = 0;
    for i in 0..bytes_needed {
        if byte_offset + i < data.len() {
            value |= (data[byte_offset + i] as u32) << (i * 8);
        }
    }
    (value >> bit_shift) & ((1u32 << bit_size) - 1)
}

fn insert_bits(data: &mut [u8], bit_offset: usize, bit_size: usize, value: u32) {
    if bit_size == 0 {
        return;
    }
    let byte_offset = bit_offset / 8;
    let bit_shift = bit_offset % 8;
    let mask = ((1u32 << bit_size) - 1) << bit_shift;
    let shifted_value = (value & ((1u32 << bit_size) - 1)) << bit_shift;
    let bytes_needed = (bit_shift + bit_size).div_ceil(8);
    for i in 0..bytes_needed {
        if byte_offset + i < data.len() {
            let byte_mask = (mask >> (i * 8)) as u8;
            let byte_val = (shifted_value >> (i * 8)) as u8;
            data[byte_offset + i] = (data[byte_offset + i] & !byte_mask) | (byte_val & byte_mask);
        }
    }
}

// ── HID descriptor parser ────────────────────────────────────────────────────

fn recognize_field(usage_page: u16, usage: u16) -> Option<FieldKey> {
    match (usage_page, usage) {
        (DIGITIZER_PAGE, USAGE_INPUT_MODE)
        | (DIGITIZER_PAGE, USAGE_CONTACT_COUNT_MAX)
        | (DIGITIZER_PAGE, USAGE_SURFACE_SWITCH)
        | (DIGITIZER_PAGE, USAGE_BUTTON_SWITCH)
        | (DIGITIZER_PAGE, USAGE_PAD_TYPE)
        | (DIGITIZER_PAGE, USAGE_LATENCY_MODE)
        | (DIGITIZER_PAGE, USAGE_BUTTON_PRESS_THRESHOLD)
        | (HAPTIC_PAGE, USAGE_HAPTIC_INTENSITY) => Some((usage_page, usage)),
        _ => None,
    }
}

/// Parse HID report descriptor to find PTP / haptic feature fields.
/// Returns (key -> FeatureField map, report_id -> report byte size map).
fn parse_ptp_features(desc: &[u8]) -> (HashMap<FieldKey, FeatureField>, HashMap<u8, usize>) {
    let mut fields: HashMap<FieldKey, FeatureField> = HashMap::new();

    // Global state
    let mut usage_page: u16 = 0;
    let mut report_id: u8 = 0;
    let mut report_size: u32 = 0;
    let mut report_count: u32 = 0;
    let mut logical_min: i32 = 0;
    let mut logical_max: i32 = 0;
    let mut physical_min: i32 = 0;
    let mut physical_max: i32 = 0;

    // Local state (cleared after each main item)
    let mut usages: Vec<u16> = Vec::new();

    // Per-report-id bit offset tracking for Feature reports
    let mut feature_bit_offsets: HashMap<u8, usize> = HashMap::new();

    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];

        // Long item
        if prefix == 0xFE {
            if i + 2 >= desc.len() {
                break;
            }
            let data_size = desc[i + 1] as usize;
            i += 3 + data_size;
            continue;
        }

        // Short item
        let size = match prefix & 0x03 {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 4,
            _ => unreachable!(),
        };

        if i + 1 + size > desc.len() {
            break;
        }

        let tag = prefix & 0xFC;
        let data = &desc[i + 1..i + 1 + size];

        match tag {
            // Usage Page (Global)
            0x04 => {
                usage_page = read_unsigned(data, size) as u16;
            }
            // Usage (Local)
            0x08 => {
                usages.push(read_unsigned(data, size) as u16);
            }
            // Logical Minimum (Global)
            0x14 => {
                logical_min = read_signed(data, size);
            }
            // Logical Maximum (Global)
            0x24 => {
                logical_max = read_signed(data, size);
            }
            // Physical Minimum (Global)
            0x34 => {
                physical_min = read_signed(data, size);
            }
            // Physical Maximum (Global)
            0x44 => {
                physical_max = read_signed(data, size);
            }
            // Report Size (Global)
            0x74 => {
                report_size = read_unsigned(data, size);
            }
            // Report ID (Global)
            0x84 => {
                if let Some(&id) = data.first() {
                    report_id = id;
                }
            }
            // Report Count (Global)
            0x94 => {
                report_count = read_unsigned(data, size);
            }
            // Feature (Main)
            0xB0 => {
                let base_offset = *feature_bit_offsets.entry(report_id).or_insert(0);
                // Bit 0 of the Feature item data = Constant flag (1 = read-only)
                let is_constant = !data.is_empty() && (data[0] & 0x01) != 0;

                for field_idx in 0..report_count as usize {
                    let usage = if field_idx < usages.len() {
                        usages[field_idx]
                    } else if !usages.is_empty() {
                        *usages.last().unwrap()
                    } else {
                        continue;
                    };

                    if let Some(key) = recognize_field(usage_page, usage) {
                        fields.insert(
                            key,
                            FeatureField {
                                report_id,
                                bit_offset: base_offset + field_idx * report_size as usize,
                                bit_size: report_size as usize,
                                read_only: is_constant,
                                logical_min,
                                logical_max,
                                physical_min,
                                physical_max,
                            },
                        );
                    }
                }

                let total_bits = report_count as usize * report_size as usize;
                *feature_bit_offsets.get_mut(&report_id).unwrap() += total_bits;
                usages.clear();
            }
            // Input (Main), Output (Main), Collection (Main) — clear local state
            0x80 | 0x90 | 0xA0 => {
                usages.clear();
            }
            _ => {}
        }

        i += 1 + size;
    }

    // Convert per-report bit totals to byte sizes
    let report_byte_sizes: HashMap<u8, usize> = feature_bit_offsets
        .into_iter()
        .map(|(id, bits)| (id, bits.div_ceil(8)))
        .collect();

    (fields, report_byte_sizes)
}

fn read_unsigned(data: &[u8], size: usize) -> u32 {
    match size {
        1 => data[0] as u32,
        2 => u16::from_le_bytes([data[0], data[1]]) as u32,
        4 => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        _ => 0,
    }
}

fn read_signed(data: &[u8], size: usize) -> i32 {
    match size {
        1 => data[0] as i8 as i32,
        2 => i16::from_le_bytes([data[0], data[1]]) as i32,
        4 => i32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        _ => 0,
    }
}

// ── Physical size parser ──────────────────────────────────────────────────────

const GENERIC_DESKTOP_PAGE: u16 = 0x0001;
const USAGE_X: u16 = 0x0030;
const USAGE_Y: u16 = 0x0031;

/// Decode a HID Unit Exponent value (4-bit signed nibble).
fn decode_unit_exponent(raw: i32) -> i32 {
    let nibble = raw & 0x0F;
    if nibble > 7 {
        nibble - 16
    } else {
        nibble
    }
}

/// Convert a physical range with unit info into millimeters.
fn physical_range_mm(phys_min: i32, phys_max: i32, unit: u32, unit_exp_raw: i32) -> Option<f64> {
    let range = (phys_max - phys_min) as f64;
    if range <= 0.0 {
        return None;
    }
    let exp = decode_unit_exponent(unit_exp_raw);
    let system = unit & 0x0F;
    match system {
        1 | 2 => Some(range * 10f64.powi(exp) * 10.0), // SI: cm → mm
        3 | 4 => Some(range * 10f64.powi(exp) * 25.4), // English: inch → mm
        _ => None,
    }
}

/// Build an `AxisPhysicalInfo` from the current HID global state.
fn make_axis_info(
    logical_min: i32,
    logical_max: i32,
    physical_min: i32,
    physical_max: i32,
    unit: u32,
    unit_exp_raw: i32,
) -> Option<AxisPhysicalInfo> {
    let size_mm = physical_range_mm(physical_min, physical_max, unit, unit_exp_raw)?;
    let logical_range = (logical_max - logical_min) as f64;
    let resolution = if size_mm > 0.0 {
        logical_range / size_mm
    } else {
        0.0
    };
    Some(AxisPhysicalInfo {
        logical_min,
        logical_max,
        physical_min,
        physical_max,
        size_mm,
        resolution,
    })
}

/// Parse HID report descriptor to extract the physical touchpad dimensions.
/// Looks for X and Y usages (Generic Desktop page) in Input items and reads
/// their Physical Minimum/Maximum, Unit, and Unit Exponent global state.
fn parse_touchpad_physical_size(desc: &[u8]) -> Option<TouchpadPhysicalSize> {
    let mut usage_page: u16 = 0;
    let mut logical_min: i32 = 0;
    let mut logical_max: i32 = 0;
    let mut physical_min: i32 = 0;
    let mut physical_max: i32 = 0;
    let mut unit: u32 = 0;
    let mut unit_exponent: i32 = 0;
    let mut usages: Vec<u16> = Vec::new();

    let mut x_info: Option<AxisPhysicalInfo> = None;
    let mut y_info: Option<AxisPhysicalInfo> = None;

    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];

        if prefix == 0xFE {
            if i + 2 >= desc.len() {
                break;
            }
            let data_size = desc[i + 1] as usize;
            i += 3 + data_size;
            continue;
        }

        let size = match prefix & 0x03 {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 4,
            _ => unreachable!(),
        };

        if i + 1 + size > desc.len() {
            break;
        }

        let tag = prefix & 0xFC;
        let data = &desc[i + 1..i + 1 + size];

        match tag {
            0x04 => usage_page = read_unsigned(data, size) as u16,
            0x08 => usages.push(read_unsigned(data, size) as u16),
            0x14 => logical_min = read_signed(data, size),
            0x24 => logical_max = read_signed(data, size),
            0x34 => physical_min = read_signed(data, size),
            0x44 => physical_max = read_signed(data, size),
            0x54 => unit_exponent = read_signed(data, size),
            0x64 => unit = read_unsigned(data, size),
            // Input (Main) — check for X/Y on Generic Desktop page
            0x80 => {
                if usage_page == GENERIC_DESKTOP_PAGE {
                    for &u in &usages {
                        if u == USAGE_X && x_info.is_none() {
                            x_info = make_axis_info(
                                logical_min,
                                logical_max,
                                physical_min,
                                physical_max,
                                unit,
                                unit_exponent,
                            );
                        } else if u == USAGE_Y && y_info.is_none() {
                            y_info = make_axis_info(
                                logical_min,
                                logical_max,
                                physical_min,
                                physical_max,
                                unit,
                                unit_exponent,
                            );
                        }
                    }
                }
                usages.clear();
            }
            // Other Main items — clear local state
            0x90 | 0xA0 | 0xB0 | 0xC0 => {
                usages.clear();
            }
            _ => {}
        }

        i += 1 + size;
    }

    match (x_info, y_info) {
        (Some(x), Some(y)) => Some(TouchpadPhysicalSize { x, y }),
        _ => None,
    }
}

// ── Discovery ─────────────────────────────────────────────────────────────────

pub fn discover(evdev_path: &Path) -> Option<PtpConfig> {
    let hidraw_path = match find_sibling_hidraw(evdev_path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("config: failed to find hidraw device: {}", e);
            return None;
        }
    };

    let hidraw_name = hidraw_path.file_name()?.to_str()?;
    let desc_path = format!("/sys/class/hidraw/{}/device/report_descriptor", hidraw_name);
    let desc = match fs::read(&desc_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("config: failed to read report descriptor: {}", e);
            return None;
        }
    };

    let (fields, report_sizes) = parse_ptp_features(&desc);
    let physical_size = parse_touchpad_physical_size(&desc);

    if fields.is_empty() {
        return None;
    }

    let writable =
        |key: FieldKey| -> bool { fields.get(&key).map(|f| !f.read_only).unwrap_or(false) };

    let features = PtpFeatures {
        has_input_mode: fields.contains_key(&KEY_INPUT_MODE),
        has_surface_switch: fields.contains_key(&KEY_SURFACE_SWITCH),
        has_button_switch: fields.contains_key(&KEY_BUTTON_SWITCH),
        has_contact_count_max: fields.contains_key(&KEY_CONTACT_COUNT_MAX),
        has_pad_type: fields.contains_key(&KEY_PAD_TYPE),
        has_latency_mode: fields.contains_key(&KEY_LATENCY_MODE),
        has_button_press_threshold: fields.contains_key(&KEY_BUTTON_PRESS_THRESHOLD),
        has_haptic_intensity: fields.contains_key(&KEY_HAPTIC_INTENSITY),
        input_mode_writable: writable(KEY_INPUT_MODE),
        surface_switch_writable: writable(KEY_SURFACE_SWITCH),
        button_switch_writable: writable(KEY_BUTTON_SWITCH),
        latency_mode_writable: writable(KEY_LATENCY_MODE),
        button_press_threshold_writable: writable(KEY_BUTTON_PRESS_THRESHOLD),
        haptic_intensity_writable: writable(KEY_HAPTIC_INTENSITY),
    };

    let button_press_threshold_range = fields.get(&KEY_BUTTON_PRESS_THRESHOLD).map(|f| f.range());
    let haptic_intensity_range = fields.get(&KEY_HAPTIC_INTENSITY).map(|f| f.range());

    let device = match HidrawDevice::open(&hidraw_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("config: failed to open hidraw device: {}", e);
            return None;
        }
    };

    eprintln!("config: found PTP features on {}", hidraw_path.display());

    let mut backend = LinuxConfigBackend {
        device,
        fields,
        report_sizes,
    };
    let values = backend.read_all();

    let mut config = PtpConfig {
        features,
        input_mode: values.input_mode,
        surface_switch: values.surface_switch,
        button_switch: values.button_switch,
        contact_count_max: values.contact_count_max,
        pad_type: values.pad_type,
        latency_mode: values.latency_mode,
        button_press_threshold: values.button_press_threshold,
        button_press_threshold_range,
        haptic_intensity: values.haptic_intensity,
        haptic_intensity_range,
        physical_size,
        backend: Box::new(backend),
    };
    config.probe_writable();
    Some(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_insert_bits() {
        let data = [0b1010_0110, 0b1100_0011];

        // Extract 4 bits at offset 0: should be 0b0110 = 6
        assert_eq!(extract_bits(&data, 0, 4), 6);

        // Extract 2 bits at offset 4: should be 0b10 = 2
        assert_eq!(extract_bits(&data, 4, 2), 2);

        // Extract 8 bits at offset 4: crosses byte boundary
        assert_eq!(extract_bits(&data, 4, 8), 0b0011_1010);

        // Insert and verify round-trip
        let mut buf = [0u8; 2];
        insert_bits(&mut buf, 0, 4, 0b1001);
        assert_eq!(extract_bits(&buf, 0, 4), 0b1001);

        // Insert at offset preserves other bits
        let mut buf = [0xFF, 0xFF];
        insert_bits(&mut buf, 2, 3, 0b010);
        assert_eq!(extract_bits(&buf, 2, 3), 0b010);
        assert_eq!(buf[0] & 0b11, 0b11); // bits 0-1 preserved
        assert_eq!(buf[0] >> 5, 0b111); // bits 5-7 preserved
    }

    #[test]
    fn test_parse_ptp_features_input_mode() {
        // Minimal HID descriptor with a Feature report containing Input Mode
        let desc: Vec<u8> = vec![
            0x05, 0x0D, // Usage Page (Digitizer)
            0x09, 0x0E, // Usage (Device Configuration)
            0xA1, 0x01, // Collection (Application)
            0x85, 0x03, //   Report ID (3)
            0x09, 0x52, //   Usage (Input Mode)
            0x15, 0x00, //   Logical Minimum (0)
            0x25, 0x03, //   Logical Maximum (3)
            0x75, 0x02, //   Report Size (2)
            0x95, 0x01, //   Report Count (1)
            0xB1, 0x02, //   Feature (Data,Var,Abs)
            0x09, 0x57, //   Usage (Surface Switch)
            0x09, 0x58, //   Usage (Button Switch)
            0x15, 0x00, //   Logical Minimum (0)
            0x25, 0x01, //   Logical Maximum (1)
            0x75, 0x01, //   Report Size (1)
            0x95, 0x02, //   Report Count (2)
            0xB1, 0x02, //   Feature (Data,Var,Abs)
            0xC0, // End Collection
        ];

        let (fields, sizes) = parse_ptp_features(&desc);

        // Input Mode: report_id=3, bit_offset=0, bit_size=2, writable
        let im = fields.get(&KEY_INPUT_MODE).unwrap();
        assert_eq!(im.report_id, 3);
        assert_eq!(im.bit_offset, 0);
        assert_eq!(im.bit_size, 2);
        assert!(!im.read_only);

        // Surface Switch: report_id=3, bit_offset=2, bit_size=1, writable
        let ss = fields.get(&KEY_SURFACE_SWITCH).unwrap();
        assert_eq!(ss.report_id, 3);
        assert_eq!(ss.bit_offset, 2);
        assert_eq!(ss.bit_size, 1);
        assert!(!ss.read_only);

        // Button Switch: report_id=3, bit_offset=3, bit_size=1, writable
        let bs = fields.get(&KEY_BUTTON_SWITCH).unwrap();
        assert_eq!(bs.report_id, 3);
        assert_eq!(bs.bit_offset, 3);
        assert_eq!(bs.bit_size, 1);
        assert!(!bs.read_only);

        // Report size for report_id=3: (2+1+1) bits = 4 bits = 1 byte
        assert_eq!(sizes[&3], 1);
    }

    #[test]
    fn test_parse_touchpad_physical_size() {
        // Descriptor fragment with X and Y Input items including physical size.
        // X: Physical Min 0, Physical Max 1046, Unit Exponent -2, Unit 0x11 (cm)
        //    → 1046 * 10^(-2) cm = 10.46 cm = 104.6 mm
        // Y: Physical Min 0, Physical Max 672, same unit
        //    → 672 * 10^(-2) cm = 6.72 cm = 67.2 mm
        let desc: Vec<u8> = vec![
            0x05, 0x0D, // Usage Page (Digitizer)
            0x09, 0x05, // Usage (Touch Pad)
            0xA1, 0x01, // Collection (Application)
            0x85, 0x01, //   Report ID (1)
            0x09, 0x22, //   Usage (Finger)
            0xA1, 0x02, //   Collection (Logical)
            0x05, 0x01, //     Usage Page (Generic Desktop)
            0x09, 0x30, //     Usage (X)
            0x35, 0x00, //     Physical Minimum (0)
            0x46, 0x16, 0x04, // Physical Maximum (1046)
            0x55, 0x0E, //     Unit Exponent (-2)
            0x65, 0x11, //     Unit (cm)
            0x15, 0x00, //     Logical Minimum (0)
            0x26, 0xFF, 0x0F, // Logical Maximum (4095)
            0x75, 0x10, //     Report Size (16)
            0x95, 0x01, //     Report Count (1)
            0x81, 0x02, //     Input (Data,Var,Abs)
            0x09, 0x31, //     Usage (Y)
            0x46, 0xA0, 0x02, // Physical Maximum (672)
            0x26, 0xFF, 0x0F, // Logical Maximum (4095)
            0x81, 0x02, //     Input (Data,Var,Abs)
            0xC0, //   End Collection
            0xC0, // End Collection
        ];

        let phys = parse_touchpad_physical_size(&desc).unwrap();
        assert!((phys.x.size_mm - 104.6).abs() < 0.01);
        assert!((phys.y.size_mm - 67.2).abs() < 0.01);
        // Logical ranges
        assert_eq!(phys.x.logical_min, 0);
        assert_eq!(phys.x.logical_max, 4095);
        assert_eq!(phys.y.logical_min, 0);
        assert_eq!(phys.y.logical_max, 4095);
        // Physical ranges
        assert_eq!(phys.x.physical_min, 0);
        assert_eq!(phys.x.physical_max, 1046);
        assert_eq!(phys.y.physical_min, 0);
        assert_eq!(phys.y.physical_max, 672);
        // Resolution: 4095 / 104.6 ≈ 39.15, 4095 / 67.2 ≈ 60.94
        assert!((phys.x.resolution - 39.15).abs() < 0.1);
        assert!((phys.y.resolution - 60.94).abs() < 0.1);
    }

    #[test]
    fn test_parse_physical_size_inches() {
        // X: Physical Max 400, Unit Exponent -2, Unit 0x13 (inch)
        //    → 400 * 10^(-2) inch = 4.0 inch = 101.6 mm
        // Y: Physical Max 250
        //    → 250 * 10^(-2) inch = 2.5 inch = 63.5 mm
        let desc: Vec<u8> = vec![
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x30, // Usage (X)
            0x35, 0x00, // Physical Minimum (0)
            0x46, 0x90, 0x01, // Physical Maximum (400)
            0x55, 0x0E, // Unit Exponent (-2)
            0x65, 0x13, // Unit (inch)
            0x15, 0x00, // Logical Minimum (0)
            0x26, 0xFF, 0x0F, // Logical Maximum (4095)
            0x75, 0x10, // Report Size (16)
            0x95, 0x01, // Report Count (1)
            0x81, 0x02, // Input (Data,Var,Abs)
            0x09, 0x31, // Usage (Y)
            0x46, 0xFA, 0x00, // Physical Maximum (250)
            0x81, 0x02, // Input (Data,Var,Abs)
        ];

        let phys = parse_touchpad_physical_size(&desc).unwrap();
        assert!((phys.x.size_mm - 101.6).abs() < 0.01);
        assert!((phys.y.size_mm - 63.5).abs() < 0.01);
    }

    #[test]
    fn test_parse_physical_size_missing() {
        // Descriptor with no physical info (unit = 0)
        let desc: Vec<u8> = vec![
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x09, 0x30, // Usage (X)
            0x15, 0x00, // Logical Minimum (0)
            0x25, 0x7F, // Logical Maximum (127)
            0x75, 0x08, // Report Size (8)
            0x95, 0x01, // Report Count (1)
            0x81, 0x02, // Input (Data,Var,Abs)
        ];

        assert!(parse_touchpad_physical_size(&desc).is_none());
    }

    #[test]
    fn test_parse_constant_feature_fields() {
        // Descriptor with a mix of writable (Data) and read-only (Constant) features
        let desc: Vec<u8> = vec![
            0x05, 0x0D, // Usage Page (Digitizer)
            0x09, 0x0E, // Usage (Device Configuration)
            0xA1, 0x01, // Collection (Application)
            0x85, 0x03, //   Report ID (3)
            0x09, 0x52, //   Usage (Input Mode)
            0x15, 0x00, //   Logical Minimum (0)
            0x25, 0x03, //   Logical Maximum (3)
            0x75, 0x02, //   Report Size (2)
            0x95, 0x01, //   Report Count (1)
            0xB1, 0x02, //   Feature (Data,Var,Abs) — writable
            0x09, 0x55, //   Usage (Contact Count Maximum)
            0x15, 0x00, //   Logical Minimum (0)
            0x25, 0x0A, //   Logical Maximum (10)
            0x75, 0x04, //   Report Size (4)
            0x95, 0x01, //   Report Count (1)
            0xB1, 0x03, //   Feature (Cnst,Var,Abs) — read-only (bit 0 set)
            0x09, 0x60, //   Usage (Latency Mode)
            0x15, 0x00, //   Logical Minimum (0)
            0x25, 0x01, //   Logical Maximum (1)
            0x75, 0x01, //   Report Size (1)
            0x95, 0x01, //   Report Count (1)
            0xB1, 0x03, //   Feature (Cnst,Var,Abs) — read-only
            0xC0, // End Collection
        ];

        let (fields, _) = parse_ptp_features(&desc);

        // Input Mode should be writable
        let im = fields.get(&KEY_INPUT_MODE).unwrap();
        assert!(!im.read_only);

        // Contact Count Max should be read-only
        let ccm = fields.get(&KEY_CONTACT_COUNT_MAX).unwrap();
        assert!(ccm.read_only);

        // Latency Mode should be read-only
        let lm = fields.get(&KEY_LATENCY_MODE).unwrap();
        assert!(lm.read_only);
    }

    #[test]
    fn test_parse_haptic_intensity_and_click_force() {
        // Fragment from a real touchpad descriptor: Report ID 8 (button press
        // threshold, click force, logical 1..3 mapped to 110..190 g) and
        // Report ID 9 (haptic intensity, logical 0..100, on Haptic page 0x0E).
        let desc: Vec<u8> = vec![
            0x05, 0x0D, // Usage Page (Digitizer)
            0x09, 0xB0, // Usage (Button Press Threshold)
            0x85, 0x08, // Report ID (8)
            0x35, 0x6E, // Physical Minimum (110)
            0x46, 0xBE, 0x00, // Physical Maximum (190)
            0x66, 0x01, 0x01, // Unit (SI Linear: g)
            0x15, 0x01, // Logical Minimum (1)
            0x25, 0x03, // Logical Maximum (3)
            0x95, 0x01, // Report Count (1)
            0x75, 0x02, // Report Size (2)
            0xB1, 0x02, // Feature (Data,Var,Abs)
            0x75, 0x06, // Report Size (6)
            0xB1, 0x03, // Feature (Cnst,Var,Abs) — padding
            0x05, 0x0E, // Usage Page (Haptic)
            0x09, 0x01, // Usage (Simple Haptic Controller)
            0xA1, 0x02, // Collection (Logical)
            0x09, 0x23, //   Usage (Intensity)
            0x85, 0x09, //   Report ID (9)
            0x15, 0x00, //   Logical Minimum (0)
            0x25, 0x64, //   Logical Maximum (100)
            0x75, 0x08, //   Report Size (8)
            0x95, 0x01, //   Report Count (1)
            0xB1, 0x02, //   Feature (Data,Var,Abs)
            0xC0, // End Collection
        ];

        let (fields, _) = parse_ptp_features(&desc);

        let bpt = fields
            .get(&KEY_BUTTON_PRESS_THRESHOLD)
            .expect("click force");
        assert_eq!(bpt.report_id, 8);
        assert_eq!(bpt.bit_size, 2);
        assert_eq!(bpt.logical_min, 1);
        assert_eq!(bpt.logical_max, 3);
        assert_eq!(bpt.physical_min, 110);
        assert_eq!(bpt.physical_max, 190);
        assert!(!bpt.read_only);

        let hi = fields.get(&KEY_HAPTIC_INTENSITY).expect("haptic intensity");
        assert_eq!(hi.report_id, 9);
        assert_eq!(hi.bit_size, 8);
        assert_eq!(hi.logical_min, 0);
        assert_eq!(hi.logical_max, 100);
        assert!(!hi.read_only);
    }
}
