//! A flat model of a HID report descriptor: every report element as a
//! [`ReportField`] with its bit position, plus the collection tree it sits in.
//!
//! Built by [`ReportLayout::parse`] from raw descriptor bytes with a single
//! walker that understands the short items the PTP, heatmap and config code
//! care about (global/local state, Push/Pop, Usage Minimum/Maximum, extended
//! usages, collections). Long items are skipped.

use std::collections::HashMap;

/// Which report type a field belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReportKind {
    Input,
    Output,
    Feature,
}

/// One element of a report: a single value (`report_count` items are
/// expanded into one field each), located by `bit_offset`/`bit_size` inside
/// the report payload (the report-ID byte, if any, is not counted).
#[derive(Clone, Debug, PartialEq)]
pub struct ReportField {
    pub kind: ReportKind,
    /// 0 for unnumbered reports.
    pub report_id: u8,
    /// 0/0 for padding (an item with no usage).
    pub usage_page: u16,
    pub usage: u16,
    /// Index into [`ReportLayout::collections`] of the innermost enclosing
    /// collection, if any.
    pub collection: Option<usize>,
    pub bit_offset: usize,
    pub bit_size: usize,
    /// Main-item flag bit 0: Constant (read-only / padding).
    pub constant: bool,
    /// Main-item flag bit 1: Variable (as opposed to Array).
    pub variable: bool,
    pub logical_min: i32,
    pub logical_max: i32,
    pub physical_min: i32,
    pub physical_max: i32,
    /// Raw Unit item value.
    pub unit: u32,
    /// Raw Unit Exponent item value; decode with [`decode_unit_exponent`].
    pub unit_exponent: i32,
}

/// A `Collection` main item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collection {
    pub parent: Option<usize>,
    /// Collection type: 0 Physical, 1 Application, 2 Logical, ...
    pub kind: u8,
    pub usage_page: u16,
    pub usage: u16,
}

impl Collection {
    pub const APPLICATION: u8 = 1;
}

/// The parsed descriptor.
#[derive(Clone, Debug, Default)]
pub struct ReportLayout {
    /// Fields in descriptor order.
    pub fields: Vec<ReportField>,
    pub collections: Vec<Collection>,
    /// Total payload bits per (kind, report id).
    sizes: HashMap<(ReportKind, u8), usize>,
}

/// Global item state, saved/restored by Push/Pop.
#[derive(Clone, Copy, Default)]
struct Globals {
    usage_page: u16,
    logical_min: i32,
    logical_max: i32,
    physical_min: i32,
    physical_max: i32,
    unit: u32,
    unit_exponent: i32,
    report_size: u32,
    report_id: u8,
    report_count: u32,
}

impl ReportLayout {
    /// Parse raw descriptor bytes. Never fails: malformed trailing data is
    /// ignored, unknown items are skipped.
    pub fn parse(desc: &[u8]) -> ReportLayout {
        let mut layout = ReportLayout::default();
        let mut g = Globals::default();
        let mut stack: Vec<Globals> = Vec::new();
        // Local state: (usage_page override for extended usages, usage)
        let mut usages: Vec<(Option<u16>, u16)> = Vec::new();
        let mut usage_min: Option<(Option<u16>, u32)> = None;
        let mut collection_stack: Vec<usize> = Vec::new();

        let mut i = 0;
        while i < desc.len() {
            let prefix = desc[i];

            // Long item: skip
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
                _ => 4,
            };
            if i + 1 + size > desc.len() {
                break;
            }
            let tag = prefix & 0xFC;
            let data = &desc[i + 1..i + 1 + size];
            let unsigned = read_unsigned(data);
            let signed = read_signed(data);

            match tag {
                // ── Global items ─────────────────────────────────────────
                0x04 => g.usage_page = unsigned as u16,
                0x14 => g.logical_min = signed,
                0x24 => g.logical_max = signed,
                0x34 => g.physical_min = signed,
                0x44 => g.physical_max = signed,
                0x54 => g.unit_exponent = signed,
                0x64 => g.unit = unsigned,
                0x74 => g.report_size = unsigned,
                0x84 => g.report_id = unsigned as u8,
                0x94 => g.report_count = unsigned,
                0xA4 => stack.push(g),
                0xB4 => {
                    if let Some(saved) = stack.pop() {
                        g = saved;
                    }
                }
                // ── Local items ──────────────────────────────────────────
                0x08 => usages.push(split_usage(unsigned, size)),
                0x18 => {
                    let (page, u) = split_usage(unsigned, size);
                    usage_min = Some((page, u as u32));
                }
                0x28 => {
                    let (page, max) = split_usage(unsigned, size);
                    if let Some((min_page, min)) = usage_min.take() {
                        for u in min..=(max as u32) {
                            usages.push((min_page.or(page), u as u16));
                        }
                    }
                }
                // ── Main items ───────────────────────────────────────────
                0xA0 => {
                    let (page, usage) = usages
                        .first()
                        .map(|&(p, u)| (p.unwrap_or(g.usage_page), u))
                        .unwrap_or((g.usage_page, 0));
                    layout.collections.push(Collection {
                        parent: collection_stack.last().copied(),
                        kind: unsigned as u8,
                        usage_page: page,
                        usage,
                    });
                    collection_stack.push(layout.collections.len() - 1);
                    usages.clear();
                    usage_min = None;
                }
                0xC0 => {
                    collection_stack.pop();
                    usages.clear();
                    usage_min = None;
                }
                0x80 | 0x90 | 0xB0 => {
                    let kind = match tag {
                        0x80 => ReportKind::Input,
                        0x90 => ReportKind::Output,
                        _ => ReportKind::Feature,
                    };
                    let flags = unsigned;
                    let constant = flags & 0x01 != 0;
                    let variable = flags & 0x02 != 0;
                    let bit_size = g.report_size as usize;
                    let offset = layout.sizes.entry((kind, g.report_id)).or_insert(0);
                    for idx in 0..g.report_count as usize {
                        // Variable items take usages in order, the last one
                        // repeating; array items share the whole usage list,
                        // recorded by its first entry. No usage = padding.
                        let (page, usage) = if variable {
                            usages
                                .get(idx)
                                .or(usages.last())
                                .map(|&(p, u)| (p.unwrap_or(g.usage_page), u))
                                .unwrap_or((0, 0))
                        } else {
                            usages
                                .first()
                                .map(|&(p, u)| (p.unwrap_or(g.usage_page), u))
                                .unwrap_or((0, 0))
                        };
                        layout.fields.push(ReportField {
                            kind,
                            report_id: g.report_id,
                            usage_page: page,
                            usage,
                            collection: collection_stack.last().copied(),
                            bit_offset: *offset + idx * bit_size,
                            bit_size,
                            constant,
                            variable,
                            logical_min: g.logical_min,
                            logical_max: g.logical_max,
                            physical_min: g.physical_min,
                            physical_max: g.physical_max,
                            unit: g.unit,
                            unit_exponent: g.unit_exponent,
                        });
                    }
                    *offset += g.report_count as usize * bit_size;
                    usages.clear();
                    usage_min = None;
                }
                _ => {}
            }

            i += 1 + size;
        }

        layout
    }

    /// Build a layout from fields that were laid out elsewhere (a browser's
    /// parsed `HIDDevice.collections`): report sizes are derived from the
    /// fields' extents, so every field of a report — padding included — must
    /// be present.
    pub fn from_parts(fields: Vec<ReportField>, collections: Vec<Collection>) -> ReportLayout {
        let mut sizes: HashMap<(ReportKind, u8), usize> = HashMap::new();
        for f in &fields {
            let end = f.bit_offset + f.bit_size;
            let size = sizes.entry((f.kind, f.report_id)).or_insert(0);
            *size = (*size).max(end);
        }
        ReportLayout {
            fields,
            collections,
            sizes,
        }
    }

    /// Payload size in bytes of a report (excluding the report-ID byte); 0
    /// if the descriptor declares no such report.
    pub fn report_bytes(&self, kind: ReportKind, report_id: u8) -> usize {
        self.sizes
            .get(&(kind, report_id))
            .map(|bits| bits.div_ceil(8))
            .unwrap_or(0)
    }

    /// Report IDs declared for `kind`, in ascending order.
    pub fn report_ids(&self, kind: ReportKind) -> Vec<u8> {
        let mut ids: Vec<u8> = self
            .sizes
            .keys()
            .filter(|(k, _)| *k == kind)
            .map(|(_, id)| *id)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// First field (descriptor order) of `kind` with the given usage.
    pub fn find(&self, kind: ReportKind, usage_page: u16, usage: u16) -> Option<&ReportField> {
        self.fields
            .iter()
            .find(|f| f.kind == kind && f.usage_page == usage_page && f.usage == usage)
    }

    /// All fields of one report, in descriptor order.
    pub fn fields_in(&self, kind: ReportKind, report_id: u8) -> impl Iterator<Item = &ReportField> {
        self.fields
            .iter()
            .filter(move |f| f.kind == kind && f.report_id == report_id)
    }

    /// Whether `field` sits (at any depth) inside a collection with the given usage.
    pub fn is_inside(&self, field: &ReportField, usage_page: u16, usage: u16) -> bool {
        let mut cur = field.collection;
        while let Some(idx) = cur {
            let c = &self.collections[idx];
            if c.usage_page == usage_page && c.usage == usage {
                return true;
            }
            cur = c.parent;
        }
        false
    }

    /// Whether the descriptor declares an Application collection with this usage.
    pub fn has_application_collection(&self, usage_page: u16, usage: u16) -> bool {
        self.collections.iter().any(|c| {
            c.kind == Collection::APPLICATION && c.usage_page == usage_page && c.usage == usage
        })
    }
}

/// A Usage/Usage Minimum/Usage Maximum item: 4-byte data carries the usage
/// page in the high half (an "extended usage").
fn split_usage(value: u32, size: usize) -> (Option<u16>, u16) {
    if size == 4 {
        (Some((value >> 16) as u16), value as u16)
    } else {
        (None, value as u16)
    }
}

fn read_unsigned(data: &[u8]) -> u32 {
    match data.len() {
        1 => data[0] as u32,
        2 => u16::from_le_bytes([data[0], data[1]]) as u32,
        4 => u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        _ => 0,
    }
}

fn read_signed(data: &[u8]) -> i32 {
    match data.len() {
        1 => data[0] as i8 as i32,
        2 => i16::from_le_bytes([data[0], data[1]]) as i32,
        4 => i32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        _ => 0,
    }
}

// ── Bit manipulation ──────────────────────────────────────────────────────

/// Read `bit_size` bits (LSB first, little-endian byte order) at `bit_offset`.
pub fn extract_bits(data: &[u8], bit_offset: usize, bit_size: usize) -> u32 {
    if bit_size == 0 {
        return 0;
    }
    let byte_offset = bit_offset / 8;
    let bit_shift = bit_offset % 8;
    let bytes_needed = (bit_shift + bit_size).div_ceil(8);
    let mut value: u64 = 0;
    for i in 0..bytes_needed {
        if byte_offset + i < data.len() {
            value |= (data[byte_offset + i] as u64) << (i * 8);
        }
    }
    let mask = if bit_size >= 32 {
        u32::MAX
    } else {
        (1u32 << bit_size) - 1
    };
    ((value >> bit_shift) as u32) & mask
}

/// Like [`extract_bits`] but sign-extends from `bit_size` bits.
pub fn extract_signed(data: &[u8], bit_offset: usize, bit_size: usize) -> i32 {
    let raw = extract_bits(data, bit_offset, bit_size);
    if bit_size == 0 || bit_size >= 32 {
        return raw as i32;
    }
    let shift = 32 - bit_size;
    ((raw << shift) as i32) >> shift
}

/// Write the low `bit_size` bits of `value` at `bit_offset`, leaving the
/// surrounding bits untouched.
pub fn insert_bits(data: &mut [u8], bit_offset: usize, bit_size: usize, value: u32) {
    if bit_size == 0 {
        return;
    }
    let byte_offset = bit_offset / 8;
    let bit_shift = bit_offset % 8;
    let field_mask: u64 = if bit_size >= 32 {
        u32::MAX as u64
    } else {
        (1u64 << bit_size) - 1
    };
    let mask = field_mask << bit_shift;
    let shifted_value = ((value as u64) & field_mask) << bit_shift;
    let bytes_needed = (bit_shift + bit_size).div_ceil(8);
    for i in 0..bytes_needed {
        if byte_offset + i < data.len() {
            let byte_mask = (mask >> (i * 8)) as u8;
            let byte_val = (shifted_value >> (i * 8)) as u8;
            data[byte_offset + i] = (data[byte_offset + i] & !byte_mask) | (byte_val & byte_mask);
        }
    }
}

// ── Units ─────────────────────────────────────────────────────────────────

/// Decode a HID Unit Exponent value (4-bit signed nibble).
pub fn decode_unit_exponent(raw: i32) -> i32 {
    let nibble = raw & 0x0F;
    if nibble > 7 {
        nibble - 16
    } else {
        nibble
    }
}

/// The physical extent of a field in millimeters, if its Unit is a length
/// (SI: cm, English: inch) and its physical range is non-empty.
pub fn physical_range_mm(field: &ReportField) -> Option<f64> {
    let range = (field.physical_max - field.physical_min) as f64;
    if range <= 0.0 {
        return None;
    }
    let exp = decode_unit_exponent(field.unit_exponent);
    match field.unit & 0x0F {
        1 | 2 => Some(range * 10f64.powi(exp) * 10.0), // SI: cm → mm
        3 | 4 => Some(range * 10f64.powi(exp) * 25.4), // English: inch → mm
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGITIZER: u16 = 0x0D;
    const GENERIC_DESKTOP: u16 = 0x01;
    const BUTTON: u16 = 0x09;

    /// Report descriptor of the Framework Laptop 13/16 touchpad
    /// (PixArt PIXA3854, 093A:0343), copied from sysfs.
    const FRAMEWORK13: &[u8] = include_bytes!("../../testdata/ptp/framework13_pixa3854.desc");

    #[test]
    fn extract_insert_bits() {
        let data = [0b1010_0110, 0b1100_0011];
        assert_eq!(extract_bits(&data, 0, 4), 6);
        assert_eq!(extract_bits(&data, 4, 2), 2);
        assert_eq!(extract_bits(&data, 4, 8), 0b0011_1010);

        let mut buf = [0u8; 2];
        insert_bits(&mut buf, 0, 4, 0b1001);
        assert_eq!(extract_bits(&buf, 0, 4), 0b1001);

        let mut buf = [0xFF, 0xFF];
        insert_bits(&mut buf, 2, 3, 0b010);
        assert_eq!(extract_bits(&buf, 2, 3), 0b010);
        assert_eq!(buf[0] & 0b11, 0b11);
        assert_eq!(buf[0] >> 5, 0b111);

        // 32-bit field spanning 5 bytes at a 4-bit offset
        let mut buf = [0u8; 5];
        insert_bits(&mut buf, 4, 32, 0xDEAD_BEEF);
        assert_eq!(extract_bits(&buf, 4, 32), 0xDEAD_BEEF);
    }

    #[test]
    fn extract_signed_sign_extends() {
        let data = [0xF0, 0x0F]; // low nibble 0, then 0xFF, then 0
        assert_eq!(extract_signed(&data, 4, 8), -1);
        assert_eq!(extract_signed(&data, 4, 4), -1);
        assert_eq!(extract_signed(&data, 8, 4), -1);
        assert_eq!(extract_signed(&data, 12, 4), 0);
        let data = [0x7F];
        assert_eq!(extract_signed(&data, 0, 8), 127);
    }

    #[test]
    fn unit_exponent_and_mm() {
        assert_eq!(decode_unit_exponent(0x0E), -2);
        assert_eq!(decode_unit_exponent(0x03), 3);
        let mut f = padding_field();
        f.physical_min = 0;
        f.physical_max = 1046;
        f.unit = 0x11; // cm
        f.unit_exponent = 0x0E;
        assert!((physical_range_mm(&f).unwrap() - 104.6).abs() < 0.01);
        f.unit = 0x13; // inch
        f.physical_max = 400;
        assert!((physical_range_mm(&f).unwrap() - 101.6).abs() < 0.01);
        f.unit = 0;
        assert!(physical_range_mm(&f).is_none());
    }

    fn padding_field() -> ReportField {
        ReportField {
            kind: ReportKind::Input,
            report_id: 0,
            usage_page: 0,
            usage: 0,
            collection: None,
            bit_offset: 0,
            bit_size: 0,
            constant: true,
            variable: false,
            logical_min: 0,
            logical_max: 0,
            physical_min: 0,
            physical_max: 0,
            unit: 0,
            unit_exponent: 0,
        }
    }

    #[test]
    fn feature_fields_and_report_size() {
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
            0x09, 0x55, //   Usage (Contact Count Maximum)
            0x75, 0x04, //   Report Size (4)
            0x95, 0x01, //   Report Count (1)
            0xB1, 0x03, //   Feature (Cnst,Var,Abs)
            0xC0, // End Collection
        ];
        let layout = ReportLayout::parse(&desc);

        let im = layout.find(ReportKind::Feature, DIGITIZER, 0x52).unwrap();
        assert_eq!((im.report_id, im.bit_offset, im.bit_size), (3, 0, 2));
        assert!(!im.constant);
        assert_eq!((im.logical_min, im.logical_max), (0, 3));

        let ss = layout.find(ReportKind::Feature, DIGITIZER, 0x57).unwrap();
        assert_eq!((ss.bit_offset, ss.bit_size), (2, 1));
        let bs = layout.find(ReportKind::Feature, DIGITIZER, 0x58).unwrap();
        assert_eq!((bs.bit_offset, bs.bit_size), (3, 1));

        let ccm = layout.find(ReportKind::Feature, DIGITIZER, 0x55).unwrap();
        assert!(ccm.constant);
        assert_eq!(ccm.bit_offset, 4);

        // 2+1+1+4 bits = 8 bits = 1 byte
        assert_eq!(layout.report_bytes(ReportKind::Feature, 3), 1);
        assert_eq!(layout.report_bytes(ReportKind::Input, 3), 0);
        assert_eq!(layout.report_ids(ReportKind::Feature), vec![3]);

        assert!(layout.has_application_collection(DIGITIZER, 0x0E));
        assert!(!layout.has_application_collection(DIGITIZER, 0x05));
        assert_eq!(layout.collections[0].kind, Collection::APPLICATION);
        assert!(layout.is_inside(im, DIGITIZER, 0x0E));
    }

    #[test]
    fn push_pop_and_extended_usage() {
        let desc: Vec<u8> = vec![
            0x05, 0x01, // Usage Page (Generic Desktop)
            0x75, 0x08, // Report Size (8)
            0x95, 0x01, // Report Count (1)
            0xA4, // Push
            0x75, 0x10, // Report Size (16)
            0x0B, 0x30, 0x00, 0x0D, 0x00, // Usage (Digitizer page, 0x30 Tip Pressure)
            0x81, 0x02, // Input
            0xB4, // Pop
            0x09, 0x30, // Usage (X)
            0x81, 0x02, // Input
        ];
        let layout = ReportLayout::parse(&desc);
        assert_eq!(layout.fields.len(), 2);
        assert_eq!(layout.fields[0].usage_page, DIGITIZER);
        assert_eq!(layout.fields[0].bit_size, 16);
        assert_eq!(layout.fields[1].usage_page, GENERIC_DESKTOP);
        assert_eq!(layout.fields[1].bit_size, 8);
        assert_eq!(layout.fields[1].bit_offset, 16);
        assert_eq!(layout.report_bytes(ReportKind::Input, 0), 3);
    }

    #[test]
    fn usage_range_expands_to_fields() {
        let desc: Vec<u8> = vec![
            0x05, 0x09, // Usage Page (Button)
            0x19, 0x01, // Usage Minimum (1)
            0x29, 0x03, // Usage Maximum (3)
            0x15, 0x00, 0x25, 0x01, // Logical 0..1
            0x75, 0x01, // Report Size (1)
            0x95, 0x03, // Report Count (3)
            0x81, 0x02, // Input (Data,Var,Abs)
            0x75, 0x05, // Report Size (5)
            0x95, 0x01, // Report Count (1)
            0x81, 0x03, // Input (Cnst) padding
        ];
        let layout = ReportLayout::parse(&desc);
        let buttons: Vec<u16> = layout
            .fields_in(ReportKind::Input, 0)
            .filter(|f| f.usage_page == BUTTON)
            .map(|f| f.usage)
            .collect();
        assert_eq!(buttons, vec![1, 2, 3]);
        let pad = layout.fields.last().unwrap();
        assert!(pad.constant);
        assert_eq!((pad.usage_page, pad.usage), (0, 0));
        assert_eq!((pad.bit_offset, pad.bit_size), (3, 5));
        assert_eq!(layout.report_bytes(ReportKind::Input, 0), 1);
    }

    #[test]
    fn framework13_fixture() {
        let layout = ReportLayout::parse(FRAMEWORK13);

        // Touch Pad application collection, Input report 1 of 28 payload bytes
        assert!(layout.has_application_collection(DIGITIZER, 0x05));
        assert_eq!(layout.report_bytes(ReportKind::Input, 1), 28);

        // Contact Count: 4 bits at bit 4 (after button + 2 pad bits + 1 vendor bit)
        let cc = layout.find(ReportKind::Input, DIGITIZER, 0x54).unwrap();
        assert_eq!((cc.report_id, cc.bit_offset, cc.bit_size), (1, 4, 4));

        // Five Finger collections under the Touch Pad collection (the Device
        // Configuration collection has two more), each with X (max 2833) and
        // Y (max 1723)
        let touchpad = layout
            .collections
            .iter()
            .position(|c| c.kind == Collection::APPLICATION && c.usage == 0x05)
            .unwrap();
        let fingers: Vec<&Collection> = layout
            .collections
            .iter()
            .filter(|c| c.parent == Some(touchpad) && c.usage_page == DIGITIZER && c.usage == 0x22)
            .collect();
        assert_eq!(fingers.len(), 5);
        let xs: Vec<&ReportField> = layout
            .fields_in(ReportKind::Input, 1)
            .filter(|f| f.usage_page == GENERIC_DESKTOP && f.usage == 0x30)
            .collect();
        assert_eq!(xs.len(), 5);
        assert_eq!(xs[0].logical_max, 2833);
        assert!((physical_range_mm(xs[0]).unwrap() - 120.0).abs() < 0.01);
        assert!(layout.is_inside(xs[0], DIGITIZER, 0x22));
        assert!(layout.is_inside(xs[0], DIGITIZER, 0x05));
        // `find` returns descriptor order, and the Mouse collection comes first
        let mouse_y = layout
            .find(ReportKind::Input, GENERIC_DESKTOP, 0x31)
            .unwrap();
        assert_eq!((mouse_y.report_id, mouse_y.logical_max), (2, 127));
        let y = layout
            .fields_in(ReportKind::Input, 1)
            .find(|f| f.usage_page == GENERIC_DESKTOP && f.usage == 0x31)
            .unwrap();
        assert_eq!(y.logical_max, 1723);
        assert!((physical_range_mm(y).unwrap() - 73.0).abs() < 0.01);

        // One button (usage 1) outside the finger collections
        let buttons: Vec<&ReportField> = layout
            .fields_in(ReportKind::Input, 1)
            .filter(|f| f.usage_page == BUTTON)
            .collect();
        assert_eq!(buttons.len(), 1);
        assert_eq!(buttons[0].usage, 1);
        assert!(!layout.is_inside(buttons[0], DIGITIZER, 0x22));

        // Heatmap burst report: 256 payload bytes; register access reports 3 bytes
        assert_eq!(layout.report_bytes(ReportKind::Feature, 0x41), 256);
        assert_eq!(layout.report_bytes(ReportKind::Feature, 0x42), 3);

        // PTP config: Input Mode in Feature report 3
        let im = layout.find(ReportKind::Feature, DIGITIZER, 0x52).unwrap();
        assert_eq!(im.report_id, 3);
        // Contact Count Maximum: this pad declares it as Data, not Constant
        let ccm = layout.find(ReportKind::Feature, DIGITIZER, 0x55).unwrap();
        assert_eq!(ccm.logical_max, 5);
        assert_eq!(ccm.bit_size, 8);
    }
}
