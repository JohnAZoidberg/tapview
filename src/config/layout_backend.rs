//! PTP configuration over any [`HidDevice`], driven by the report
//! descriptor: every field is located through a [`ReportLayout`] and read or
//! written with bit extraction on the raw feature report. Used on Linux
//! (hidraw), Android (USB) and, with a layout built from `HIDDevice.collections`,
//! in the browser. Windows keeps its HidP-based backend.

use super::{
    AxisPhysicalInfo, ConfigBackend, ConfigValues, PtpFeatures, TouchpadPhysicalSize, ValueRange,
};
use crate::heatmap::HidDevice;
use crate::hid::{
    extract_bits, insert_bits, physical_range_mm, ReportField, ReportKind, ReportLayout,
};
use std::collections::HashMap;
use std::io;

// PTP usage IDs on Usage Page 0x0D (Digitizer)
pub const DIGITIZER_PAGE: u16 = 0x000D;
pub const USAGE_INPUT_MODE: u16 = 0x0052;
pub const USAGE_CONTACT_COUNT_MAX: u16 = 0x0055;
pub const USAGE_SURFACE_SWITCH: u16 = 0x0057;
pub const USAGE_BUTTON_SWITCH: u16 = 0x0058;
pub const USAGE_PAD_TYPE: u16 = 0x0059;
pub const USAGE_LATENCY_MODE: u16 = 0x0060;
pub const USAGE_BUTTON_PRESS_THRESHOLD: u16 = 0x00B0;

pub const HAPTIC_PAGE: u16 = 0x000E;
pub const USAGE_HAPTIC_INTENSITY: u16 = 0x0023;

const GENERIC_DESKTOP_PAGE: u16 = 0x0001;
const USAGE_X: u16 = 0x0030;
const USAGE_Y: u16 = 0x0031;

/// Key combining usage page and usage ID, so usages on different pages
/// (e.g. Digitizer 0xB0 vs Haptic 0x23) don't collide in the field table.
pub type FieldKey = (u16, u16);
pub const KEY_INPUT_MODE: FieldKey = (DIGITIZER_PAGE, USAGE_INPUT_MODE);
pub const KEY_CONTACT_COUNT_MAX: FieldKey = (DIGITIZER_PAGE, USAGE_CONTACT_COUNT_MAX);
pub const KEY_SURFACE_SWITCH: FieldKey = (DIGITIZER_PAGE, USAGE_SURFACE_SWITCH);
pub const KEY_BUTTON_SWITCH: FieldKey = (DIGITIZER_PAGE, USAGE_BUTTON_SWITCH);
pub const KEY_PAD_TYPE: FieldKey = (DIGITIZER_PAGE, USAGE_PAD_TYPE);
pub const KEY_LATENCY_MODE: FieldKey = (DIGITIZER_PAGE, USAGE_LATENCY_MODE);
pub const KEY_BUTTON_PRESS_THRESHOLD: FieldKey = (DIGITIZER_PAGE, USAGE_BUTTON_PRESS_THRESHOLD);
pub const KEY_HAPTIC_INTENSITY: FieldKey = (HAPTIC_PAGE, USAGE_HAPTIC_INTENSITY);

const ALL_KEYS: [FieldKey; 8] = [
    KEY_INPUT_MODE,
    KEY_CONTACT_COUNT_MAX,
    KEY_SURFACE_SWITCH,
    KEY_BUTTON_SWITCH,
    KEY_PAD_TYPE,
    KEY_LATENCY_MODE,
    KEY_BUTTON_PRESS_THRESHOLD,
    KEY_HAPTIC_INTENSITY,
];

/// The PTP/haptic Feature fields a descriptor declares, keyed by usage.
/// Separate from the device so discovery can run without I/O.
#[derive(Clone, Debug, Default)]
pub struct PtpFields {
    fields: HashMap<FieldKey, ReportField>,
    /// report_id -> payload byte count (excluding the report-ID byte)
    report_bytes: HashMap<u8, usize>,
}

impl PtpFields {
    /// Collect the recognised Feature fields from a layout. The first field
    /// with a given usage wins, as before.
    pub fn from_layout(layout: &ReportLayout) -> PtpFields {
        let mut fields = HashMap::new();
        let mut report_bytes = HashMap::new();
        for key in ALL_KEYS {
            if let Some(f) = layout.find(ReportKind::Feature, key.0, key.1) {
                report_bytes.insert(
                    f.report_id,
                    layout.report_bytes(ReportKind::Feature, f.report_id),
                );
                fields.insert(key, f.clone());
            }
        }
        PtpFields {
            fields,
            report_bytes,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    fn has(&self, key: FieldKey) -> bool {
        self.fields.contains_key(&key)
    }

    fn writable(&self, key: FieldKey) -> bool {
        self.fields.get(&key).map(|f| !f.constant).unwrap_or(false)
    }

    /// Presence and writability flags, as the descriptor declares them.
    pub fn features(&self) -> PtpFeatures {
        PtpFeatures {
            has_input_mode: self.has(KEY_INPUT_MODE),
            has_surface_switch: self.has(KEY_SURFACE_SWITCH),
            has_button_switch: self.has(KEY_BUTTON_SWITCH),
            has_contact_count_max: self.has(KEY_CONTACT_COUNT_MAX),
            has_pad_type: self.has(KEY_PAD_TYPE),
            has_latency_mode: self.has(KEY_LATENCY_MODE),
            has_button_press_threshold: self.has(KEY_BUTTON_PRESS_THRESHOLD),
            has_haptic_intensity: self.has(KEY_HAPTIC_INTENSITY),
            input_mode_writable: self.writable(KEY_INPUT_MODE),
            surface_switch_writable: self.writable(KEY_SURFACE_SWITCH),
            button_switch_writable: self.writable(KEY_BUTTON_SWITCH),
            latency_mode_writable: self.writable(KEY_LATENCY_MODE),
            button_press_threshold_writable: self.writable(KEY_BUTTON_PRESS_THRESHOLD),
            haptic_intensity_writable: self.writable(KEY_HAPTIC_INTENSITY),
        }
    }

    /// Logical (and physical, when declared distinct) range of a field.
    pub fn range(&self, key: FieldKey) -> Option<ValueRange> {
        self.fields.get(&key).map(value_range)
    }
}

/// The slider range of a numeric feature field.
pub fn value_range(f: &ReportField) -> ValueRange {
    let physical = if f.physical_min != f.physical_max
        && (f.physical_min, f.physical_max) != (f.logical_min, f.logical_max)
    {
        Some((f.physical_min, f.physical_max))
    } else {
        None
    };
    ValueRange {
        logical_min: f.logical_min,
        logical_max: f.logical_max,
        physical,
    }
}

fn axis_info(f: &ReportField) -> Option<AxisPhysicalInfo> {
    let size_mm = physical_range_mm(f)?;
    let logical_range = (f.logical_max - f.logical_min) as f64;
    let resolution = if size_mm > 0.0 {
        logical_range / size_mm
    } else {
        0.0
    };
    Some(AxisPhysicalInfo {
        logical_min: f.logical_min,
        logical_max: f.logical_max,
        physical_min: f.physical_min,
        physical_max: f.physical_max,
        size_mm,
        resolution,
    })
}

/// Physical touchpad dimensions from the first Input X/Y fields on the
/// Generic Desktop page that carry a length unit. (A Mouse collection
/// declared ahead of the Touch Pad has unit-less X/Y; those are skipped.)
pub fn touchpad_physical_size(layout: &ReportLayout) -> Option<TouchpadPhysicalSize> {
    let axis = |usage: u16| {
        layout
            .fields
            .iter()
            .filter(|f| {
                f.kind == ReportKind::Input
                    && f.usage_page == GENERIC_DESKTOP_PAGE
                    && f.usage == usage
            })
            .find_map(axis_info)
    };
    Some(TouchpadPhysicalSize {
        x: axis(USAGE_X)?,
        y: axis(USAGE_Y)?,
    })
}

/// [`ConfigBackend`] over a raw HID device and a parsed descriptor.
pub struct LayoutConfigBackend<D: HidDevice> {
    device: D,
    fields: PtpFields,
}

impl<D: HidDevice> LayoutConfigBackend<D> {
    /// `None` when the descriptor declares no PTP/haptic feature at all.
    pub fn new(device: D, fields: PtpFields) -> Option<Self> {
        if fields.is_empty() {
            return None;
        }
        Some(Self { device, fields })
    }

    pub fn fields(&self) -> &PtpFields {
        &self.fields
    }

    fn report_buf(&self, field: &ReportField) -> Vec<u8> {
        let size = self
            .fields
            .report_bytes
            .get(&field.report_id)
            .copied()
            .unwrap_or(0);
        let mut buf = vec![0u8; 1 + size];
        buf[0] = field.report_id;
        buf
    }

    async fn read_field(&self, key: FieldKey) -> Option<u32> {
        let field = self.fields.fields.get(&key)?;
        let mut buf = self.report_buf(field);
        self.device.get_feature(&mut buf).await.ok()?;
        Some(extract_bits(&buf[1..], field.bit_offset, field.bit_size))
    }

    async fn write_field(&self, key: FieldKey, value: u32) -> io::Result<()> {
        let field =
            self.fields.fields.get(&key).ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "feature field not found")
            })?;
        let mut buf = self.report_buf(field);
        // Read-modify-write
        self.device.get_feature(&mut buf).await?;
        insert_bits(&mut buf[1..], field.bit_offset, field.bit_size, value);
        self.device.set_feature(&buf).await
    }
}

impl<D: HidDevice> ConfigBackend for LayoutConfigBackend<D> {
    async fn read_all(&mut self) -> ConfigValues {
        ConfigValues {
            input_mode: self.read_field(KEY_INPUT_MODE).await.map(|v| v as u8),
            surface_switch: self.read_field(KEY_SURFACE_SWITCH).await.map(|v| v != 0),
            button_switch: self.read_field(KEY_BUTTON_SWITCH).await.map(|v| v != 0),
            contact_count_max: self
                .read_field(KEY_CONTACT_COUNT_MAX)
                .await
                .map(|v| v as u8),
            pad_type: self.read_field(KEY_PAD_TYPE).await.map(|v| v as u8),
            latency_mode: self.read_field(KEY_LATENCY_MODE).await.map(|v| v != 0),
        }
    }

    async fn write_input_mode(&mut self, value: u8) -> io::Result<()> {
        self.write_field(KEY_INPUT_MODE, value as u32).await
    }

    async fn write_selective_reporting(&mut self, surface: bool, button: bool) -> io::Result<()> {
        if self.fields.has(KEY_SURFACE_SWITCH) {
            self.write_field(KEY_SURFACE_SWITCH, surface as u32).await?;
        }
        if self.fields.has(KEY_BUTTON_SWITCH) {
            self.write_field(KEY_BUTTON_SWITCH, button as u32).await?;
        }
        Ok(())
    }

    async fn write_latency_mode(&mut self, high: bool) -> io::Result<()> {
        self.write_field(KEY_LATENCY_MODE, high as u32).await
    }

    async fn write_button_press_threshold(&mut self, value: u8) -> io::Result<()> {
        self.write_field(KEY_BUTTON_PRESS_THRESHOLD, value as u32)
            .await
    }

    async fn write_haptic_intensity(&mut self, value: u8) -> io::Result<()> {
        self.write_field(KEY_HAPTIC_INTENSITY, value as u32).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ptp_fields_from_layout() {
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
            0xB1, 0x03, //   Feature (Cnst,Var,Abs) — read-only
            0x09, 0x60, //   Usage (Latency Mode)
            0x15, 0x00, //   Logical Minimum (0)
            0x25, 0x01, //   Logical Maximum (1)
            0x75, 0x01, //   Report Size (1)
            0x95, 0x01, //   Report Count (1)
            0xB1, 0x03, //   Feature (Cnst,Var,Abs) — read-only
            0xC0, // End Collection
        ];
        let fields = PtpFields::from_layout(&ReportLayout::parse(&desc));
        let f = fields.features();
        assert!(f.has_input_mode && f.input_mode_writable);
        assert!(f.has_contact_count_max);
        assert!(f.has_latency_mode && !f.latency_mode_writable);
        assert!(!f.has_haptic_intensity);
        // 2+4+1 bits -> 1 byte
        assert_eq!(fields.report_bytes[&3], 1);
    }

    #[test]
    fn haptic_intensity_and_click_force_ranges() {
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
        let fields = PtpFields::from_layout(&ReportLayout::parse(&desc));
        let bpt = fields
            .range(KEY_BUTTON_PRESS_THRESHOLD)
            .expect("click force");
        assert_eq!((bpt.logical_min, bpt.logical_max), (1, 3));
        assert_eq!(bpt.physical, Some((110, 190)));
        let hi = fields
            .range(KEY_HAPTIC_INTENSITY)
            .expect("haptic intensity");
        assert_eq!((hi.logical_min, hi.logical_max), (0, 100));
        // Physical Minimum/Maximum are global items and were never reset, so
        // the haptic field inherits the click-force range. Faithful to the
        // descriptor (and to what the previous parser did).
        assert_eq!(hi.physical, Some((110, 190)));
        let f = fields.features();
        assert!(f.button_press_threshold_writable && f.haptic_intensity_writable);
        assert_eq!(fields.report_bytes[&8], 1);
        assert_eq!(fields.report_bytes[&9], 1);
    }

    #[test]
    fn physical_size_cm_and_inches() {
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
        let phys = touchpad_physical_size(&ReportLayout::parse(&desc)).unwrap();
        assert!((phys.x.size_mm - 104.6).abs() < 0.01);
        assert!((phys.y.size_mm - 67.2).abs() < 0.01);
        assert_eq!((phys.x.logical_min, phys.x.logical_max), (0, 4095));
        assert_eq!((phys.x.physical_min, phys.x.physical_max), (0, 1046));
        assert_eq!((phys.y.physical_min, phys.y.physical_max), (0, 672));
        assert!((phys.x.resolution - 39.15).abs() < 0.1);
        assert!((phys.y.resolution - 60.94).abs() < 0.1);

        let inches: Vec<u8> = vec![
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
        let phys = touchpad_physical_size(&ReportLayout::parse(&inches)).unwrap();
        assert!((phys.x.size_mm - 101.6).abs() < 0.01);
        assert!((phys.y.size_mm - 63.5).abs() < 0.01);

        let no_unit: Vec<u8> = vec![
            0x05, 0x01, 0x09, 0x30, 0x15, 0x00, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02,
        ];
        assert!(touchpad_physical_size(&ReportLayout::parse(&no_unit)).is_none());
    }

    #[test]
    fn framework13_physical_size_skips_mouse_axes() {
        // The Mouse collection (unit-less X/Y, max 127) comes first in this
        // descriptor; the touchpad axes behind it must still be found.
        let desc = include_bytes!("../../testdata/ptp/framework13_pixa3854.desc");
        let layout = ReportLayout::parse(desc);
        let phys = touchpad_physical_size(&layout).unwrap();
        assert_eq!((phys.x.logical_max, phys.y.logical_max), (2833, 1723));
        assert_eq!((phys.x.physical_max, phys.y.physical_max), (1200, 730));
        assert!((phys.x.size_mm - 120.0).abs() < 0.01);
        assert!((phys.y.size_mm - 73.0).abs() < 0.01);
        let fields = PtpFields::from_layout(&layout);
        let f = fields.features();
        assert!(f.has_input_mode && f.has_haptic_intensity && f.has_button_press_threshold);
        assert!(f.has_contact_count_max);
    }
}
