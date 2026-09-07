//! Android entry point for tapview.
//!
//! `android-activity`'s native-activity glue calls [`android_main`] on its
//! own thread once the `NativeActivity` is up. Everything of substance lives
//! in the `tapview` crate: this file only wires logging, hands the
//! JVM/activity pointers to `tapview::android_hid` (which talks to the Kotlin
//! `UsbBridge`), and enters the shared egui UI with Android-shaped
//! `NativeOptions` — no desktop viewport hints, `android_app` filled in.
//!
//! The cfg guard makes desktop builds of the workspace produce an empty
//! cdylib rather than an error; the real artifact comes from
//! `cargo ndk -t arm64-v8a build`, driven by Gradle.
#![cfg(target_os = "android")]

use winit::platform::android::activity::AndroidApp;

// No `extern "C"`: android-activity's glue declares android_main with the
// Rust ABI (`extern "Rust"`), and AndroidApp is not an FFI-safe type anyway.
#[no_mangle]
pub fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("tapview"),
    );
    // Panics land in logcat rather than dying silently with the process.
    std::panic::set_hook(Box::new(|info| {
        log::error!("panic: {info}");
    }));

    // SAFETY: android-activity guarantees these pointers are the process's
    // JavaVM and a live activity reference for the app's lifetime.
    if let Err(e) = unsafe { tapview::android_hid::init(app.vm_as_ptr(), app.activity_as_ptr()) } {
        // The UI still runs; the device picker will report this error.
        log::error!("USB bridge init failed: {e}");
    }

    let options = eframe::NativeOptions {
        android_app: Some(app),
        ..Default::default()
    };
    if let Err(e) = tapview::android::run_with(options) {
        log::error!("tapview exited with error: {e}");
    }
}
