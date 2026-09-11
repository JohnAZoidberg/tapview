# Tapview

A touchpad visualizer. Shows multitouch contact points in real time, the raw capacitive heatmap of PixArt pads, and the Precision Touchpad (PTP) configuration the firmware exposes. Useful for debugging touchpad behavior, testing palm rejection, and understanding how your touchpad reports touches.

It runs natively on Linux (the primary platform) and Windows, as an [Android app](#on-android) for phones with USB host support, and [in the browser](#in-the-browser) through WebHID.

## Tested on

| Hardware            | Platform             | Raw Events | Interpreted panel | Heatmap | PTP config |
|---------------------|----------------------|------------|-------------------|---------|------------|
| Framework Laptop 12 | Linux                | Working    | Working           | Working | Working    |
| Framework Laptop 13 | Linux                | Working    | Working           | Working | Working    |
| Framework Laptop 16 | Linux                | Working    | Working           | Working | Working    |
| Framework Laptop    | Linux, Chrome        | Working    | Browser events    | Working | Working    |
| Framework Laptop 16 | Windows, Chrome      | Not exposed| Browser events    | Working | Partial    |
| Touchpad KB (daisy) | Android (Fairphone 6)| Working    | n/a               | Working | Working    |

Touchpads attached over Bluetooth work on Linux and Windows through the normal HID stack. The heatmap is noticeably slower there (a few frames per second instead of around ten) because every frame is a series of feature-report round trips over the link.

## What it does

- Discovers your touchpad automatically via udev
- Reads raw multitouch events from `/dev/input/event*`
- Renders touch points as colored circles with trails
- Magenta = first finger, teal = additional fingers, gray = palm-rejected touches
- Shows press state (filled dot) and double-tap state (ring)
- Estimates the report rate in the top-right corner while a finger is down (see below)
- Optionally grabs exclusive access so touches don't move the system cursor
- Shows what the system makes of the same touches in a right-hand panel: pointer motion, scrolling and gestures from libinput on Linux, from a mouse hook on Windows
- Renders the raw capacitive heatmap of PixArt touchpad controllers (see below)
- Reads and writes the pad's PTP configuration: input mode, surface and button switches, latency mode, click force and haptic intensity (see below)
- Records a touch session to a file and plays it back, with no device needed

### Heatmap

Touchpads built on a PixArt controller (PJP274, PJP343, PJP255, PJP215, PLP239, PCT1036, which covers the Framework laptops) expose their raw capacitance matrix through vendor feature reports on the pad's HID interface. Tapview polls it and draws every sensor cell as a colored square next to the touch view, so a palm, a hovering finger or a noisy cell is visible even when the firmware reports no contact. The heatmap is enabled automatically when a supported chip is found; `--heatmap` insists on it, `--no-heatmap` turns it off, and `--heatmap-cols` overrides the column count if a frame comes out with the wrong stride. On Linux the reports go through the touchpad's `/dev/hidraw*` node, which is why the `hidraw` group is needed.

### PTP configuration

Precision Touchpads describe their settings in the HID report descriptor, and tapview reads that descriptor to find out which fields the pad has and which are writable. The panel shows the current input mode (mouse or touchpad), whether the surface and the buttons report, the maximum contact count, the latency mode, and, on pads that have them, sliders for the click-force threshold and haptic intensity. Changes are written to the device as you make them. The two write-only fields can also be set from the command line without opening the UI (`--set-click-force`, `--set-haptic-intensity`), and `--info` prints everything the descriptor says about the device. The panel is enabled automatically when the pad has any configurable field; `--config` insists on it and `--no-config` hides it.

### Report rate

While a finger is on the pad, the top-right corner shows something like `Report rate: pad 140 Hz · host 100 Hz`, taken over the last two seconds of contact and held while the pad is idle.

- **pad** is the touchpad's scan cadence: the typical (median) step of the timestamp the pad writes into each report (PTP Scan Time, which the kernel passes on as `MSC_TIMESTAMP`). It says how often the pad scans, regardless of what happens on the way to the host.
- **host** is throughput: the number of frames that arrived, divided by the time they span on the host's clock. A count rather than a median, because links that deliver frames in clumps (Bluetooth hands them over on its connection-interval grid) would otherwise make the typical arrival gap look faster than the real rate.

The two agree on a pad whose every scan reaches the host. `pad` above `host` means scans are being skipped, coalesced or dropped between the pad and the host: a pad that scans at 140 Hz but only ships a report every 10 ms reads `pad 140 Hz · host 100 Hz`. Only `host` is shown when the device puts no timestamp in its reports (or on Windows, where it is not read yet). In playback, the figure is the rate around the current position of the recording.

## Dependencies

### Build dependencies

You need a Rust toolchain and development headers for libudev and libinput:

**Fedora / RHEL:**
```
sudo dnf install libudev-devel libinput-devel
```

**Debian / Ubuntu:**
```
sudo apt install libudev-dev libinput-dev
```

**Arch:**
```
sudo pacman -S systemd-libs libinput
```

You also need the standard graphics libs that eframe/egui depend on (typically already present on desktop systems):

**Fedora:**
```
sudo dnf install gcc pkg-config libxkbcommon-devel wayland-devel libX11-devel
```

**Debian / Ubuntu:**
```
sudo apt install libxkbcommon-dev libwayland-dev libx11-dev
```

**NixOS/Nix:**
```
nix develop
```

Or run directly without cloning:
```
# Without cloning
sudo nix run github:JohnAZoidberg/tapview
sudo nix run github:JohnAZoidberg/tapview -- -l --heatmap

# In cloned repo
sudo nix run
sudo nix run . -- -l --heatmap
```

### Runtime

Requires read access to the touchpad's `/dev/input/event*` device. Typically this means running as root or adding your user to the appropriate groups:

```bash
# For /dev/input/event* access (evdev)
sudo usermod -aG input $USER

# For /dev/hidraw* access (heatmap, PTP configuration, --hidraw)
sudo usermod -aG hidraw $USER
```

Log out and back in for group changes to take effect. After this, you can run tapview without sudo.

## Building

```
cargo build --release
```

The binary will be at `target/release/tapview`.

## Usage

```
sudo ./target/release/tapview [OPTIONS]
```

### Options

| Flag | Description |
|------|-------------|
| `-t, --trails <N>` | Number of trail frames to show (default: 20, max: 20) |
| `-v, --verbose` | Print raw kernel multitouch events to stderr |
| `-l, --libinput` | Insist on the interpreted-input panel (libinput on Linux, mouse hook on Windows) and exit if it is unavailable. It is enabled automatically otherwise |
| `--no-libinput` | Hide the interpreted-input panel |
| `--heatmap` | Insist on the raw capacitive heatmap and exit if the pad has no supported chip. Enabled automatically otherwise |
| `--no-heatmap` | Disable the heatmap |
| `--heatmap-cols <N>` | Override the heatmap column count (for debugging stride issues) |
| `--config` | Insist on the PTP configuration panel and exit if the pad has no configurable field. Enabled automatically otherwise |
| `--no-config` | Hide the PTP configuration panel |
| `--list` | List the detected touchpads and exit |
| `--info` | Print device info (axis ranges, physical size, PTP configuration) and exit |
| `--device <DEV>` | Use a specific touchpad instead of auto-detection: a path, a name, or an event number from `--list` (`event8` or `8`) |
| `--set-haptic-intensity <N>` | Set the haptic intensity (0, 25, 50, 75 or 100; the firmware has five levels) and exit |
| `--set-click-force <N>` | Set the click-force / button-press threshold level (typically 1 = light to 3 = firm) and exit |
| `--record <path>` | Record touch session to a binary file |
| `--play <path>` | Play back a recorded touch session (no device needed) |
| `--hidraw` | Linux: read touches from the hidraw node through tapview's own PTP report parser instead of evdev (same `hidraw` access as the heatmap; no grab). For checking the parser against the kernel |
| `--dump-hidraw <N>` | Linux: print the HID report descriptor and N raw input reports as hex, then exit |
| `-h, --help` | Show help |

### Controls

| Key | Action |
|-----|--------|
| Enter | Grab touchpad (exclusive access, system cursor stops moving) |
| Escape | Release grab |
| Space | Play/pause (playback mode) |
| Left/Right | Step -/+100ms (playback mode) |

The heatmap panel has a **Heatmap** checkbox in its header. Unchecking it stops polling the sensor matrix (a stream of feature-report reads that competes with everything else on the link, and over Bluetooth nearly saturates it) and shrinks the panel to the header; checking it resumes. The panel only appears on hardware that produced a frame.

### Examples

```
# Basic usage
sudo ./target/release/tapview

# Short trails
sudo ./target/release/tapview --trails 5

# Debug raw events
sudo ./target/release/tapview --verbose

# Compare raw events with libinput interpretation
sudo ./target/release/tapview --libinput

# Record a touch session
sudo ./target/release/tapview --record /tmp/session.tapv

# Play it back (no device/sudo needed)
./target/release/tapview --play /tmp/session.tapv

# Pick a touchpad when several are attached
sudo ./target/release/tapview --list
sudo ./target/release/tapview --device event8

# Inspect and change the PTP configuration without the UI
sudo ./target/release/tapview --info
sudo ./target/release/tapview --set-haptic-intensity 50
```

#### Cross-platform builds with Nix

```
nix build            # Linux build
nix build .#windows  # Windows cross-compile
```

#### Windows clippy

```
nix develop .#windows -c cargo clippy --target x86_64-pc-windows-gnu
```

#### Incremental builds with Nix

Build as your user, then run the binary with sudo:

```
nix develop -c cargo build && nix develop -c bash -c 'sudo env LD_LIBRARY_PATH="$LD_LIBRARY_PATH" ./target/debug/tapview --record /tmp/test.tapv'
```

## On Windows

The same binary runs on Windows. Touches come from the pad's Precision Touchpad reports through Raw Input, so no driver or special privileges are needed, and the right-hand panel shows pointer motion, clicks and scrolling captured with a low-level mouse hook. Heatmap and PTP configuration use the Windows HID API and work as on Linux. Two things differ: the touchpad cannot be grabbed (Enter does nothing), and the pad's own scan timestamp is not read yet, so the report-rate figure shows only the host rate.

Windows builds are cross-compiled from Linux with Nix (see below); there is no Windows CI artifact yet.

## On Android

Tapview also builds as an Android app for phones and tablets with a USB-C
(OTG) port: plug a touchpad in and the same UI shows its contacts, heatmap
and PTP configuration. The pad is read as a raw USB HID device (Android has
no evdev or hidraw for apps), which also takes it away from the system
pointer while the app has it open. Bluetooth pads and the libinput panel are
not available there. Build and install with

```
make -C android flash
```

See [android/README.md](android/README.md) for the one-time SDK/NDK setup,
the Makefile targets and the USB permission flow.

## In the browser

Tapview also runs as a web page: the same app compiled to WebAssembly, with
the touchpad reached through
[WebHID](https://developer.mozilla.org/en-US/docs/Web/API/WebHID_API)
instead of evdev and hidraw (`src/web_hid.rs`, see `plans/web.md`). Every
push to `main` deploys it to GitHub Pages:
<https://johnazoidberg.github.io/tapview/>. To run it locally:

```
rustup target add wasm32-unknown-unknown
cargo install trunk   # or download a release binary; nix: `nix develop .#web`
trunk serve           # http://localhost:8080 (localhost is a secure context)
```

`trunk build --release` writes the same static site to `dist/`, which any
file server can host (`python -m http.server -d dist`) — just not `file://`,
and anywhere beyond localhost must be HTTPS or WebHID stays disabled.

Browser differences, all inherent to the platform:

- **Chromium on the desktop only** (Chrome, Edge, Opera): no other browser
  ships WebHID. Elsewhere the page can still play the bundled demo recording.
- **Linux needs hidraw access.** Chrome opens `/dev/hidraw*` as the user, so
  the same `hidraw` group membership the heatmap needs applies — and only
  that; the `input` group is not needed.
- **The touchpad is granted, not discovered.** Press **Connect touchpad** and
  pick it in the browser's prompt. The grant persists across visits (the page
  re-takes it on load and opens the pad when it is the only one granted).
  **Disconnect** in the bar above a live view returns to the list, where
  another granted pad can be opened or a new one connected.
- **Touches come from the raw PTP reports**, parsed like on Android, so the
  system cursor keeps working and palms show up as the firmware flags them.
  Heatmap and PTP configuration work as on the desktop.
- **Windows is heatmap-only.** The Precision Touchpad driver keeps the pad's
  Touch Pad collection, so the browser is given no input reports at all and
  no touches can be drawn — running tapview natively is the only way to see
  them there. The vendor and configuration collections do come through, so
  the page opens the pad with the heatmap filling the window, plus the
  config panel minus the fields (physical size among them) that live in the
  collection Windows withholds. Linux has no such split and is the tested
  platform.
- **No grab, no libinput.** A page cannot take exclusive hold of the pad. The
  right-hand panel shows what the browser makes of it instead: pointer
  motion, scrolling, and pinch (delivered as ctrl+wheel), only while the
  pointer is over the page.
- **No recording to a file yet.** Playback of the bundled demo works.

## Architecture

Everything lives in one library crate; the native binary, the Android `cdylib` and the browser build are thin front ends that assemble a `Session` (an open touchpad: touch reader, heatmap loop, PTP configuration worker) and hand it to the same egui `TapviewApp`. Device I/O never runs on the UI thread: the native build uses one thread per source and `mpsc` channels, the browser build uses async tasks over the same traits.

```
src/
  main.rs                    Entry shim: cli::main natively, web::start on wasm32
  lib.rs                     Module tree shared by every front end
  cli.rs                     Native CLI: arguments, device selection, --info/--set-*, thread setup
  app.rs                     eframe::App impl, rendering loop, history buffer, connect screen
  session.rs                 An open touchpad bundled with its heatmap and config handles
  multitouch.rs              MT Protocol B state machine (platform-independent)
  ptp.rs                     Parser for raw PTP input reports (hidraw, USB, WebHID)
  report_rate.rs             Pad cadence vs host throughput estimate
  recording.rs               Record/playback file format
  dimensions.rs              Touchpad-to-screen scaling math
  render.rs                  egui Painter drawing helpers
  libinput_backend.rs        Linux: libinput integration (pointer, scroll, gestures)
  libinput_state.rs          Interpreted-input event state for the side panel
  windows_input_backend.rs   Windows: mouse hook feeding the same side panel
  input/
    mod.rs                   InputBackend trait
    evdev_backend.rs         Linux evdev implementation
    hidraw_backend.rs        Linux hidraw + PTP parser (--hidraw), for checking the parser
    windows_backend.rs       Windows Raw Input implementation
  discovery/
    mod.rs                   DeviceDiscovery trait, device table
    udev_discovery.rs        Linux udev implementation
    windows_discovery.rs     Windows SetupAPI/HID implementation
  hid/
    mod.rs, descriptor.rs    HID report-descriptor model shared by every transport
    linux.rs                 Descriptor from sysfs next to the hidraw node
  heatmap/
    mod.rs                   HidDevice trait (feature-report I/O), frame type
    chips.rs, protocol.rs    PixArt chip detection, register and burst-read protocol
    discovery.rs             Finding the heatmap-capable HID interface
    backend.rs               Polling loop and its pause/stop switches
    hidraw.rs, windows_hid.rs  Native HidDevice implementations
  config/
    mod.rs                   PTP configuration model, UI handle and worker protocol
    layout_backend.rs        Descriptor-driven backend (Linux, Android, browser)
    linux.rs, windows.rs     Platform glue (Windows uses HidP instead of the descriptor model)
  web.rs, web_hid.rs         Browser front end and WebHID transport
  android.rs, android_hid.rs Android front end and JNI bridge to the Kotlin USB code
android/                     Gradle project, Kotlin USB bridge, tapview-android cdylib crate
index.html, Trunk.toml       The browser build
plans/                       Design notes for the Android and web ports
```

The `InputBackend`, `DeviceDiscovery`, `HidDevice` and `ConfigBackend` traits are the seams between the shared UI and each platform; adding a transport means implementing them and building a `Session`.
