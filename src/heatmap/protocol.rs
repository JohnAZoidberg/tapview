use super::hidraw::HidrawDevice;
use std::io;

const REPORT_SINGLE: u8 = 0x42;
const REPORT_USER: u8 = 0x43;
const REPORT_BURST: u8 = 0x41;
const READ_FLAG: u8 = 0x10;

/// Write a single register via Report 0x42.
pub fn write_reg(dev: &HidrawDevice, bank: u8, addr: u8, value: u8) -> io::Result<()> {
    dev.set_feature(&[REPORT_SINGLE, addr, bank, value])
}

/// Read a single register via Report 0x42.
/// Step 1: SetFeature with bank | 0x10 read flag.
/// Step 2: GetFeature, result at buf[3].
pub fn read_reg(dev: &HidrawDevice, bank: u8, addr: u8) -> io::Result<u8> {
    dev.set_feature(&[REPORT_SINGLE, addr, bank | READ_FLAG, 0x00])?;
    let mut buf = [REPORT_SINGLE, 0, 0, 0];
    dev.get_feature(&mut buf)?;
    Ok(buf[3])
}

/// Read a user register via Report 0x43.
pub fn read_user_reg(dev: &HidrawDevice, bank: u8, addr: u8) -> io::Result<u8> {
    dev.set_feature(&[REPORT_USER, addr, bank | READ_FLAG, 0x00])?;
    let mut buf = [REPORT_USER, 0, 0, 0];
    dev.get_feature(&mut buf)?;
    Ok(buf[3])
}

/// Burst read via repeated GetFeature(Report 0x41).
/// `report_len` is the payload bytes per report (excluding report ID byte).
pub fn burst_read(
    dev: &HidrawDevice,
    total_bytes: usize,
    report_len: usize,
) -> io::Result<Vec<u8>> {
    let mut result = Vec::with_capacity(total_bytes);
    // Buffer: report ID + payload
    let buf_size = 1 + report_len;
    let mut buf = vec![0u8; buf_size];

    while result.len() < total_bytes {
        buf[0] = REPORT_BURST;
        let n = dev.get_feature(&mut buf)?;
        // Data starts at index 1
        let payload_end = n.min(buf_size);
        let remaining = total_bytes - result.len();
        let take = remaining.min(payload_end - 1);
        result.extend_from_slice(&buf[1..1 + take]);
    }

    Ok(result)
}
