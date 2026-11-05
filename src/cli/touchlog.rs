//! `--log-touches FILE`: run headless and append one CSV row per touch, to
//! measure how pads are really used (for power modelling: how often, how
//! long and with how many fingers they are touched, and how long they then
//! sit untouched).
//!
//! Every touchpad is followed, including ones attached later (rescanned
//! every few seconds), unless `--device` names one. A touch runs from the
//! first contact down to the last one up, so a two-finger scroll that
//! starts with one finger is one touch. Rows (`time` in Unix seconds):
//!
//! | event   | time          | device | duration_s       | max_contacts | contacts | clicks | detail |
//! |---------|---------------|--------|------------------|--------------|----------|--------|--------|
//! | `log`   | logger started |       |                  |              |          |        | version |
//! | `start` | pad found     | pad    |                  |              |          |        | node   |
//! | `touch` | first contact | pad    | until last lifts | most at once | landed   | button presses | |
//! | `lost`  | pad gone      | pad    |                  |              |          |        | why    |
//! | `sleep` | system slept  |        | how long         |              |          |        |        |
//! | `alive` | still logging (after 5 min without a row) | | |        |          |        |        |
//!
//! A pad is covered from its `start` to its `lost`, or to the last row
//! before the next `log` (the logger was stopped).

use crate::discovery::DeviceInfo;
use crate::input::TouchState;
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A lift followed by a contact within this time is the same touch. Covers
/// Windows' hybrid-mode reports (the continuation reports parse as empty
/// frames) and is far below any timeout a pad's run state has.
const DEBOUNCE: Duration = Duration::from_millis(50);
/// How long to block waiting for a frame while nothing is pending.
const TICK: Duration = Duration::from_secs(1);
/// The wall clock moving this much further than a wait can last means the
/// system was asleep (or the process frozen, as in Windows modern standby).
const SLEEP_SLACK: Duration = Duration::from_secs(4);
const HEARTBEAT: Duration = Duration::from_secs(300);
/// How often to look for pads that came or went. Windows also rescans
/// when a report arrives from a pad it has not seen, so it can look less
/// often (enumerating through SetupAPI is not free).
#[cfg(not(target_os = "windows"))]
const RESCAN: Duration = Duration::from_secs(2);
#[cfg(target_os = "windows")]
const RESCAN: Duration = Duration::from_secs(10);

const HEADER: &str = "event,time,device,duration_s,max_contacts,contacts,clicks,detail";

#[derive(Debug, Clone, PartialEq)]
pub struct TouchRow {
    /// Unix seconds when the first contact was seen.
    pub start: f64,
    pub duration_s: f64,
    pub max_contacts: usize,
    pub contacts: u32,
    pub clicks: u32,
}

#[derive(Debug)]
struct Touch {
    start: f64,
    start_us: u64,
    last_us: u64,
    /// Stamp of the frame with no contacts that may end the touch.
    end_us: Option<u64>,
    max: usize,
    contacts: u32,
    clicks: u32,
    prev_n: usize,
    prev_click: bool,
}

impl Touch {
    fn finish(self) -> TouchRow {
        let end = self.end_us.unwrap_or(self.last_us);
        TouchRow {
            start: self.start,
            duration_s: end.saturating_sub(self.start_us) as f64 / 1e6,
            max_contacts: self.max,
            contacts: self.contacts,
            clicks: self.clicks,
        }
    }
}

/// Turns frames into touches. Pure, so the edge cases are testable.
#[derive(Debug, Default)]
pub struct Activity {
    cur: Option<Touch>,
}

impl Activity {
    /// One frame: `n` contacts down, `click` any button held, `t_us` the
    /// backend's stamp, `wall` Unix seconds at receipt. Returns a touch that
    /// this frame shows to be over.
    pub fn frame(&mut self, n: usize, click: bool, t_us: u64, wall: f64) -> Option<TouchRow> {
        let mut done = None;
        if let Some(e) = self.cur.as_ref().and_then(|c| c.end_us) {
            if t_us >= e + DEBOUNCE.as_micros() as u64 {
                done = self.close();
            }
        }
        if n == 0 {
            if let Some(c) = &mut self.cur {
                c.end_us.get_or_insert(t_us);
            }
            return done;
        }
        let c = self.cur.get_or_insert(Touch {
            start: wall,
            start_us: t_us,
            last_us: t_us,
            end_us: None,
            max: 0,
            contacts: 0,
            clicks: 0,
            prev_n: 0,
            prev_click: false,
        });
        c.end_us = None;
        c.last_us = t_us;
        if n > c.prev_n {
            c.contacts += (n - c.prev_n) as u32;
        }
        c.prev_n = n;
        c.max = c.max.max(n);
        if click && !c.prev_click {
            c.clicks += 1;
        }
        c.prev_click = click;
        done
    }

    /// Whether the last frame had no contacts and the touch awaits closing.
    pub fn ending(&self) -> bool {
        self.cur.as_ref().is_some_and(|c| c.end_us.is_some())
    }

    /// End the current touch, if any, at its lift (or its last frame).
    pub fn close(&mut self) -> Option<TouchRow> {
        self.cur.take().map(Touch::finish)
    }
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn contacts(state: &TouchState) -> usize {
    state.touches.iter().filter(|t| t.used).count()
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

struct Log {
    file: File,
    last_write: Instant,
}

impl Log {
    fn open(path: &str) -> std::io::Result<Self> {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        if file.metadata()?.len() == 0 {
            writeln!(file, "{}", HEADER)?;
        }
        Ok(Self {
            file,
            last_write: Instant::now(),
        })
    }

    /// `rest`: the columns after `device`.
    fn row(&mut self, event: &str, time: f64, device: &str, rest: &str) {
        let device = if device.is_empty() {
            String::new()
        } else {
            quote(device)
        };
        if let Err(e) = writeln!(self.file, "{},{:.3},{},{}", event, time, device, rest) {
            log::error!("log-touches: write failed: {}", e);
        }
        self.last_write = Instant::now();
    }

    fn touch(&mut self, device: &str, t: &TouchRow) {
        let rest = format!(
            "{:.3},{},{},{},",
            t.duration_s, t.max_contacts, t.contacts, t.clicks
        );
        self.row("touch", t.start, device, &rest);
    }

    fn note(&mut self, event: &str, time: f64, device: &str, detail: &str) {
        self.row(event, time, device, &format!(",,,,{}", quote(detail)));
    }
}

/// Same pad after a reconnect: the node may have moved (`event22` to
/// `event25`), so match what the device says it is.
fn same_device(a: &DeviceInfo, b: &DeviceInfo) -> bool {
    a.name == b.name && a.vendor_id == b.vendor_id && a.product_id == b.product_id
}

/// What the reader threads send, keyed by the opened pad.
enum Msg {
    Frame(String, Box<TouchState>),
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    Lost(String, String),
    /// The Windows Raw Input reader died: no pad can be followed.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    Fatal(String),
}

/// A pad being followed.
struct Pad {
    label: String,
    /// Its discovery identity: the node on Linux, the interface path on
    /// Windows. Gone from a rescan means unplugged.
    ident: String,
    act: Activity,
    /// When the frame that may end the current touch arrived.
    end_seen: Option<Instant>,
}

#[cfg(not(target_os = "windows"))]
fn ident(d: &DeviceInfo) -> String {
    d.devnode.display().to_string()
}

#[cfg(target_os = "windows")]
fn ident(d: &DeviceInfo) -> String {
    crate::input::windows_backend::normalize_device_path(&d.devnode.to_string_lossy())
}

/// Open the pad and start a thread forwarding its frames. Returns the key
/// its frames come with.
#[cfg(not(target_os = "windows"))]
fn open_pad(d: &DeviceInfo, gen: u64, tx: &mpsc::Sender<Msg>) -> Result<String, String> {
    use crate::input::InputBackend;
    #[cfg(target_os = "linux")]
    let backend: Box<dyn InputBackend> = Box::new(
        crate::input::evdev_backend::EvdevBackend::open(&d.devnode).map_err(|e| e.to_string())?,
    );
    #[cfg(target_os = "macos")]
    let backend: Box<dyn InputBackend> = if crate::input::macos_mt::is_mt_path(&d.devnode) {
        Box::new(crate::input::macos_mt::MtBackend::open(&d.devnode).map_err(|e| e.to_string())?)
    } else {
        Box::new(
            crate::input::macos_backend::MacBackend::open(&d.devnode).map_err(|e| e.to_string())?,
        )
    };
    // A node can be reused by the next device before this thread notices
    // the old one is gone, so the key carries a generation.
    let key = format!("{}#{}", ident(d), gen);
    let (k, tx) = (key.clone(), tx.clone());
    std::thread::spawn(move || {
        let mut backend = backend;
        loop {
            let msg = match backend.wait_events(TICK) {
                Ok(None) => continue,
                Ok(Some(state)) => Msg::Frame(k.clone(), Box::new(state)),
                Err(e) => Msg::Lost(k.clone(), e.to_string()),
            };
            let lost = matches!(msg, Msg::Lost(..));
            if tx.send(msg).is_err() || lost {
                break;
            }
        }
    });
    Ok(key)
}

/// Raw Input delivers every touchpad to the one reader started in
/// [`start_readers`]; frames come keyed by interface path.
#[cfg(target_os = "windows")]
fn open_pad(d: &DeviceInfo, _gen: u64, _tx: &mpsc::Sender<Msg>) -> Result<String, String> {
    Ok(ident(d))
}

#[cfg(not(target_os = "windows"))]
fn start_readers(_tx: &mpsc::Sender<Msg>) {}

#[cfg(target_os = "windows")]
fn start_readers(tx: &mpsc::Sender<Msg>) {
    let tx = tx.clone();
    std::thread::spawn(move || {
        let mut all = crate::input::windows_backend::AllTouchpads::open();
        loop {
            let msg = match all.wait_events(TICK) {
                Ok(None) => continue,
                Ok(Some((path, state))) => Msg::Frame(path, Box::new(state)),
                Err(e) => Msg::Fatal(e.to_string()),
            };
            let fatal = matches!(msg, Msg::Fatal(..));
            if tx.send(msg).is_err() || fatal {
                break;
            }
        }
    });
}

struct Logger {
    log: Log,
    /// Only this pad, if `--device` named one.
    only: Option<DeviceInfo>,
    find: fn() -> Vec<DeviceInfo>,
    tx: mpsc::Sender<Msg>,
    pads: HashMap<String, Pad>,
    /// Keys of frames from pads not followed (filtered out, or not yet
    /// discovered); cleared on every periodic rescan.
    unknown: HashSet<String>,
    /// Pads that failed to open, warned about once.
    failed: HashSet<String>,
    gen: u64,
    last_scan: Instant,
}

impl Logger {
    fn rescan(&mut self) {
        self.last_scan = Instant::now();
        let found: Vec<DeviceInfo> = (self.find)()
            .into_iter()
            .filter(|d| self.only.as_ref().is_none_or(|o| same_device(d, o)))
            .collect();
        let idents: HashSet<String> = found.iter().map(ident).collect();
        let gone: Vec<String> = self
            .pads
            .iter()
            .filter(|(_, p)| !idents.contains(&p.ident))
            .map(|(k, _)| k.clone())
            .collect();
        for key in gone {
            self.drop_pad(&key, "removed");
        }
        for d in found {
            let id = ident(&d);
            if self.pads.values().any(|p| p.ident == id) {
                continue;
            }
            self.gen += 1;
            match open_pad(&d, self.gen, &self.tx) {
                Ok(key) => {
                    self.failed.remove(&id);
                    let label = d.label();
                    log::info!("log-touches: following {}", label);
                    self.log.note("start", now(), &label, &id);
                    self.unknown.remove(&key);
                    self.pads.insert(
                        key,
                        Pad {
                            label,
                            ident: id,
                            act: Activity::default(),
                            end_seen: None,
                        },
                    );
                }
                Err(e) => {
                    if self.failed.insert(id) {
                        log::warn!("log-touches: cannot open {}: {}", d.label(), e);
                    }
                }
            }
        }
    }

    fn drop_pad(&mut self, key: &str, why: &str) {
        if let Some(mut p) = self.pads.remove(key) {
            if let Some(t) = p.act.close() {
                self.log.touch(&p.label, &t);
            }
            log::info!("log-touches: lost {}: {}", p.label, why);
            self.log.note("lost", now(), &p.label, why);
        }
    }

    fn frame(&mut self, key: String, state: Box<TouchState>, wall: f64) {
        if !self.pads.contains_key(&key) {
            // A pad Raw Input knows before the last rescan did (Windows);
            // look once, then ignore it until the next periodic rescan.
            if self.unknown.insert(key.clone()) {
                self.rescan();
            }
            if !self.pads.contains_key(&key) {
                return;
            }
        }
        let Some(p) = self.pads.get_mut(&key) else {
            return;
        };
        let b = state.buttons;
        let t_us = state.timestamp_us.unwrap_or_else(crate::input::host_now_us);
        let done = p
            .act
            .frame(contacts(&state), b.left || b.right || b.middle, t_us, wall);
        p.end_seen = match (p.act.ending(), p.end_seen) {
            (false, _) => None,
            (true, None) => Some(Instant::now()),
            (true, seen) => seen,
        };
        if let Some(t) = done {
            self.log.touch(&p.label, &t);
        }
    }

    /// End every touch whose lift is older than the debounce, or all of them.
    fn close_touches(&mut self, all: bool) {
        for p in self.pads.values_mut() {
            let due = p.end_seen.is_some_and(|t| t.elapsed() >= DEBOUNCE);
            if all || due {
                if let Some(t) = p.act.close() {
                    self.log.touch(&p.label, &t);
                }
                p.end_seen = None;
            }
        }
    }
}

/// Run until killed. `only`: the one pad to follow (`--device`), else
/// every touchpad `find` turns up, now or later.
pub fn run(
    only: Option<DeviceInfo>,
    path: &str,
    find: fn() -> Vec<DeviceInfo>,
) -> Result<(), String> {
    let log = Log::open(path).map_err(|e| format!("{}: {}", path, e))?;
    let (tx, rx) = mpsc::channel();
    let mut lg = Logger {
        log,
        only,
        find,
        tx,
        pads: HashMap::new(),
        unknown: HashSet::new(),
        failed: HashSet::new(),
        gen: 0,
        last_scan: Instant::now(),
    };
    let what = lg
        .only
        .as_ref()
        .map_or("all touchpads".to_string(), |d| d.label());
    log::info!("Logging touches on {} to {}", what, path);
    lg.log.note(
        "log",
        now(),
        "",
        &format!("tapview {}, {}", env!("CARGO_PKG_VERSION"), what),
    );
    start_readers(&lg.tx);
    lg.rescan();
    let mut last_wall = now();
    loop {
        let ending = lg.pads.values().any(|p| p.end_seen.is_some());
        let timeout = if ending { DEBOUNCE } else { TICK };
        let msg = rx.recv_timeout(timeout);
        let wall = now();
        if wall - last_wall > (timeout + SLEEP_SLACK).as_secs_f64() {
            lg.close_touches(true);
            let rest = format!("{:.3},,,,", wall - last_wall);
            lg.log.row("sleep", last_wall, "", &rest);
        }
        last_wall = wall;
        match msg {
            Ok(Msg::Frame(key, state)) => lg.frame(key, state, wall),
            Ok(Msg::Lost(key, why)) => lg.drop_pad(&key, &why),
            Ok(Msg::Fatal(e)) => return Err(e),
            Err(_) => {}
        }
        lg.close_touches(false);
        if lg.last_scan.elapsed() >= RESCAN {
            lg.unknown.clear();
            lg.rescan();
        }
        if lg.log.last_write.elapsed() >= HEARTBEAT {
            lg.log.row("alive", wall, "", ",,,,");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1000;

    #[test]
    fn one_finger_tap() {
        let mut a = Activity::default();
        assert_eq!(a.frame(1, false, 0, 100.0), None);
        assert_eq!(a.frame(1, false, 7 * MS, 100.007), None);
        assert_eq!(a.frame(0, false, 120 * MS, 100.12), None);
        assert!(a.ending());
        let t = a.close().unwrap();
        assert_eq!(t.start, 100.0);
        assert!((t.duration_s - 0.12).abs() < 1e-9);
        assert_eq!((t.max_contacts, t.contacts, t.clicks), (1, 1, 0));
    }

    #[test]
    fn second_finger_and_click_count_once() {
        let mut a = Activity::default();
        a.frame(1, false, 0, 0.0);
        a.frame(2, false, 10 * MS, 0.0);
        a.frame(2, true, 20 * MS, 0.0);
        a.frame(2, true, 30 * MS, 0.0);
        a.frame(1, false, 40 * MS, 0.0);
        a.frame(0, false, 50 * MS, 0.0);
        let t = a.close().unwrap();
        assert_eq!((t.max_contacts, t.contacts, t.clicks), (2, 2, 1));
    }

    #[test]
    fn short_empty_frame_does_not_split_a_touch() {
        // Windows hybrid mode: a continuation report parses as no contacts.
        let mut a = Activity::default();
        a.frame(3, false, 0, 0.0);
        assert_eq!(a.frame(0, false, 7 * MS, 0.0), None);
        assert_eq!(a.frame(3, false, 8 * MS, 0.0), None);
        assert!(!a.ending());
        a.frame(0, false, 500 * MS, 0.0);
        let t = a.close().unwrap();
        assert!((t.duration_s - 0.5).abs() < 1e-9);
        assert_eq!(t.contacts, 3);
    }

    #[test]
    fn new_contact_after_debounce_starts_a_new_touch() {
        let mut a = Activity::default();
        a.frame(1, false, 0, 10.0);
        a.frame(0, false, 100 * MS, 10.1);
        let t = a.frame(1, false, 300 * MS, 10.3).unwrap();
        assert!((t.duration_s - 0.1).abs() < 1e-9);
        let t = a.close().unwrap();
        assert_eq!(t.start, 10.3);
        assert_eq!(t.duration_s, 0.0);
    }
}
