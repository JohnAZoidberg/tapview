//! The run/rest-mode rate registers of PixArt pads (user bank 0, 0x10-0x1f).
//!
//! A PixArt pad scans at its report rate while touched and drops into two
//! slower idle modes (REST1, then REST2) after a stretch without touch. The
//! rates, the delays and the current mode are user registers, loaded from
//! the firmware's parameter section at boot. Names and units are from
//! PixArt's PTHIDUtility register tables. The PJP274 family (PJP215, PJP255,
//! PJP274 and the PCT1036 / PJP360) and the PJP343 disagree about 0x14 and
//! 0x18-0x1b, so the layout is per chip.
//!
//! The tables call 0x12/0x13 a u16 report rate, but firmware parameter
//! sections only ever set 0x12, and a PJP343 reads 0x8c (140) back from
//! 0x13, which as a high byte would make 35980 Hz. So 0x12 is shown as the
//! rate and 0x13 raw. Watched through a full idle cycle, that PJP343's 0x12
//! followed the mode (140 in run, 20 in rest2) while 0x13 stayed at 140:
//! there 0x12 is the live scan rate, and 0x13 looks like the configured run
//! rate. `read_status` follows both bytes.

use super::chips::ChipVariant;
use super::protocol::read_user_reg;
use super::HidDevice;
use std::io;

const OBS_REPORT_RATE: u8 = 0x10; // u16 LE, read-only
const OBS_OP_MODE: u8 = 0x15;

#[derive(Clone, Copy)]
enum Kind {
    Hz8,
    Hz16,
    /// u16 LE in 50 ms units.
    Delay50ms16,
    /// u8 in seconds.
    DelaySecs8,
    /// Shown as hex and decimal, meaning unconfirmed.
    Raw,
    OpMode,
    SleepMode,
}

use Kind::*;

type Layout = &'static [(u8, &'static str, Kind)];

const PJP274_LAYOUT: Layout = &[
    (OBS_OP_MODE, "OBS_OP_MODE", OpMode),
    (0x16, "FORCE_OP_MODE", OpMode),
    (OBS_REPORT_RATE, "OBS_REPORT_RATE", Hz16),
    (0x12, "MAX_REPORT_RATE", Hz8),
    (0x13, "MAX_REPORT_RATE[15:8]", Raw),
    (0x14, "MIN_REPORT_RATE", Hz8),
    (0x18, "REST1_DS", Delay50ms16),
    (0x1a, "REST2_DS", Delay50ms16),
    (0x1c, "REST1_FRAME_RATE", Hz8),
    (0x1d, "REST2_FRAME_RATE", Hz8),
    (0x1e, "REST1_SLEEP_MODE", SleepMode),
    (0x1f, "REST2_SLEEP_MODE", SleepMode),
];

const PJP343_LAYOUT: Layout = &[
    (OBS_OP_MODE, "OBS_OP_MODE", OpMode),
    (0x16, "FORCE_OP_MODE", OpMode),
    (OBS_REPORT_RATE, "OBS_REPORT_RATE", Hz16),
    (0x12, "SCAN_REPORT_RATE", Hz8),
    (0x13, "SCAN_REPORT_RATE[15:8]", Raw),
    (0x14, "FAKE_REPORT_RATE", Hz8),
    (0x18, "REST1_DS", DelaySecs8),
    (0x19, "REST2_DS", DelaySecs8),
    (0x1c, "REST1_FRAME_RATE", Hz8),
    (0x1d, "REST2_FRAME_RATE", Hz8),
    (0x1e, "REST1_SLEEP_MODE", SleepMode),
    (0x1f, "REST2_SLEEP_MODE", SleepMode),
];

fn layout(chip: ChipVariant) -> io::Result<Layout> {
    match chip {
        ChipVariant::PJP274 | ChipVariant::PJP255 | ChipVariant::PJP215 | ChipVariant::PCT1036 => {
            Ok(PJP274_LAYOUT)
        }
        ChipVariant::PJP343 => Ok(PJP343_LAYOUT),
        ChipVariant::PLP239 => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "rate registers of the PLP239 are not known",
        )),
    }
}

/// OBS_OP_MODE / FORCE_OP_MODE value as a word.
pub fn op_mode_label(mode: u8) -> &'static str {
    match mode {
        0 => "auto",
        1 => "run",
        2 => "rest1",
        3 => "rest2",
        _ => "unknown",
    }
}

fn sleep_mode_label(mode: u8) -> &'static str {
    match mode {
        0 => "light",
        1 => "normal",
        2 => "deep",
        _ => "unknown",
    }
}

async fn read_user_u16<D: HidDevice>(dev: &D, addr: u8) -> io::Result<u16> {
    let lo = read_user_reg(dev, 0, addr).await? as u16;
    let hi = read_user_reg(dev, 0, addr + 1).await? as u16;
    Ok(lo | (hi << 8))
}

/// One register as read, formatted for display.
pub struct Register {
    pub addr: u8,
    pub name: &'static str,
    pub value: String,
}

/// Read every rate register of `chip`'s layout, in display order.
pub async fn read_rates<D: HidDevice>(dev: &D, chip: ChipVariant) -> io::Result<Vec<Register>> {
    let mut out = Vec::new();
    for &(addr, name, kind) in layout(chip)? {
        let value = match kind {
            Hz8 => format!("{} Hz", read_user_reg(dev, 0, addr).await?),
            Hz16 => format!("{} Hz", read_user_u16(dev, addr).await?),
            Delay50ms16 => {
                let v = read_user_u16(dev, addr).await?;
                format!("{} x 50 ms = {:.2} s", v, v as f32 * 0.05)
            }
            DelaySecs8 => format!("{} s", read_user_reg(dev, 0, addr).await?),
            Raw => {
                let v = read_user_reg(dev, 0, addr).await?;
                format!("0x{:02x} ({})", v, v)
            }
            OpMode => {
                let v = read_user_reg(dev, 0, addr).await?;
                format!("{} ({})", op_mode_label(v), v)
            }
            SleepMode => {
                let v = read_user_reg(dev, 0, addr).await?;
                format!("{} ({})", sleep_mode_label(v), v)
            }
        };
        out.push(Register { addr, name, value });
    }
    Ok(out)
}

/// What the pad is doing right now: the read-only mode and observed rate,
/// plus the report-rate register pair, all at the same address in every
/// layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status {
    pub op_mode: u8,
    pub obs_report_rate: u16,
    /// 0x12 and 0x13 as read.
    pub report_rate_reg: [u8; 2],
}

/// Read the current mode, observed report rate and 0x12/0x13 (five register
/// reads).
pub async fn read_status<D: HidDevice>(dev: &D) -> io::Result<Status> {
    Ok(Status {
        report_rate_reg: [
            read_user_reg(dev, 0, 0x12).await?,
            read_user_reg(dev, 0, 0x13).await?,
        ],
        op_mode: read_user_reg(dev, 0, OBS_OP_MODE).await?,
        obs_report_rate: read_user_u16(dev, OBS_REPORT_RATE).await?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// Answers user-register reads from a 256-byte bank 0, the way the pad
    /// does: a SetFeature(0x43) naming the register, then a GetFeature(0x43)
    /// with the value in the last byte.
    struct FakePad {
        bank0: [u8; 256],
        pending: Cell<u8>,
    }

    impl HidDevice for FakePad {
        async fn set_feature(&self, buf: &[u8]) -> io::Result<()> {
            assert_eq!(buf[0], 0x43, "only user registers are read here");
            assert_eq!(buf[2], 0x10, "a read of bank 0");
            self.pending.set(buf[1]);
            Ok(())
        }

        async fn get_feature(&self, buf: &mut [u8]) -> io::Result<usize> {
            buf[3] = self.bank0[self.pending.get() as usize];
            Ok(4)
        }
    }

    fn pad(regs: &[(u8, u8)]) -> FakePad {
        let mut bank0 = [0u8; 256];
        for &(a, v) in regs {
            bank0[a as usize] = v;
        }
        FakePad {
            bank0,
            pending: Cell::new(0),
        }
    }

    fn find<'a>(regs: &'a [Register], name: &str) -> &'a str {
        &regs.iter().find(|r| r.name == name).unwrap().value
    }

    #[test]
    fn pct1036_matches_daisy_firmware_1204() {
        // The parameter-section values of Daisy touchpad firmware 0x1204.
        let dev = pad(&[
            (0x12, 140),
            (0x14, 1),
            (0x18, 0xc8),
            (0x1a, 0xc8),
            (0x1c, 20),
            (0x1d, 20),
            (0x1e, 2),
            (0x1f, 2),
            (0x15, 2),
            (0x10, 0x8c),
        ]);
        let regs = pollster::block_on(read_rates(&dev, ChipVariant::PCT1036)).unwrap();
        assert_eq!(find(&regs, "MAX_REPORT_RATE"), "140 Hz");
        assert_eq!(find(&regs, "REST1_DS"), "200 x 50 ms = 10.00 s");
        assert_eq!(find(&regs, "REST2_FRAME_RATE"), "20 Hz");
        assert_eq!(find(&regs, "REST2_SLEEP_MODE"), "deep (2)");
        assert_eq!(find(&regs, "OBS_OP_MODE"), "rest1 (2)");
        let status = pollster::block_on(read_status(&dev)).unwrap();
        assert_eq!(
            status,
            Status {
                op_mode: 2,
                obs_report_rate: 140,
                report_rate_reg: [140, 0],
            }
        );
    }

    #[test]
    fn pjp343_delays_are_seconds_and_0x14_is_fake_rate() {
        // As the laptop's built-in PJP343 reads (0x13 included).
        let dev = pad(&[
            (0x12, 140),
            (0x13, 0x8c),
            (0x14, 140),
            (0x18, 30),
            (0x19, 0),
            (0x1a, 0xff),
        ]);
        let regs = pollster::block_on(read_rates(&dev, ChipVariant::PJP343)).unwrap();
        assert_eq!(find(&regs, "SCAN_REPORT_RATE"), "140 Hz");
        assert_eq!(find(&regs, "SCAN_REPORT_RATE[15:8]"), "0x8c (140)");
        assert_eq!(find(&regs, "FAKE_REPORT_RATE"), "140 Hz");
        assert_eq!(find(&regs, "REST1_DS"), "30 s");
        assert_eq!(find(&regs, "REST2_DS"), "0 s");
        assert!(
            regs.iter().all(|r| r.addr != 0x1a),
            "0x1a is a button enable here"
        );
    }

    #[test]
    fn plp239_is_unsupported() {
        let dev = pad(&[]);
        let err = pollster::block_on(read_rates(&dev, ChipVariant::PLP239))
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }
}
