use super::protocol::{burst_read, read_reg, read_user_reg, write_reg};
use super::HidDevice;
use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipVariant {
    PJP274,
    PJP343,
    PJP255,
    PJP215,
    PLP239,
    PCT1036,
}

impl std::fmt::Display for ChipVariant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChipVariant::PJP274 => write!(f, "PJP274"),
            ChipVariant::PJP343 => write!(f, "PJP343"),
            ChipVariant::PJP255 => write!(f, "PJP255"),
            ChipVariant::PJP215 => write!(f, "PJP215"),
            ChipVariant::PLP239 => write!(f, "PLP239"),
            ChipVariant::PCT1036 => write!(f, "PCT1036"),
        }
    }
}

/// Read Part ID from Bank 0, regs 0x78 (low) and 0x79 (high).
pub async fn identify_chip<D: HidDevice>(dev: &D) -> io::Result<ChipVariant> {
    let lo = read_reg(dev, 0, 0x78).await? as u16;
    let hi = read_reg(dev, 0, 0x79).await? as u16;
    let part_id = lo | (hi << 8);

    match part_id {
        0x0274 => Ok(ChipVariant::PJP274),
        0x0343 => Ok(ChipVariant::PJP343),
        0x0255 => Ok(ChipVariant::PJP255),
        0x0215 => Ok(ChipVariant::PJP215),
        0x0239 => Ok(ChipVariant::PLP239),
        0x0360 => Ok(ChipVariant::PCT1036),
        _ => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("Unknown PixArt chip Part ID: 0x{:04X}", part_id),
        )),
    }
}

/// Read matrix dimensions as (rows, cols) from chip-specific registers.
pub async fn read_matrix_dims<D: HidDevice>(
    dev: &D,
    chip: ChipVariant,
) -> io::Result<(usize, usize)> {
    match chip {
        ChipVariant::PJP274 | ChipVariant::PJP343 | ChipVariant::PCT1036 => {
            let rows = read_user_reg(dev, 0, 0x6E).await? as usize;
            let cols = read_user_reg(dev, 0, 0x6F).await? as usize;
            Ok((rows, cols))
        }
        ChipVariant::PJP255 | ChipVariant::PJP215 => {
            let drives = read_user_reg(dev, 0, 0x5A).await? as usize;
            let senses = read_user_reg(dev, 0, 0x59).await? as usize;
            Ok((drives, senses))
        }
        ChipVariant::PLP239 => {
            // Bank 9 (AFE), values are count-1
            // Drives = cols (fast/stride axis), senses = rows
            let drives = read_reg(dev, 9, 0x01).await? as usize + 1;
            let senses = read_reg(dev, 9, 0x02).await? as usize + 1;
            Ok((senses, drives))
        }
    }
}

/// Read one raw capacitive frame. Returns signed 16-bit values in row-major order.
pub async fn read_frame<D: HidDevice>(
    dev: &D,
    chip: ChipVariant,
    rows: usize,
    cols: usize,
    burst_len: usize,
) -> io::Result<Vec<i16>> {
    let total_bytes = rows * cols * 2;

    let raw = match chip {
        ChipVariant::PJP274 | ChipVariant::PJP343 | ChipVariant::PCT1036 => {
            read_frame_pjp274(dev, rows, cols, total_bytes, burst_len).await?
        }
        ChipVariant::PJP255 | ChipVariant::PJP215 => {
            read_frame_pjp255(dev, total_bytes, burst_len).await?
        }
        ChipVariant::PLP239 => read_frame_plp239(dev, total_bytes, burst_len).await?,
    };

    // Convert LE bytes to i16
    Ok(raw
        .as_chunks::<2>().0.iter()
        .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        .collect())
}

async fn read_frame_pjp274<D: HidDevice>(
    dev: &D,
    rows: usize,
    cols: usize,
    total_bytes: usize,
    burst_len: usize,
) -> io::Result<Vec<u8>> {
    // 1. Configure matrix dimensions in IO bank (Bank 6)
    //    0x0E = numDrives-1 (cols), 0x0F = numSenses-1 (rows)
    write_reg(dev, 6, 0x0E, (cols - 1) as u8).await?;
    write_reg(dev, 6, 0x0F, (rows - 1) as u8).await?;

    // 2. Select SRAM = Frame0 (0x05)
    write_reg(dev, 6, 0x09, 0x05).await?;

    // 3. Assert NCS
    write_reg(dev, 6, 0x0A, 0x00).await?;

    // 4. Burst read
    let data = burst_read(dev, total_bytes, burst_len).await?;

    // 5. Deassert NCS
    write_reg(dev, 6, 0x0A, 0x01).await?;

    Ok(data)
}

async fn read_frame_pjp255<D: HidDevice>(
    dev: &D,
    total_bytes: usize,
    burst_len: usize,
) -> io::Result<Vec<u8>> {
    // 1. Enable frame buffer reading
    write_reg(dev, 1, 0x0D, 0x40).await?;
    write_reg(dev, 1, 0x0E, 0x06).await?;

    // 2. Select SRAM (Frame0 = 0x05) and assert NCS (Bank 2)
    write_reg(dev, 2, 0x09, 0x05).await?;
    write_reg(dev, 2, 0x0A, 0x00).await?;

    // 3. Burst read
    let data = burst_read(dev, total_bytes, burst_len).await?;

    // 4. Deassert NCS
    write_reg(dev, 2, 0x0A, 0x01).await?;

    Ok(data)
}

async fn read_frame_plp239<D: HidDevice>(
    dev: &D,
    total_bytes: usize,
    burst_len: usize,
) -> io::Result<Vec<u8>> {
    // 1. Unlock level-0 protection
    write_reg(dev, 6, 0x20, 0xCC).await?;

    // 2. Flash read command
    write_reg(dev, 6, 0x25, 0x77).await?;

    // 3. Poll finish bit (Bank 6, 0x27, bit 0)
    for _ in 0..1000 {
        let status = read_reg(dev, 6, 0x27).await?;
        if status & 0x01 != 0 {
            break;
        }
    }

    // 4. Finalize read command
    write_reg(dev, 6, 0x25, 0xDD).await?;

    // 5. Reset SRAM read offset (Bank 4)
    write_reg(dev, 4, 0x1C, 0x00).await?;
    write_reg(dev, 4, 0x1D, 0x00).await?;

    // 6. SRAM read mode
    write_reg(dev, 6, 0x25, 0x11).await?;

    // 7. Burst read
    let data = burst_read(dev, total_bytes, burst_len).await?;

    // 8. Finalize
    write_reg(dev, 6, 0x25, 0xDD).await?;

    Ok(data)
}
