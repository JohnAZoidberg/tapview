#[cfg(not(target_os = "android"))]
fn main() {
    tapview::cli::main()
}

/// Android has no binary: the app is the `tapview-android` cdylib in
/// android/rust. This only keeps `cargo build --target aarch64-linux-android`
/// from failing on the bin target.
#[cfg(target_os = "android")]
fn main() {}
