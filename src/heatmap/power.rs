//! The power-state registers of PixArt pads (user bank 0).
//!
//! Two independent mechanisms decide how a PixArt pad saves power. The
//! pad's own run/rest machine (see [`super::rates`]) steps down to REST1 and
//! REST2 after a stretch without touch; every one of those modes keeps
//! scanning and wakes on touch. (FORCE_OP_MODE, 0x16, can pin the pad to
//! one of them, but a pad forced to rest1 or rest2 behaves erratically, so
//! tapview leaves it alone.) Separately, the host's HID-over-I2C SET_POWER(SLEEP) puts the pad
//! into one of two states, picked by a suspend-enable flag: HID sleep,
//! which keeps scanning slowly and wakes on touch, or HID suspend, which
//! stops scanning and wakes only on a GPI wake pin (in practice the click).
//! tapview cannot send SET_POWER, which belongs to the kernel's i2c-hid or
//! the keyboard firmware, but it can choose which of the two a SLEEP means.
//!
//! The flag is 0x8c bit 3 on the PCT1036 (missing from PixArt's PJP360
//! table, whose sleep registers only refer to a HID_SUSPEND_ENABLE; found
//! and confirmed on a Daisy pad, firmware 0x1204) and 0xb6
//! HID_SLEEP_SUSPEND_ENABLE on the PJP343. Both are RAM only, like every
//! user register: the firmware's value is back after a pad reset.

use super::chips::ChipVariant;
use super::protocol::{read_user_reg, write_user_reg};
use super::HidDevice;
use std::io;

const OBS_OP_MODE: u8 = 0x15;

/// What wakes the pad out of a host SET_POWER(SLEEP).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SleepWake {
    /// HID sleep: the pad keeps scanning and a touch wakes it.
    Touch,
    /// HID suspend: scanning stops and only a wake pin (the click) wakes
    /// it. The waking click itself is not reported.
    Click,
}

impl std::fmt::Display for SleepWake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SleepWake::Touch => write!(f, "touch"),
            SleepWake::Click => write!(f, "click only"),
        }
    }
}

/// Where a chip keeps its HID sleep registers.
struct Layout {
    /// The suspend-enable flag: register and bit mask.
    suspend: (u8, u8),
    /// HID_SLEEP_REPORT_RATE, the scan rate of HID sleep.
    hid_sleep_rate: u8,
}

fn layout(chip: ChipVariant) -> Option<Layout> {
    match chip {
        ChipVariant::PCT1036 => Some(Layout {
            suspend: (0x8c, 0x08),
            hid_sleep_rate: 0x8e,
        }),
        ChipVariant::PJP343 => Some(Layout {
            suspend: (0xb6, 0x01),
            hid_sleep_rate: 0xb9,
        }),
        _ => None,
    }
}

/// Whether OBS_OP_MODE is at 0x15 on `chip`.
pub fn has_op_mode(chip: ChipVariant) -> bool {
    chip != ChipVariant::PLP239
}

fn unsupported(chip: ChipVariant, what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        format!("the {} register of the {} is not known", what, chip),
    )
}

/// The pad's power state as far as the registers show it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerStatus {
    /// OBS_OP_MODE: 1 run, 2 rest1, 3 rest2.
    pub op_mode: u8,
    /// `None` on chips whose suspend flag is not known.
    pub sleep_wake: Option<SleepWake>,
    /// HID_SLEEP_REPORT_RATE in Hz, `None` where unknown.
    pub hid_sleep_rate: Option<u8>,
}

/// Read the power registers of `chip` (one to three register reads).
pub async fn read_power<D: HidDevice>(dev: &D, chip: ChipVariant) -> io::Result<PowerStatus> {
    if !has_op_mode(chip) {
        return Err(unsupported(chip, "op mode"));
    }
    let mut status = PowerStatus {
        op_mode: read_user_reg(dev, 0, OBS_OP_MODE).await?,
        sleep_wake: None,
        hid_sleep_rate: None,
    };
    if let Some(l) = layout(chip) {
        let (addr, mask) = l.suspend;
        status.sleep_wake = Some(if read_user_reg(dev, 0, addr).await? & mask != 0 {
            SleepWake::Click
        } else {
            SleepWake::Touch
        });
        status.hid_sleep_rate = Some(read_user_reg(dev, 0, l.hid_sleep_rate).await?);
    }
    Ok(status)
}

/// Write `value` to user bank 0 `addr` and check that it reads back.
async fn write_checked<D: HidDevice>(dev: &D, addr: u8, value: u8) -> io::Result<()> {
    write_user_reg(dev, 0, addr, value).await?;
    let now = read_user_reg(dev, 0, addr).await?;
    if now != value {
        return Err(io::Error::other(format!(
            "0x{:02x} reads back 0x{:02x}, not the 0x{:02x} written",
            addr, now, value
        )));
    }
    Ok(())
}

/// Choose what wakes the pad from the host's next SET_POWER(SLEEP). Only
/// the flag's bit changes; the rest of its register is written back as
/// read (0x8c also holds the PCT1036's PTP/mouse mode and lid function).
pub async fn set_sleep_wake<D: HidDevice>(
    dev: &D,
    chip: ChipVariant,
    wake: SleepWake,
) -> io::Result<()> {
    let (addr, mask) = layout(chip)
        .ok_or_else(|| unsupported(chip, "HID suspend enable"))?
        .suspend;
    let old = read_user_reg(dev, 0, addr).await?;
    let new = match wake {
        SleepWake::Touch => old & !mask,
        SleepWake::Click => old | mask,
    };
    write_checked(dev, addr, new).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// A bank 0 that answers user-register reads and takes writes the way
    /// the pad does.
    struct FakePad {
        bank0: RefCell<[u8; 256]>,
        pending: Cell<u8>,
    }

    impl HidDevice for FakePad {
        async fn set_feature(&self, buf: &[u8]) -> io::Result<()> {
            assert_eq!(buf[0], 0x43, "only user registers are used here");
            match buf[2] {
                0x10 => self.pending.set(buf[1]),
                0x00 => self.bank0.borrow_mut()[buf[1] as usize] = buf[3],
                bank => panic!("bank byte 0x{:02x} is not bank 0", bank),
            }
            Ok(())
        }

        async fn get_feature(&self, buf: &mut [u8]) -> io::Result<usize> {
            buf[3] = self.bank0.borrow()[self.pending.get() as usize];
            Ok(4)
        }
    }

    fn pad(regs: &[(u8, u8)]) -> FakePad {
        let mut bank0 = [0u8; 256];
        for &(a, v) in regs {
            bank0[a as usize] = v;
        }
        FakePad {
            bank0: RefCell::new(bank0),
            pending: Cell::new(0),
        }
    }

    fn reg(dev: &FakePad, addr: u8) -> u8 {
        dev.bank0.borrow()[addr as usize]
    }

    #[test]
    fn pct1036_suspend_is_0x8c_bit3_and_keeps_the_other_bits() {
        // Daisy firmware 0x1204 as read: PTP default mode, rest2.
        let dev = pad(&[(0x8c, 0x01), (0x8e, 4), (0x15, 3)]);
        let s = pollster::block_on(read_power(&dev, ChipVariant::PCT1036)).unwrap();
        assert_eq!(
            s,
            PowerStatus {
                op_mode: 3,
                sleep_wake: Some(SleepWake::Touch),
                hid_sleep_rate: Some(4),
            }
        );
        pollster::block_on(set_sleep_wake(&dev, ChipVariant::PCT1036, SleepWake::Click)).unwrap();
        assert_eq!(reg(&dev, 0x8c), 0x09);
        pollster::block_on(set_sleep_wake(&dev, ChipVariant::PCT1036, SleepWake::Touch)).unwrap();
        assert_eq!(reg(&dev, 0x8c), 0x01);
    }

    #[test]
    fn pjp343_suspend_is_0xb6() {
        // The laptop's built-in PJP343 as read.
        let dev = pad(&[(0x15, 1), (0xb6, 0), (0xb9, 5)]);
        pollster::block_on(set_sleep_wake(&dev, ChipVariant::PJP343, SleepWake::Click)).unwrap();
        assert_eq!(reg(&dev, 0xb6), 1);
        let s = pollster::block_on(read_power(&dev, ChipVariant::PJP343)).unwrap();
        assert_eq!(s.sleep_wake, Some(SleepWake::Click));
        assert_eq!(s.hid_sleep_rate, Some(5));
    }

    #[test]
    fn unknown_chips_have_no_suspend_flag() {
        let dev = pad(&[(0x15, 1)]);
        let s = pollster::block_on(read_power(&dev, ChipVariant::PJP274)).unwrap();
        assert_eq!(s.sleep_wake, None);
        let err = pollster::block_on(set_sleep_wake(&dev, ChipVariant::PJP274, SleepWake::Click))
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        assert!(pollster::block_on(read_power(&dev, ChipVariant::PLP239)).is_err());
    }
}
