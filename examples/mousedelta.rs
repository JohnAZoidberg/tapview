//! Diagnostic: measure a PTP pad's mouse-mode resolution on macOS.
//!
//! Switches the pad to Input Mode 0 (mouse), then sums the relative X/Y of
//! its mouse reports while macOS keeps moving the cursor. Drag a finger a
//! known distance and divide: counts / mm. Enter prints the totals and
//! starts over.
//!
//! Usage:
//!   cargo run --example mousedelta              # first PTP pad
//!   cargo run --example mousedelta -- <device>  # index or DEVICE from --list

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("macos only");
}

#[cfg(target_os = "macos")]
fn main() {
    use std::io::Write;
    use std::sync::mpsc;
    use tapview::config::{self, ConfigBackend};
    use tapview::hid::macos::{find_ptp_touchpads, MacHidDevice};
    use tapview::hid::{extract_signed, ReportKind, ReportLayout};

    let pads = find_ptp_touchpads().expect("enumerate HID devices");
    let pad = match std::env::args().nth(1) {
        Some(w) => tapview::discovery::find_device(&pads, &w).cloned(),
        None => pads.first().cloned(),
    }
    .expect("no PTP touchpad found");
    eprintln!("pad: {}", pad);

    let dev = MacHidDevice::open(&pad.devnode).expect("open");
    let layout = ReportLayout::parse(&dev.report_descriptor().expect("descriptor"));

    // The relative X/Y of the mouse collection: the only X/Y Input fields
    // that are not absolute. The PTP finger X/Y are absolute.
    let rel = |usage| {
        layout
            .fields
            .iter()
            .find(|f| {
                f.kind == ReportKind::Input
                    && f.usage_page == 0x01
                    && f.usage == usage
                    && f.logical_min < 0
            })
            .cloned()
            .expect("no relative X/Y in the descriptor")
    };
    let (fx, fy) = (rel(0x30), rel(0x31));
    assert_eq!(fx.report_id, fy.report_id);
    let id = fx.report_id;
    eprintln!(
        "mouse report {}: X {} bits @ {}, Y {} bits @ {}, physical {}..{} unit 0x{:x}",
        id,
        fx.bit_size,
        fx.bit_offset,
        fy.bit_size,
        fy.bit_offset,
        fx.physical_min,
        fx.physical_max,
        fx.unit
    );

    match config::discover(&pad.devnode) {
        Some(d) => {
            let mut backend = d.backend;
            match pollster::block_on(backend.write_input_mode(0)) {
                Ok(()) => eprintln!("input mode set to 0 (mouse)"),
                Err(e) => eprintln!("could not set mouse mode: {}", e),
            }
        }
        None => eprintln!("no PTP config on this pad; reading it as it is"),
    }

    let (enter_tx, enter_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        while std::io::stdin().read_line(&mut line).is_ok_and(|n| n > 0) {
            let _ = enter_tx.send(());
        }
    });

    eprintln!("drag a finger a known distance; Enter prints the totals and resets");
    let mut buf = vec![0u8; layout.report_bytes(ReportKind::Input, id).max(64) + 1];
    let (mut sx, mut sy, mut ax, mut ay, mut reports) = (0i64, 0i64, 0i64, 0i64, 0u64);
    loop {
        if enter_rx.try_recv().is_ok() {
            println!(
                "\rtotal: x {} y {} (|x| {} |y| {}) over {} reports",
                sx, sy, ax, ay, reports
            );
            (sx, sy, ax, ay, reports) = (0, 0, 0, 0, 0);
        }
        let Some(n) = dev.read_report(&mut buf, 50).expect("read") else {
            continue;
        };
        let report = &buf[..n];
        let payload = if id != 0 {
            match report.split_first() {
                Some((&rid, rest)) if rid == id => rest,
                _ => continue,
            }
        } else {
            report
        };
        let dx = extract_signed(payload, fx.bit_offset, fx.bit_size) as i64;
        let dy = extract_signed(payload, fy.bit_offset, fy.bit_size) as i64;
        sx += dx;
        sy += dy;
        ax += dx.abs();
        ay += dy.abs();
        reports += 1;
        eprint!("\rx {:6} y {:6}  last ({:4},{:4})   ", sx, sy, dx, dy);
        let _ = std::io::stderr().flush();
    }
}
