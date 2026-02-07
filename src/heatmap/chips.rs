use super::hidraw::HidrawDevice;
use super::protocol::{burst_read, read_reg, read_user_reg, write_reg};
use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipVariant {
    PJP274,
    PJP343,
    PJP255,
    PJP215,
    PLP239,
}

impl std::fmt::Display for ChipVariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChipVariant::PJP274 => write!(f, "PJP274"),
            ChipVariant::PJP343 => write!(f, "PJP343"),
            ChipVariant::PJP255 => write!(f, "PJP255"),
            ChipVariant::PJP215 => write!(f, "PJP215"),
            ChipVariant::PLP239 => write!(f, "PLP239"),
        }
    }
}

/// Read Part ID from Bank 0, regs 0x78 (low) and 0x79 (high).
pub fn identify_chip(dev: &HidrawDevice) -> io::Result<ChipVariant> {
    let lo = read_reg(dev, 0, 0x78)? as u16;
    let hi = read_reg(dev, 0, 0x79)? as u16;
    let part_id = lo | (hi << 8);

    match part_id {
        0x0274 => Ok(ChipVariant::PJP274),
        0x0343 => Ok(ChipVariant::PJP343),
        0x0255 => Ok(ChipVariant::PJP255),
        0x0215 => Ok(ChipVariant::PJP215),
        0x0239 => Ok(ChipVariant::PLP239),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("Unknown PixArt chip Part ID: 0x{:04X}", part_id),
        )),
    }
}

/// Read matrix dimensions as (rows, cols) from chip-specific registers.
pub fn read_matrix_dims(dev: &HidrawDevice, chip: ChipVariant) -> io::Result<(usize, usize)> {
    match chip {
        ChipVariant::PJP274 | ChipVariant::PJP343 => {
            let rows = read_user_reg(dev, 0, 0x6E)? as usize;
            let cols = read_user_reg(dev, 0, 0x6F)? as usize;
            Ok((rows, cols))
        }
        ChipVariant::PJP255 | ChipVariant::PJP215 => {
            let drives = read_user_reg(dev, 0, 0x5A)? as usize;
            let senses = read_user_reg(dev, 0, 0x59)? as usize;
            Ok((drives, senses))
        }
        ChipVariant::PLP239 => {
            // Bank 9 (AFE), values are count-1
            let drives = read_reg(dev, 9, 0x01)? as usize + 1;
            let senses = read_reg(dev, 9, 0x02)? as usize + 1;
            Ok((drives, senses))
        }
    }
}

/// Read one raw capacitive frame. Returns signed 16-bit values in row-major order.
pub fn read_frame(
    dev: &HidrawDevice,
    chip: ChipVariant,
    rows: usize,
    cols: usize,
    burst_len: usize,
) -> io::Result<Vec<i16>> {
    let total_bytes = rows * cols * 2;

    let raw = match chip {
        ChipVariant::PJP274 | ChipVariant::PJP343 => {
            read_frame_pjp274(dev, rows, cols, total_bytes, burst_len)?
        }
        ChipVariant::PJP255 | ChipVariant::PJP215 => {
            read_frame_pjp255(dev, total_bytes, burst_len)?
        }
        ChipVariant::PLP239 => read_frame_plp239(dev, total_bytes, burst_len)?,
    };

    // Convert LE bytes to i16
    Ok(raw
        .chunks_exact(2)
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect())
}

fn read_frame_pjp274(
    dev: &HidrawDevice,
    rows: usize,
    cols: usize,
    total_bytes: usize,
    burst_len: usize,
) -> io::Result<Vec<u8>> {
    // 1. Configure matrix dimensions in IO bank (Bank 6)
    //    0x0E = numDrives-1 (cols), 0x0F = numSenses-1 (rows)
    write_reg(dev, 6, 0x0E, (cols - 1) as u8)?;
    write_reg(dev, 6, 0x0F, (rows - 1) as u8)?;

    // 2. Select SRAM = Frame0 (0x05)
    write_reg(dev, 6, 0x09, 0x05)?;

    // 3. Assert NCS
    write_reg(dev, 6, 0x0A, 0x00)?;

    // 4. Burst read
    let data = burst_read(dev, total_bytes, burst_len)?;

    // 5. Deassert NCS
    write_reg(dev, 6, 0x0A, 0x01)?;

    Ok(data)
}

fn read_frame_pjp255(
    dev: &HidrawDevice,
    total_bytes: usize,
    burst_len: usize,
) -> io::Result<Vec<u8>> {
    // 1. Enable frame buffer reading
    write_reg(dev, 1, 0x0D, 0x40)?;
    write_reg(dev, 1, 0x0E, 0x06)?;

    // 2. Select SRAM (Frame0 = 0x05) and assert NCS (Bank 2)
    write_reg(dev, 2, 0x09, 0x05)?;
    write_reg(dev, 2, 0x0A, 0x00)?;

    // 3. Burst read
    let data = burst_read(dev, total_bytes, burst_len)?;

    // 4. Deassert NCS
    write_reg(dev, 2, 0x0A, 0x01)?;

    Ok(data)
}

fn read_frame_plp239(
    dev: &HidrawDevice,
    total_bytes: usize,
    burst_len: usize,
) -> io::Result<Vec<u8>> {
    // 1. Unlock level-0 protection
    write_reg(dev, 6, 0x20, 0xCC)?;

    // 2. Flash read command
    write_reg(dev, 6, 0x25, 0x77)?;

    // 3. Poll finish bit (Bank 6, 0x27, bit 0)
    for _ in 0..1000 {
        let status = read_reg(dev, 6, 0x27)?;
        if status & 0x01 != 0 {
            break;
        }
    }

    // 4. Finalize read command
    write_reg(dev, 6, 0x25, 0xDD)?;

    // 5. Reset SRAM read offset (Bank 4)
    write_reg(dev, 4, 0x1C, 0x00)?;
    write_reg(dev, 4, 0x1D, 0x00)?;

    // 6. SRAM read mode
    write_reg(dev, 6, 0x25, 0x11)?;

    // 7. Burst read
    let data = burst_read(dev, total_bytes, burst_len)?;

    // 8. Finalize
    write_reg(dev, 6, 0x25, 0xDD)?;

    Ok(data)
}
