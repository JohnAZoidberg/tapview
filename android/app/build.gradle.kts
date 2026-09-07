plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "me.danielschaefer.tapview"
    compileSdk = 35

    defaultConfig {
        applicationId = "me.danielschaefer.tapview"
        // UsbDeviceConnection/NativeActivity are ancient; 26 is just a sane
        // floor (and android-activity's own minimum).
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        ndk {
            // Phones are arm64; add x86_64 here (and to the cargoNdk task
            // below) if an emulator build is ever wanted — though without USB
            // host support an emulator cannot do much.
            abiFilters += listOf("arm64-v8a")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    sourceSets {
        getByName("main") {
            kotlin.srcDir("src/main/kotlin")
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }
}

kotlin {
    // Also sets the java toolchain AGP compiles with; the foojay resolver in
    // settings.gradle.kts downloads a JDK 17 if none is installed.
    jvmToolchain(17)
}

// Build the Rust side (../rust -> libtapview_android.so) into jniLibs before
// packaging. Requires cargo-ndk and the aarch64-linux-android target
// (see ../README.md). Always release: a debug egui build is unusably slow on
// a phone, and Rust-side debugging happens via logcat either way.
val cargoNdk = tasks.register<Exec>("cargoNdk") {
    workingDir = file("../rust")
    commandLine(
        "cargo", "ndk",
        "-t", "arm64-v8a",
        "-o", file("src/main/jniLibs").absolutePath,
        "build", "--release",
    )
}

tasks.named("preBuild") {
    dependsOn(cargoNdk)
}
