# Plan: tapview in the browser (WASM + WebHID, hosted on GitHub Pages)

Modelled on aster's `plans/browser-gui.md`: eframe already compiles to
wasm32, WebHID reaches the touchpad's HID device from a page, and trunk +
GitHub Pages host the result as static files.

**Status (2026-09-07):** phase 1 is done, on branch `android`, as one
commit per step, plus the PTP parser (2.3) and a Linux `--hidraw` backend
that exercises it. The Android port (`plans/android.md`) was built on top of
it and is the first raw-HID front end. Phases 2 and 3 are coded (see
"As built" under phase 2) and pass clippy on both targets, `cargo test`, and
`trunk build --release`; **hardware validation in Chrome is still pending**,
including the phase 0 questions — the app itself now serves as the spike
(open it under `trunk serve` and watch the console).

## Verdict

Feasible. The differences from aster are what tapview reads from:

- aster has a single `Transport` seam; tapview has three independent data
  sources — evdev (touches), libinput (interpreted input), hidraw (heatmap
  and PTP config) — and only the hidraw one has a browser equivalent.
- So the browser build reads *everything* through WebHID: the touchpad's raw
  PTP input reports for touches (a new, descriptor-driven parser), and
  feature reports for heatmap and config (the existing protocol code, made
  async).

The unavoidable change, same as in aster: WebHID is promise-based and the
wasm main thread can never block (`Atomics.wait` is forbidden there), so the
HID I/O layer and anything that loops over it become async. Touch input is
push-based in the browser, which fits the existing channel-per-frame design
without any loop.

## What the browser can and cannot do

| Feature | Native | Browser |
|---|---|---|
| Touch points, trails, buttons | evdev / RawInput | WebHID `inputreport` events from the PTP collection, parsed in Rust |
| Heatmap | hidraw / HidD feature reports | `sendFeatureReport` / `receiveFeatureReport`, same protocol code |
| PTP config panel | hidraw / HidP feature reports | same, fields derived from `HIDDevice.collections` |
| libinput panel | libinput / WH_MOUSE_LL | degraded "browser pointer" panel fed from egui's own input events |
| Grab (Enter/Esc) | EVIOCGRAB | not possible — hidden |
| `--verbose` evdev dump | stderr | n/a |
| Device enumeration, `--list`, `--device` | udev / SetupAPI | none — a **Connect touchpad** button (`requestDevice`), silent re-grant on revisit (`getDevices`) |
| `--record` / `--play` | files | record to memory + download button; play from a file picker; bundled `testdata/sample.tapv` as a demo mode |
| Axis extents | evdev absinfo | HID descriptor logical max (no kernel axis swap to worry about) |

Why touches work at all: Chromium's protected-usage list blocks Keyboard,
Mouse/Pointer, System Control and FIDO collections, not Digitizer / Touch
Pad (0x0D/0x05). The touchpad device stays visible because it has
unprotected collections; the Mouse collection's reports are filtered per
report ID. This relies on the touchpad using numbered reports (Framework's
PixArt pads do: 1 touch, 2 mouse, 3 config, 0x41–0x43 vendor) — a pad with
unnumbered reports and a mouse collection would be blocked entirely.

Reach: Chromium on the desktop only (Chrome, Edge, Opera). On Linux, Chrome
opens `/dev/hidraw*` as the user, so the same `hidraw` group / udev rule
native tapview needs for the heatmap applies — and *only* that one; the
`input` group is not needed in the browser.

## Unknowns to settle first (phase 0 spike, no Rust)

One plain HTML page in a scratch directory, opened under `python -m
http.server` (localhost is a secure context), with a button that does:

```js
const [dev] = await navigator.hid.requestDevice({filters: [{usagePage: 0x0D, usage: 0x05}]});
await dev.open();
console.log(dev.collections);              // parsed descriptor: do we see touch, config, vendor?
dev.oninputreport = e => console.log(e.reportId, new Uint8Array(e.data.buffer));
await dev.sendFeatureReport(0x42, new Uint8Array([0x78, 0x00 | 0x10, 0x00])); // read Part ID low
console.log(await dev.receiveFeatureReport(0x42));
```

Run on a Framework 13 or 16 under Linux Chrome, and once on a Windows box.
Questions it answers, ordered by risk:

1. **Windows: can Chrome open the digitizer collection?** Windows may hold
   PTP collections exclusively (tapview's own Windows backend uses RawInput,
   not `ReadFile`, which hints at this). Chrome opens every collection of the
   physical device when `open()` is called; if one fails the whole open may
   fail, which would make Windows heatmap-only or nothing. Only a test
   answers this. Linux is the primary target either way.
2. **Do PTP input reports arrive** while hid-multitouch owns the device?
   Expected yes on Linux (hidraw is a tee), and the cursor keeps moving.
3. **Do the vendor feature reports 0x41–0x43 and the config report pass**
   Chrome's per-report filtering? Vendor and Digitizer pages are not
   protected, so expected yes.
4. **Does `collections[].featureReports` carry report 0x41's `reportCount`**
   (the burst length) and the config fields with `logicalMinimum` /
   `physicalMaximum` / `unitExponent`? Needed because WebHID exposes no raw
   descriptor bytes.
5. **Heatmap frame rate**: each feature report is a Mojo IPC round trip to
   Chrome's device service (a frame is ~25–40 of them). Measure a
   `receiveFeatureReport(0x41)` loop; expected fine, but this is the one
   performance question.
6. Are the per-finger items in `inputReports[].items` in descriptor order
   (Confidence, Tip Switch, Contact ID, X, Y, … repeated per Finger
   collection)? The parser groups fingers by recurrence of the same usages,
   since WebHID items carry no link-collection id.

## Phase 1: native-only refactors (no web code, behavior unchanged) — DONE

Landed as separate commits (order 1 → 7 → 2 → 4 → 3 → 5 → 6, then 2.3 and
the `--hidraw` backend). `tapview --info` was byte-identical after every
step; Windows checked with the `cross-windows` dev shell. Where the code
differs from the sketch below, an **As built** note gives the real names.

1. **One module tree.** `lib.rs` and `main.rs` both declare modules today,
   so the crate compiles everything twice and the binary never uses the lib.
   Move every module into the lib (`app`, `render`, `config`, `dimensions`,
   `libinput_*`, `recording`, …); `main.rs` becomes a thin native entry
   (`tapview::cli::main()`), with a `#[cfg(target_arch = "wasm32")] fn main`
   later calling `tapview::web::start()` — the same split as aster-gui's
   `main.rs`. Trunk then builds the one `tapview` bin.

   **As built:** `src/cli.rs` holds the whole CLI (`pub fn main()`);
   `main.rs` is `tapview::cli::main()` behind `cfg(not(android))`. Every
   module is `pub` in `lib.rs`. There is no `gui::run_with`: front ends
   build `TapviewApp::new(trails)` and chain `.with_session(..)`,
   `.with_playback(..)`, `.with_libinput(..)`, `.with_no_session_ui(..)`,
   `.with_frame_hook(..)`, then call `eframe::run_native` themselves (see
   `src/android.rs::run_with` for the shape the wasm entry should copy).

2. **Async `HidDevice`.** `heatmap::HidDevice` gains `async fn set_feature`
   / `async fn get_feature` (native async-fn-in-trait, no `async-trait`
   crate needed: the ~16 `&dyn HidDevice` sites in `heatmap/protocol.rs`,
   `chips.rs`, `backend.rs`, `config/linux.rs`, `config/windows.rs` become
   generic `D: HidDevice`, with a per-platform concrete type behind a
   `PlatformHidDevice` alias). The heatmap thread body becomes
   `pollster::block_on(run_heatmap_loop(..))` — it blocks where the ioctl
   used to block. New deps: `pollster`.

   **As built:** `heatmap::HidDevice` is `#[allow(async_fn_in_trait)]` with
   `async fn set_feature/get_feature`; `heatmap::PlatformHidDevice` and
   `open_platform_hid_device` exist for Linux/Windows;
   `heatmap::backend::spawn_heatmap_thread_with(dev, burst_len, cols)` takes
   an already opened `D: HidDevice + Send` (what a `WebHidDevice` will use,
   though on wasm it must become a `spawn_local` task rather than a thread).

3. **Config behind a worker.** `render::draw_config_panel` calls
   `PtpConfig::set_*` and `refresh()` synchronously from the UI thread. Split
   `PtpConfig` into a UI-side handle (cached `features`, values, ranges,
   `physical_size`, a `Sender<ConfigCommand>` and a
   `Receiver<ConfigEvent>`) and a worker that owns the `ConfigBackend`
   (async now) and runs `read_all`/`probe_writable` at startup, then
   services commands. Native: a thread with `block_on`; wasm later:
   `spawn_local`. Writes are optimistic in the UI and reverted on an error
   event. The `--info` / `--set-*` CLI paths call the worker functions
   directly under `block_on`.

   **As built:** `config::ConfigBackend` is async; `config::discover(path)
   -> Option<Discovered { description: ConfigDescription, backend }>` does no
   I/O, `config::initialize(&mut backend, &mut description).await ->
   ConfigState` does read/seed/probe. UI side: `config::ConfigHandle
   { description, state, .. }` with `set_*`, `refresh()`, `pump()` (call
   once per frame; failed writes revert only their own field). Worker:
   `config::run_config_worker(backend, cmd_rx, evt_tx)` — an `async fn`
   over std `mpsc`, spawned by `ConfigHandle::spawn_thread`; wasm needs a
   `spawn_local` variant that does not block on `recv`.

4. **Descriptor model.** New `hid/layout.rs` (name open): `ReportField {
   usage_page, usage, report_id, bit_offset, bit_size, count, constant,
   logical_{min,max}, physical_{min,max}, unit, unit_exponent }` plus a
   per-report byte size. `config/linux.rs`'s `parse_ptp_features` /
   `parse_touchpad_physical_size` and `heatmap/discovery.rs`'s
   `parse_report_descriptor_for_burst_len` already compute exactly these
   pieces from raw bytes; refactor them into one raw-descriptor walker that
   emits the model, and rewrite `LinuxConfigBackend` as
   `LayoutConfigBackend<D>` (bit extract/insert over the model, any
   `HidDevice`). The browser fills the same model from
   `HIDDevice.collections`. The Windows HidP backend stays as it is.

   **As built:** `src/hid/descriptor.rs`: `ReportLayout::parse(bytes)` →
   `fields: Vec<ReportField>` (kind, report_id, usage_page, usage,
   collection index, bit_offset, bit_size, constant, variable, logical/
   physical ranges, unit, unit_exponent) plus `collections`, with
   `report_bytes`, `report_ids`, `find`, `fields_in`, `is_inside`,
   `has_application_collection`. Bit helpers `extract_bits/extract_signed/
   insert_bits`, `physical_range_mm(&ReportField)`. The browser must build a
   `ReportLayout` from `HIDDevice.collections` (fill `fields` and
   `collections` directly; `sizes` is private — add a constructor).
   `config::layout_backend::{PtpFields::from_layout, LayoutConfigBackend<D>,
   touchpad_physical_size}`; `heatmap::discovery::burst_report_length(&layout)`.
   Fixture: `testdata/ptp/framework13_pixa3854.desc`.

5. **Recording over `Write` / `&[u8]`.** `Recorder::new` takes an
   `impl Write` (file wrapper kept for the CLI); `Recording::load` gets a
   `from_bytes`. `std::time::Instant` → `web_time::Instant` here and in
   `app.rs` (drop-in on native; std's panics on wasm).

   **As built:** `Recorder<W: Write = Box<dyn Write + Send>>::new(writer,
   ex, ey)`, `into_inner()` returns the sink; `Recorder::create(path, ..)`
   for files; `Recording::{read, from_bytes, load}`.

6. **`Session` struct.** Bundle what `TapviewApp::new` takes today
   (`touch_rx`, `heatmap_rx`, config handle, extents, recorder) into a
   `Session` that `main.rs` builds before eframe runs, and let the app hold
   `Option<Session>` so a session can be attached later (the browser's
   Connect click) and the no-session state can show buttons. The
   libinput/pointer panel and playback stay outside the session.

   **As built:** `session::Session { touch_rx, grab_tx: Option<..>,
   heatmap_rx, config: Option<ConfigHandle>, extents, recorder, lost:
   Option<Receiver<String>> }` (`lost` = the backend saw the device go
   away; the app detaches and shows why). `app::NoSessionUi { message,
   controls: Option<NoSessionControls> }` where the controls closure is
   drawn every frame under the message and returns
   `Option<SessionRequest::{Attach(Box<Session>), Playback(Box<Recording>)}>`
   — the web Connect button and demo button go in there (see
   `android.rs::UsbPicker` for a worked example that polls its own worker
   threads). `TapviewApp::{attach_session, detach_session, start_playback,
   set_no_session_message}` and a per-frame `FrameHook`.

7. **Logging.** `eprintln!` is a silent no-op on wasm32-unknown-unknown.
   Route diagnostics through the `log` crate (`env_logger` or plain stderr
   natively, `console_log` + `console_error_panic_hook` on wasm).

   **As built:** all diagnostics use `log::{error,warn,info,debug}!`;
   `cli::init_logging` installs `env_logger` (desktop only, the dep is
   gated to `not(android)`); `--verbose` = our crate at debug, which is
   where the evdev event dump lives. The interactive device prompt still
   writes to stderr directly.

Acceptance: `cargo test`, clippy on Linux and the Windows cross-build, and
`tapview --info` output byte-identical before and after against a real pad;
one manual session with heatmap, config writes and a recording round trip.

## Phase 2: the web target

Already in place from the Android work: per-target eframe declarations in
`Cargo.toml` (add a wasm arm next to the android one), `clap`/`env_logger`
gated to desktop, `ptp.rs` (2.3 below is done), and the no-session
`controls` hook for the Connect/demo buttons.

1. **Cargo split**, mirroring aster: eframe with `default_fonts` + `glow`
   only on wasm (native keeps `wayland`/`x11`); `clap` gated off wasm;
   wasm-only `wasm-bindgen`, `wasm-bindgen-futures`, `js-sys`, `web-sys`
   (Window, Document, HtmlCanvasElement, Navigator), `gloo-timers`. The
   evdev/udev/libinput/windows deps are already target-gated and vanish on
   `target_os = "unknown"`.

2. **`web_hid.rs`**: hand-written `wasm_bindgen` externs (web-sys still
   gates WebHID behind an unstable cfg), copied in spirit from aster's:
   - `connect(request_new)`: `requestDevice` with the Touch Pad filter on a
     click, `getDevices` for the silent re-grant, open the device, build
     the report layout from `collections`, hand out a `Session`.
   - `oninputreport` closure: reports with the touch report ID go through
     the PTP parser into the touch `mpsc::Sender`, then `request_repaint`.
     No polling loop.
   - `WebHidDevice: HidDevice` with the two async feature-report arms
     (report ID split off the front of the buffer, hidapi-style framing
     kept for the callers).
   - Heatmap loop and config worker run under `spawn_local`; every await
     yields to the browser, so rendering is never starved.

3. **`ptp.rs`** — DONE (`src/ptp.rs`: `PtpLayout::from_layout(&ReportLayout)`,
   `PtpParser::new(layout).feed(report) -> Option<TouchState>`, report-ID
   byte included for numbered reports; hybrid mode, palms, kernel-style
   tracking ids and BTN_TOUCH/DOUBLETAP emulation; `ptp::MT_TOOL_PALM`).
   Linux `tapview --hidraw` runs it against the real pad and
   `--dump-hidraw N` captures fixtures. Original description: a
   platform-neutral PTP input-report parser producing
   `TouchState` from a report layout: Contact Count (0x54), per finger
   Confidence (0x47) → `tool_type = MT_TOOL_PALM`, Tip Switch (0x42),
   Contact ID (0x51), X/Y, width/height (0x48/0x49) → touch major/minor,
   pressure (0x30) if present, Button page (0x09) → `ButtonState`. Handle
   hybrid mode (Contact Count only in the first of several reports).
   Unit-tested against a captured descriptor and reports from a Framework
   pad; the Windows RawInput backend keeps HidP but could later switch to
   this for a smaller `windows` feature set.

4. **`web.rs` entry**: `eframe::WebRunner` on `<canvas id="tapview_canvas">`,
   `App` starting with no session, showing **Connect touchpad** and **Play
   demo recording** (`include_bytes!("../testdata/sample.tapv")`, ~1.8 MB —
   fine for a page) plus a clear "needs a Chromium browser" message when
   `navigator.hid` is missing. Grab UI and text cfg'd out; the status line
   gains a wasm arm.

5. **Browser pointer panel**: a small adapter turning egui `RawInput`
   events (`PointerMoved` deltas, `MouseWheel`, `zoom_delta` for ctrl+wheel
   pinch, pointer buttons) into `LibinputEvent`s for the existing
   `LibinputState`, so the right panel keeps working with a renamed title.
   Only sees events over the canvas, which is acceptable.

6. **`Trunk.toml`, `index.html`** (canvas fills the page, loading text
   behind it, wasm-opt params as in aster), and a **wasm clippy job in CI**
   (`cargo clippy --target wasm32-unknown-unknown -- -D warnings`) so native
   work cannot silently break the browser build.

**As built (phase 2):** `src/web_hid.rs` (externs, `layout_from_collections`
→ `ReportLayout::from_parts` + `ptp::synthesize_finger_collections`,
`connect`, `open_session`, `WebHidDevice: HidDevice`, `SessionGuard` in the
new `Session::guard` slot, page-wide hot-plug listeners); `src/web.rs`
(`start`, `WebPicker` on the no-session controls, `BrowserPointer` frame hook
feeding the libinput panel); `config::run_config_worker_polling` for the
single-threaded worker; `index.html` + `Trunk.toml`; a `check-web` CI job.
`synthesize_finger_collections` is unit-tested by flattening the Framework
13 fixture the way Chromium does and rebuilding the same `PtpLayout`.
Confirmed working in Chrome on Linux (2026-09-07). Follow-ups landed the
same day: a **Disconnect** bar over a live session (a built-in pad cannot
be unplugged to switch), the picker not auto-reopening a pad the user just
closed, and `Session::name` drawn in the view's corner on every front end.

## Phase 3: ship — coded, awaiting the first deploy

- `.github/workflows/pages.yml` like aster's: `trunk build --release
  --public-url /tapview/` + `actions/deploy-pages` on pushes to `main`
  (Pages must be set to "GitHub Actions" in the repo settings once).
- README: "In the browser" section — Chromium only, the hidraw rule, devices
  are granted not enumerated, what is missing (grab, libinput), how to run
  locally (`trunk serve`).
- `flake.nix`: `devShells.web` with the `wasm32-unknown-unknown` target,
  `trunk` and `binaryen`.

## Later

- Recording download (blob URL from the in-memory buffer) and upload
  (`rfd::AsyncFileDialog`, which aster already uses on wasm).
- URL query parameters standing in for the CLI flags (`?trails=5`,
  `?no_heatmap`), parsed into the same settings struct.
- Windows-in-browser support depending on the phase 0 answer.
- WebHID on ChromeOS, if anyone asks.

## Rejected alternatives

- **wasm threads + `Atomics.wait` proxy** to keep the sync HID layer: same
  reasons as aster — cross-origin-isolation headers Pages cannot set,
  nightly-ish toolchain, a hand-rolled main-thread proxy.
- **Pointer/Touch DOM events for touches**: browsers expose no touchpad
  multitouch (only gestures as wheel deltas), so this cannot draw contact
  points. Kept only as the source of the degraded pointer panel.
- **A separate JS web app**: would duplicate the heatmap chip protocol,
  the descriptor parsing and the rendering; the Rust code already runs on
  wasm.

## Validation

Phase 1 (bar: indistinguishable from today):
- `cargo test`, `cargo clippy -- -D warnings` on Linux, the Windows
  cross-build (`nix build .#windows` / `nix develop .#windows -c cargo
  clippy --target x86_64-pc-windows-gnu`).
- `tapview --info` diffed before/after on a Framework 13 and 16.
- One live session per platform: touches, heatmap, a config write (haptic
  intensity is safe and visible), `--record` then `--play` of the result.

Phase 2, Chrome on `trunk serve` against real hardware:
- Connect shows only the touchpad; touches match the native view side by
  side (positions, palm colouring, buttons).
- Heatmap renders; frame rate noted in the README if noticeably below
  native.
- Config panel reads the same values `--info` prints; a write sticks.
- Unplug / revoke mid-session gives an error message, not a hang; the page
  stays responsive while the heatmap loop runs (the single-thread proof).
- Revisit: the granted pad reappears without a prompt.
- Firefox/Safari: clear unsupported message, demo playback still works.
- Without hidraw permission on Linux: the failure message names the fix.

Phase 3: the Pages URL loads over HTTPS, the WebHID prompt appears, one
full session.
