#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
fn main() {
    tapview::cli::main()
}

/// The browser build: trunk compiles this bin to wasm and index.html loads
/// it; there is no command line, the page's Connect button stands in for
/// device discovery.
#[cfg(target_arch = "wasm32")]
fn main() {
    tapview::web::start()
}

/// Android has no binary: the app is the `tapview-android` cdylib in
/// android/rust. This only keeps `cargo build --target aarch64-linux-android`
/// from failing on the bin target.
#[cfg(target_os = "android")]
fn main() {}
